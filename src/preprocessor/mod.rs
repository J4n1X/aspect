//! Preprocessor — the stage between the raw lexer and the parser.
//!
//! Reads a source file, lexes it, and walks the resulting token stream
//! expanding `$<directive>` lines and define substitutions in place before
//! the parser ever sees them. Each directive family lives in its own
//! submodule ([`defines`], [`conditional`], [`modules`]) and is dispatched
//! by name from the driver's directive table; adding a new directive is a
//! new submodule plus one dispatch arm here.
//!
//! ## Line anchoring
//!
//! Directives are **line-anchored**: a directive is only recognised when
//! `$` is the first token on its line (leading whitespace is fine), and
//! everything up to the newline belongs to the directive. A `$` anywhere
//! else on a line is an error.
//!
//! ## Directive table
//!
//! The driver recognises every directive name (see [`DIRECTIVES`]);
//! unknown names get a did-you-mean suggestion computed against the
//! table. The families:
//!
//! - `$define NAME [tokens]` / `$undefine NAME` — define table and
//!   identifier substitution ([`defines`] module).
//! - `$ifdef`/`$ifndef`/`$if`/`$elseif`/`$elseifdef`/`$else`/`$endif` —
//!   conditional compilation ([`conditional`] module). The driver owns the
//!   chain stack; while a branch is skipped, ordinary tokens are dropped
//!   and non-conditional directives are inert.
//! - `$module <path>` / `$import <path>` — module identity, `-I` root
//!   resolution, and import-once loading ([`modules`] module).
//!
//! Directives are only meaningful at the top level of a file: the walk
//! tracks brace depth, and a line-leading `$` inside a block is an error.
//!
//! ## Why a separate stage
//!
//! - The lexer stays pure text-to-tokens; it doesn't touch the filesystem.
//! - The parser sees a single, already-expanded token stream — no special
//!   directive AST nodes, no recursive parser entry.
//! - Substitution over tokens is word-boundary-safe for free and can never
//!   rewrite string literals (they are single tokens).

pub mod conditional;
pub mod defines;
pub mod errors;
pub mod modules;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::lexer::{tokenize_with_file_id, Position, Token, TokenKind};
use crate::target::TargetSpec;

pub use defines::{Define, DefineOrigin, DefineTable};
pub use errors::PreprocessError;

/// The output of the preprocessor: the fully-expanded token stream plus the
/// file registry that maps each token's `pos.file_id` back to a source path.
/// Carrying the registry alongside the tokens lets the parser and the type
/// checker attribute errors to the *file the error came from*, not the entry
/// file — important once `$import` brings other files into the stream.
///
/// The module fields ([`PreprocessedSource::modules`],
/// [`PreprocessedSource::imports`], [`PreprocessedSource::search_roots`])
/// carry the data the import-visibility phase consumes. **The anonymous
/// root module** — every file without a `$module` declaration, typically
/// the entry file — **is represented as the empty string `""`** throughout.
#[derive(Debug, Clone)]
pub struct PreprocessedSource {
    pub tokens: Vec<Token>,
    pub files: Vec<PathBuf>,
    /// The module each loaded file belongs to: **exactly one entry per file
    /// in the registry, in `file_id` order** (`modules[i].0 == i`). Files
    /// that declared no `$module` map to the anonymous root module `""`.
    pub modules: Vec<(u32, String)>,
    /// Module → its **direct** imports only (no transitive closure),
    /// deduplicated, in first-import order. Imports made by any file of a
    /// module accrue to that module; imports by files without a `$module`
    /// declaration (e.g. the entry file) are recorded under `""`. Every
    /// module that appears in [`PreprocessedSource::modules`] has an entry,
    /// even if it imports nothing.
    pub imports: HashMap<String, Vec<String>>,
    /// The `-I` module search roots used, in flag order — kept for error
    /// reporting downstream.
    pub search_roots: Vec<PathBuf>,
}

/// Every directive name the driver recognises. Names missing from this
/// table produce an unknown-directive error with a did-you-mean suggestion.
const DIRECTIVES: &[&str] = &[
    "define",
    "undefine",
    "ifdef",
    "ifndef",
    "if",
    "elseif",
    "elseifdef",
    "else",
    "endif",
    "module",
    "import",
];

/// Tokenize a source file with a default-configured preprocessor (platform
/// defines only — no `-D` defines, no `-I` search roots). Convenience
/// wrapper over [`Preprocessor`] for callers that don't need CLI plumbing;
/// note that errors formatted from the returned [`PreprocessError`] lack
/// the `file:line:column` prefix [`Preprocessor::format_error`] provides.
///
/// # Errors
/// Any [`PreprocessError`] raised while lexing or expanding directives.
pub fn tokenize_file(entry: &Path) -> Result<PreprocessedSource, PreprocessError> {
    Preprocessor::new().preprocess(entry)
}

/// The preprocessor driver: owns the define table, the `-I` search roots,
/// and the expansion state (output tokens, import-once registry, file registry).
///
/// One-shot: create a fresh `Preprocessor` per entry file, seed it with
/// [`Preprocessor::add_cli_define`] / [`Preprocessor::add_include_dir`],
/// then call [`Preprocessor::preprocess`]. Keep it around after a failure —
/// [`Preprocessor::format_error`] needs the file registry to attribute the
/// error's position to the right file.
pub struct Preprocessor {
    /// Define table: platform defines at construction, `-D` defines next,
    /// then `$define`/`$undefine` directives as the stream is walked.
    defines: DefineTable,
    /// `-I` search roots for `$import`, in flag order.
    include_dirs: Vec<PathBuf>,
    /// Open `$if`/`$ifdef`/`$ifndef` chains. Gates the whole walk: while
    /// the top chain's branch is inactive, ordinary tokens are dropped and
    /// non-conditional directives are inert.
    conditionals: conditional::ConditionalStack,
    /// The fully-expanded output stream.
    tokens: Vec<Token>,
    /// Canonical path → file id; makes repeat inclusion a silent no-op.
    seen: HashMap<PathBuf, u32>,
    /// Indexed registry; `files[id]` is the canonical path of the file that
    /// produced any token whose position carries `file_id == id`.
    files: Vec<PathBuf>,
    /// `$module` declaration per file, parallel to `files` (`None` until —
    /// unless — the file declares one). `$import` verification reads it.
    file_modules: Vec<Option<String>>,
    /// Module → direct imports (deduplicated, first-import order). Each
    /// file's edges merge in when the file finishes processing.
    module_imports: HashMap<String, Vec<String>>,
    /// Import-once registry: module paths whose loading has *started* —
    /// marked before their files process, so import cycles terminate.
    imported: HashSet<String>,
    /// Per-file processing state, stacked because imports recurse: the top
    /// entry is the file whose tokens are being walked.
    file_stack: Vec<FileContext>,
}

/// Per-file state while that file's tokens are being walked. Tracks what
/// the `$module` placement rule and the imports map need to know.
pub(crate) struct FileContext {
    /// Index into the driver's `files` registry.
    pub(crate) file_id: u32,
    /// Where this file's `$module` directive appeared, once seen — drives
    /// the at-most-one rule's diagnostic.
    pub(crate) module_pos: Option<Position>,
    /// True once any non-directive, non-newline token has been emitted for
    /// this file; `$module` must come before that.
    pub(crate) saw_content: bool,
    /// Direct imports made by this file, in order, deduplicated. Merged
    /// into `module_imports` when the file finishes (its module identity is
    /// only final then — `$module` may follow an `$import`).
    pub(crate) imports: Vec<String>,
}

impl FileContext {
    fn new(file_id: u32) -> Self {
        Self {
            file_id,
            module_pos: None,
            saw_content: false,
            imports: Vec::new(),
        }
    }
}

impl Default for Preprocessor {
    fn default() -> Self {
        Self::new()
    }
}

impl Preprocessor {
    /// A driver seeded with the compiler-provided platform defines for the
    /// *host* target ([`TargetSpec::host`]). The ergonomic default for
    /// tests and any caller that doesn't need cross-target `$ifdef`
    /// behaviour — equivalent to `Preprocessor::for_target(&TargetSpec::host())`.
    #[must_use]
    pub fn new() -> Self {
        Self::for_target(&TargetSpec::host())
    }

    /// A driver seeded with the compiler-provided platform defines for an
    /// explicit compilation target — what `--target` wires up. Every
    /// subcommand that preprocesses goes through this (via `--target`'s
    /// host-defaulted value), so `$ifdef OS_*`/`$ifdef ARCH_*` always match
    /// the target being compiled for, not the host `aspc` happens to run on.
    #[must_use]
    pub fn for_target(target: &TargetSpec) -> Self {
        Self {
            defines: DefineTable::with_platform_defines(target),
            include_dirs: Vec::new(),
            conditionals: conditional::ConditionalStack::default(),
            tokens: Vec::new(),
            seen: HashMap::new(),
            files: Vec::new(),
            file_modules: Vec::new(),
            module_imports: HashMap::new(),
            imported: HashSet::new(),
            file_stack: Vec::new(),
        }
    }

    /// Register a `-D NAME` / `-D NAME=VALUE` define. Call before
    /// [`Preprocessor::preprocess`] — CLI defines count as prior defines
    /// for the redefinition rule.
    ///
    /// # Errors
    /// [`PreprocessError::InvalidCliDefine`] for malformed specs and
    /// [`PreprocessError::CliRedefinition`] for colliding ones.
    pub fn add_cli_define(&mut self, spec: &str) -> Result<(), PreprocessError> {
        self.defines.add_cli_define(spec)
    }

    /// Register a `-I` module search root (kept in flag order); `$import`
    /// resolution walks these.
    pub fn add_include_dir(&mut self, dir: impl Into<PathBuf>) {
        self.include_dirs.push(dir.into());
    }

    /// The registered `-I` search roots, in flag order.
    #[must_use]
    pub fn include_dirs(&self) -> &[PathBuf] {
        &self.include_dirs
    }

    /// The current define table (read-only; the driver owns mutation).
    #[must_use]
    pub fn defines(&self) -> &DefineTable {
        &self.defines
    }

    /// Tokenize `entry`, expanding directives recursively.
    ///
    /// # Errors
    /// Any [`PreprocessError`] from lexing, directive handling, or IO.
    pub fn preprocess(&mut self, entry: &Path) -> Result<PreprocessedSource, PreprocessError> {
        self.process_file(entry)?;
        // Per-file Eofs are dropped during the walk; cap with a single one.
        //
        // The closing EOF inherits the last real token's position rather than
        // inventing one. It is what the parser reports when input runs out
        // mid-construct ("Expected '}' but found 'EOF'"), and a synthetic
        // `Position::new(0, 0)` made every such diagnostic point at line 0,
        // column 0 of no file — a location that cannot exist and that no
        // editor can navigate to. The last token is the closest real source
        // location to where the input actually ended, so an unterminated block
        // now points at its final token. Only a completely empty translation
        // unit has no last token; it falls back to the start of the entry file.
        // (`process_file` registers the entry file first, so it is id 0.)
        let eof_pos = self
            .tokens
            .last()
            .map_or_else(|| Position::with_file(1, 1, 0), |t| t.pos);
        self.tokens
            .push(Token::new(TokenKind::Eof, eof_pos, String::new()));
        Ok(self.build_output())
    }

    /// Assemble the [`PreprocessedSource`] from the driver's state: the
    /// token stream is moved out, everything else is copied. Files without
    /// a `$module` declaration map to the anonymous root module `""`.
    fn build_output(&mut self) -> PreprocessedSource {
        let modules = self
            .file_modules
            .iter()
            .enumerate()
            .map(|(id, module)| {
                let id = u32::try_from(id).expect("file ids are bounded at allocation");
                (id, module.clone().unwrap_or_default())
            })
            .collect();
        PreprocessedSource {
            tokens: std::mem::take(&mut self.tokens),
            files: self.files.clone(),
            modules,
            imports: self.module_imports.clone(),
            search_roots: self.include_dirs.clone(),
        }
    }

    /// Format an error prefixed with the source file it came from (resolved
    /// via `pos.file_id` against the file registry) and its line/column —
    /// the same shape the parser and type checker produce.
    #[must_use]
    pub fn format_error(&self, err: &PreprocessError) -> String {
        let Some(pos) = err.position() else {
            return err.to_string();
        };
        match self.files.get(pos.file_id as usize) {
            Some(path) => format!("{}:{}:{}: {}", path.display(), pos.line, pos.column, err),
            None => err.to_string(),
        }
    }

    /// Lex one file and walk its tokens, returning its file id. Used as the
    /// entry point AND as the recursive descent step for `$import`. An
    /// already-seen canonical path is a silent no-op that returns the
    /// existing id (canonical-path dedup).
    pub(crate) fn process_file(&mut self, path: &Path) -> Result<u32, PreprocessError> {
        let canon = path
            .canonicalize()
            .map_err(|e| PreprocessError::UnresolvedPath {
                path: path.to_path_buf(),
                reason: e.to_string(),
            })?;
        // Allocate a fresh file id the first time we see this canonical
        // path; subsequent inclusions of the same file are silent no-ops.
        if let Some(&id) = self.seen.get(&canon) {
            return Ok(id);
        }
        let file_id = u32::try_from(self.files.len())
            .expect("preprocessor source files exceed u32::MAX");
        self.seen.insert(canon.clone(), file_id);
        self.files.push(canon.clone());
        self.file_modules.push(None);

        let source = fs::read_to_string(&canon).map_err(|e| PreprocessError::Unreadable {
            path: canon.clone(),
            reason: e.to_string(),
        })?;
        let raw = tokenize_with_file_id(source, file_id)?;
        self.file_stack.push(FileContext::new(file_id));
        self.process_tokens(&raw)?;
        self.finish_file();
        Ok(file_id)
    }

    /// Close out the current file: pop its context and merge its direct
    /// imports into the imports map under the file's (now final) module —
    /// the anonymous root module `""` when it declared none. Ensures every
    /// processed file's module has an entry, even an empty one.
    fn finish_file(&mut self) {
        let ctx = self
            .file_stack
            .pop()
            .expect("finish_file is only called for a pushed file context");
        let module = self.file_modules[ctx.file_id as usize]
            .clone()
            .unwrap_or_default();
        let edges = self.module_imports.entry(module).or_default();
        for import in ctx.imports {
            if !edges.contains(&import) {
                edges.push(import);
            }
        }
    }

    /// Walk one file's raw token stream: dispatch line-anchored directives,
    /// substitute defines into everything else. While a conditional branch
    /// is skipped, ordinary tokens (newlines included) are discarded and
    /// only the conditional directives themselves have any effect.
    ///
    /// Conditional chains are per-file: the stack depth at entry is the
    /// baseline this file must return to — anything still open at its end
    /// is unterminated (and `conditional.rs` refuses to close a frame
    /// another file opened).
    fn process_tokens(&mut self, raw: &[Token]) -> Result<(), PreprocessError> {
        let cond_baseline = self.conditionals.depth();
        // Raw-source brace depth for top-level directive enforcement. Only
        // active tokens count: skipped content is discarded wholesale, and
        // a skip can only start at depth 0 (the opening directive would
        // itself have errored otherwise), so the depth stays honest.
        let mut brace_depth = 0usize;
        let mut i = 0;
        let mut at_line_start = true;
        while i < raw.len() {
            let token = &raw[i];
            match &token.kind {
                TokenKind::Eof => break,
                TokenKind::Dollar => {
                    if !at_line_start {
                        if self.conditionals.active() {
                            return Err(PreprocessError::MidLineDirective(token.pos));
                        }
                        // Skipped-branch content is discarded, a mid-line
                        // `$` included.
                        i += 1;
                        continue;
                    }
                    if brace_depth > 0 {
                        return Err(PreprocessError::DirectiveInsideBlock(token.pos));
                    }
                    // Everything up to the newline belongs to the directive.
                    let line_len = raw[i..]
                        .iter()
                        .position(|t| matches!(t.kind, TokenKind::Newline | TokenKind::Eof))
                        .unwrap_or(raw.len() - i);
                    self.process_directive_line(&raw[i..i + line_len])?;
                    i += line_len;
                    // Consume the terminating newline too (nothing of the
                    // directive line reaches the output stream).
                    if matches!(raw.get(i).map(|t| &t.kind), Some(TokenKind::Newline)) {
                        i += 1;
                    }
                }
                TokenKind::Newline => {
                    if self.conditionals.active() {
                        self.tokens.push(token.clone());
                    }
                    at_line_start = true;
                    i += 1;
                }
                _ => {
                    if self.conditionals.active() {
                        match token.kind {
                            TokenKind::OpenBrace => brace_depth += 1,
                            TokenKind::CloseBrace => brace_depth = brace_depth.saturating_sub(1),
                            _ => {}
                        }
                        defines::expand_into(&mut self.tokens, token, &self.defines);
                        // The file now has non-directive content — `$module`
                        // can no longer appear (newlines don't count). Only
                        // *emitted* tokens count: content inside a skipped
                        // conditional branch does not block a later `$module`.
                        if let Some(ctx) = self.file_stack.last_mut() {
                            ctx.saw_content = true;
                        }
                    }
                    at_line_start = false;
                    i += 1;
                }
            }
        }
        if let Some((directive, pos)) = self.conditionals.unterminated_since(cond_baseline) {
            return Err(PreprocessError::UnterminatedConditional { directive, pos });
        }
        Ok(())
    }

    /// Dispatch one directive line (`line[0]` is the `$`, the newline is
    /// already stripped) to its handler. Each arm is a small function call
    /// into the directive family's submodule.
    ///
    /// Conditional directives are always processed — they steer the skip
    /// state and keep nesting matched inside skipped branches. Every other
    /// directive is inert while a branch is skipped: `$define` does not
    /// define, `$import` does not resolve, unknown names do not error.
    fn process_directive_line(&mut self, line: &[Token]) -> Result<(), PreprocessError> {
        let pos = line[0].pos;
        // `$if` / `$else` lex as keywords, not identifiers — take the name
        // from either kind so the whole directive table is reachable.
        let name = match line.get(1).map(|t| &t.kind) {
            Some(TokenKind::Identifier(n)) => n.clone(),
            Some(TokenKind::Keyword(k)) => k.to_string(),
            _ if !self.conditionals.active() => return Ok(()),
            _ => return Err(PreprocessError::MissingDirectiveName(pos)),
        };
        let rest = &line[2..];
        if matches!(
            name.as_str(),
            "ifdef" | "ifndef" | "if" | "elseif" | "elseifdef" | "else" | "endif"
        ) {
            return self.conditionals.handle(&name, rest, pos, &self.defines);
        }
        if !self.conditionals.active() {
            return Ok(());
        }
        match name.as_str() {
            "define" => self.handle_define(rest, pos),
            "undefine" => self.handle_undefine(rest, pos),
            "module" => modules::handle_module(self, rest, pos),
            "import" => modules::handle_import(self, rest, pos),
            _ => Err(PreprocessError::UnknownDirective {
                suggestion: suggest_directive(&name),
                name,
                pos,
            }),
        }
    }

    /// `$define NAME [tokens]` — the rest of the line is the (possibly
    /// empty) replacement token sequence, stored unexpanded; substitution
    /// happens at use sites.
    fn handle_define(&mut self, rest: &[Token], pos: Position) -> Result<(), PreprocessError> {
        let Some((name_token, value)) = rest.split_first() else {
            return Err(PreprocessError::ExpectedName {
                directive: "define",
                pos,
            });
        };
        let TokenKind::Identifier(name) = &name_token.kind else {
            return Err(PreprocessError::ExpectedName {
                directive: "define",
                pos: name_token.pos,
            });
        };
        self.defines.define(
            name.clone(),
            value.to_vec(),
            DefineOrigin::Directive(name_token.pos),
        )
    }

    /// `$undefine NAME` — removes the define; a no-op if it isn't defined.
    fn handle_undefine(&mut self, rest: &[Token], pos: Position) -> Result<(), PreprocessError> {
        let Some(name_token) = rest.first() else {
            return Err(PreprocessError::ExpectedName {
                directive: "undefine",
                pos,
            });
        };
        let TokenKind::Identifier(name) = &name_token.kind else {
            return Err(PreprocessError::ExpectedName {
                directive: "undefine",
                pos: name_token.pos,
            });
        };
        if let Some(extra) = rest.get(1) {
            return Err(PreprocessError::TrailingTokens {
                directive: "undefine",
                pos: extra.pos,
            });
        }
        self.defines.undefine(name);
        Ok(())
    }

}

/// The nearest directive-table entry within a length-scaled edit distance
/// (⌈len/3⌉, rustc-style), for unknown-directive did-you-mean suggestions.
fn suggest_directive(name: &str) -> Option<String> {
    let max_distance = name.len().div_ceil(3);
    DIRECTIVES
        .iter()
        .map(|candidate| (levenshtein(name, candidate), *candidate))
        .filter(|(distance, _)| *distance <= max_distance)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate.to_string())
}

/// Classic two-row Levenshtein edit distance.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut row = Vec::with_capacity(b.len() + 1);
        row.push(i + 1);
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            row.push((prev[j] + cost).min(prev[j + 1] + 1).min(row[j] + 1));
        }
        prev = row;
    }
    prev[b.len()]
}

/// Run the driver over an in-memory source string (registered as file id 0)
/// — lets unit tests exercise the walk without touching the filesystem.
#[cfg(test)]
pub(crate) fn preprocess_str(source: &str) -> Result<Vec<Token>, PreprocessError> {
    preprocess_str_with(Preprocessor::new(), source)
}

/// [`preprocess_str`] with a pre-seeded driver (e.g. `-D` defines applied).
#[cfg(test)]
pub(crate) fn preprocess_str_with(
    pp: Preprocessor,
    source: &str,
) -> Result<Vec<Token>, PreprocessError> {
    Ok(preprocess_str_full(pp, source)?.tokens)
}

/// Full-output variant of [`preprocess_str_with`] for tests that inspect
/// the module registry alongside the tokens. The in-memory source is
/// registered as file id 0 with a synthetic file context, mirroring what
/// [`Preprocessor::process_file`] sets up for real files. No trailing EOF
/// token is appended (matching the historical `preprocess_str` shape).
#[cfg(test)]
pub(crate) fn preprocess_str_full(
    mut pp: Preprocessor,
    source: &str,
) -> Result<PreprocessedSource, PreprocessError> {
    pp.files.push(PathBuf::from("<test>"));
    pp.file_modules.push(None);
    pp.file_stack.push(FileContext::new(0));
    let raw = tokenize_with_file_id(source.to_string(), 0)?;
    pp.process_tokens(&raw)?;
    pp.finish_file();
    Ok(pp.build_output())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midline_dollar_is_an_error() {
        let err = preprocess_str("i32 x = 0 $define MAX 1\n").unwrap_err();
        let PreprocessError::MidLineDirective(pos) = err else {
            panic!("expected MidLineDirective, got {err:?}");
        };
        assert_eq!((pos.line, pos.column), (1, 11));
    }

    #[test]
    fn leading_whitespace_keeps_a_directive_line_anchored() {
        let tokens = preprocess_str("    $define MAX 1\nMAX\n").unwrap();
        assert!(tokens
            .iter()
            .any(|t| matches!(t.kind, TokenKind::Integer(1))));
    }

    #[test]
    fn directive_after_statement_line_is_recognised() {
        let tokens = preprocess_str("i32 x = 0\n$define MAX 1\nMAX\n").unwrap();
        assert!(tokens
            .iter()
            .any(|t| matches!(t.kind, TokenKind::Integer(1))));
    }

    #[test]
    fn dollar_without_a_name_is_an_error() {
        let err = preprocess_str("$\n").unwrap_err();
        assert!(matches!(err, PreprocessError::MissingDirectiveName(_)));
    }

    #[test]
    fn unknown_directive_suggests_the_nearest_name() {
        let err = preprocess_str("$defien MAX 1\n").unwrap_err();
        let PreprocessError::UnknownDirective {
            name, suggestion, ..
        } = &err
        else {
            panic!("expected UnknownDirective, got {err:?}");
        };
        assert_eq!(name, "defien");
        assert_eq!(suggestion.as_deref(), Some("define"));
        assert!(err.to_string().contains("did you mean `$define`?"));
    }

    #[test]
    fn unknown_directive_far_from_the_table_has_no_suggestion() {
        let err = preprocess_str("$frobnicate\n").unwrap_err();
        let PreprocessError::UnknownDirective { suggestion, .. } = &err else {
            panic!("expected UnknownDirective, got {err:?}");
        };
        assert!(suggestion.is_none());
        assert!(!err.to_string().contains("did you mean"));
    }

    #[test]
    fn conditional_directives_reach_their_handlers() {
        // `$if` and `$else` lex as keywords — the directive-name fallback
        // must still reach the table for them. Behaviour is covered in
        // `conditional.rs`; this only pins the driver routing.
        let tokens = preprocess_str("$if 1\n7\n$else\n8\n$endif\n").unwrap();
        assert!(tokens
            .iter()
            .any(|t| matches!(t.kind, TokenKind::Integer(7))));
        assert!(!tokens
            .iter()
            .any(|t| matches!(t.kind, TokenKind::Integer(8))));
        let tokens = preprocess_str("$ifdef NOPE\n1\n$elseifdef ALSO_NOPE\n2\n$endif\n").unwrap();
        assert!(!tokens.iter().any(|t| matches!(t.kind, TokenKind::Integer(_))));
    }

    #[test]
    fn define_without_a_name_is_an_error() {
        let err = preprocess_str("$define\n").unwrap_err();
        assert!(matches!(
            err,
            PreprocessError::ExpectedName {
                directive: "define",
                ..
            }
        ));
    }
}
