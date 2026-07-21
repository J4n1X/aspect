//! Preprocessor — the stage between the raw lexer and the parser.
//!
//! Walks the lexer's token stream, expanding `$<directive>` lines and define
//! substitutions in place before the parser sees them. Each directive family
//! lives in its own submodule ([`defines`], [`conditional`], [`modules`]),
//! dispatched by name from the driver's directive table.
//!
//! Directives are **line-anchored**: `$` is a directive only as the first
//! token on its line (leading whitespace aside), and the rest of the line
//! belongs to it; a `$` anywhere else is an error.
//!
//! Conditional directives (`$if*`/`$else`/`$endif`) work anywhere, including
//! inside a function body — they only gate which tokens reach the parser. The
//! rest establish module-level state (`$define`/`$undefine`) or file structure
//! (`$module`/`$import`) and are top-level only: a line-leading `$` for one of
//! those inside a block is an error.
//!
//! Kept a separate stage so the lexer stays pure text-to-tokens (no
//! filesystem) and the parser sees one already-expanded stream — no directive
//! AST nodes, no recursive parser entry. Substitution over tokens can never
//! rewrite string literals, which are single tokens.

pub mod conditional;
pub mod defines;
pub mod errors;
pub mod expr_eval;
pub mod modules;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::lexer::{tokenize_with_file_id, Position, Token, TokenKind};
use crate::target::TargetSpec;

pub use defines::{Define, DefineOrigin, DefineTable};
pub use errors::PreprocessError;

use defines::ScopedDefines;

/// The fully-expanded token stream plus the file registry mapping each token's
/// `pos.file_id` back to a source path — carried together so the parser and
/// type checker attribute errors to the *file the error came from*, not the
/// entry file, once `$import` pulls other files into the stream.
///
/// **The anonymous root module** — every file without a `$module` declaration,
/// typically the entry file — **is the empty string `""`** throughout.
#[derive(Debug, Clone)]
pub struct PreprocessedSource {
    pub tokens: Vec<Token>,
    pub files: Vec<PathBuf>,
    /// The module each loaded file belongs to: **exactly one entry per file
    /// in the registry, in `file_id` order** (`modules[i].0 == i`).
    pub modules: Vec<(u32, String)>,
    /// Module → its **direct** imports only (no transitive closure),
    /// deduplicated, in first-import order. Every module in `modules` has an
    /// entry, even if it imports nothing.
    pub imports: HashMap<String, Vec<String>>,
    /// The `-I` module search roots used, in flag order — kept for downstream
    /// error reporting.
    pub search_roots: Vec<PathBuf>,
}

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

/// Convenience wrapper over [`Preprocessor`] with only platform defines (no
/// `-D`, no `-I`). Errors from the returned [`PreprocessError`] lack the
/// `file:line:column` prefix that [`Preprocessor::format_error`] adds.
///
/// # Errors
/// Any [`PreprocessError`] raised while lexing or expanding directives.
pub fn tokenize_file(entry: &Path) -> Result<PreprocessedSource, PreprocessError> {
    Preprocessor::new().preprocess(entry)
}

/// The preprocessor driver. One-shot: create a fresh `Preprocessor` per entry
/// file, seed it with [`Preprocessor::add_cli_define`] /
/// [`Preprocessor::add_include_dir`], then call [`Preprocessor::preprocess`].
/// Keep it around after a failure — [`Preprocessor::format_error`] needs the
/// file registry to attribute the error to the right file.
pub struct Preprocessor {
    /// Platform defines at construction, `-D` defines next, then
    /// `$define`/`$undefine` directives as the stream is walked.
    defines: DefineTable,
    include_dirs: Vec<PathBuf>,
    /// Gates the whole walk: while the top chain's branch is inactive,
    /// ordinary tokens are dropped and non-conditional directives are inert.
    conditionals: conditional::ConditionalStack,
    tokens: Vec<Token>,
    /// Canonical path → file id; makes repeat inclusion a silent no-op.
    seen: HashMap<PathBuf, u32>,
    /// `files[id]` is the canonical path of the file that produced any token
    /// whose position carries `file_id == id`.
    files: Vec<PathBuf>,
    /// `$module` declaration per file, parallel to `files` (`None` unless the
    /// file declares one).
    file_modules: Vec<Option<String>>,
    module_imports: HashMap<String, Vec<String>>,
    /// Module paths whose loading has *started* — marked before their files
    /// process, so import cycles terminate.
    imported: HashSet<String>,
    /// Stacked because imports recurse: the top entry is the file whose
    /// tokens are being walked.
    file_stack: Vec<FileContext>,
}

pub(crate) struct FileContext {
    pub(crate) file_id: u32,
    /// Where this file's `$module` directive appeared, once seen — drives the
    /// at-most-one rule's diagnostic.
    pub(crate) module_pos: Option<Position>,
    /// True once any non-directive, non-newline token has been emitted;
    /// `$module` must come before that.
    pub(crate) saw_content: bool,
    /// Merged into `module_imports` when the file finishes, since its module
    /// identity is only final then — `$module` may follow an `$import`.
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
    /// Seeded with platform defines for the *host* target — equivalent to
    /// `Preprocessor::for_target(&TargetSpec::host())`.
    #[must_use]
    pub fn new() -> Self {
        Self::for_target(&TargetSpec::host())
    }

    /// Seeded with platform defines for an explicit target — what `--target`
    /// wires up, so `$ifdef OS_*`/`$ifdef ARCH_*` match the target being
    /// compiled for, not the host `aspc` runs on.
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

    /// Call before [`Preprocessor::preprocess`] — CLI defines count as prior
    /// defines for the redefinition rule.
    ///
    /// # Errors
    /// [`PreprocessError::InvalidCliDefine`] for malformed specs and
    /// [`PreprocessError::CliRedefinition`] for colliding ones.
    pub fn add_cli_define(&mut self, spec: &str) -> Result<(), PreprocessError> {
        self.defines.add_cli_define(spec)
    }

    pub fn add_include_dir(&mut self, dir: impl Into<PathBuf>) {
        self.include_dirs.push(dir.into());
    }

    #[must_use]
    pub fn include_dirs(&self) -> &[PathBuf] {
        &self.include_dirs
    }

    #[must_use]
    pub fn defines(&self) -> &DefineTable {
        &self.defines
    }

    /// # Errors
    /// Any [`PreprocessError`] from lexing, directive handling, or IO.
    pub fn preprocess(&mut self, entry: &Path) -> Result<PreprocessedSource, PreprocessError> {
        self.process_file(entry)?;
        // The closing EOF inherits the last real token's position: it is what
        // the parser reports when input runs out mid-construct ("Expected '}'
        // but found 'EOF'"), and line 0:0 of no file is a location no editor
        // can navigate to. A completely empty unit falls back to the entry file.
        let eof_pos = self
            .tokens
            .last()
            .map_or_else(|| Position::with_file(1, 1, 0), |t| t.pos);
        self.tokens
            .push(Token::new(TokenKind::Eof, eof_pos, String::new()));
        Ok(self.build_output())
    }

    /// Files without a `$module` declaration map to the anonymous root module
    /// `""`.
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

    /// Prefixes the error with `file:line:column` (resolved via `pos.file_id`
    /// against the file registry) — the shape the parser and checker produce.
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

    /// The entry point AND the recursive descent step for `$import`. An
    /// already-seen canonical path is a silent no-op returning the existing id.
    pub(crate) fn process_file(&mut self, path: &Path) -> Result<u32, PreprocessError> {
        let canon = path
            .canonicalize()
            .map_err(|e| PreprocessError::UnresolvedPath {
                path: path.to_path_buf(),
                reason: e.to_string(),
            })?;
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

    /// Merges the file's direct imports into the imports map under its now-final
    /// module — `""` when it declared none — ensuring every processed file's
    /// module has an entry, even an empty one.
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

    /// Dispatch line-anchored directives, substitute defines into everything
    /// else. While a conditional branch is skipped, ordinary tokens (newlines
    /// included) are discarded and only conditional directives have effect.
    ///
    /// Conditional chains are per-file: the stack depth at entry is the
    /// baseline this file must return to — anything still open at its end is
    /// unterminated.
    fn process_tokens(&mut self, raw: &[Token]) -> Result<(), PreprocessError> {
        let cond_baseline = self.conditionals.depth();
        // Only active tokens count toward brace depth: a skip can only start at
        // depth 0 (the opening directive would have errored otherwise), so
        // discarding skipped content wholesale keeps the depth honest.
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
                        i += 1;
                        continue;
                    }
                    let line_len = raw[i..]
                        .iter()
                        .position(|t| matches!(t.kind, TokenKind::Newline | TokenKind::Eof))
                        .unwrap_or(raw.len() - i);
                    self.process_directive_line(&raw[i..i + line_len], brace_depth)?;
                    i += line_len;
                    // Consume the terminating newline; nothing of the directive
                    // line reaches the output.
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
                        {
                            let ctx = self.file_stack.last().expect(
                                "a file context is always active while tokens are processed",
                            );
                            let module = self.file_modules[ctx.file_id as usize]
                                .as_deref()
                                .unwrap_or("");
                            let scoped = ScopedDefines::new(
                                &self.defines,
                                module,
                                &ctx.imports,
                                &self.file_modules,
                            );
                            scoped.expand_into(&mut self.tokens, token);
                        }
                        // Only *emitted* tokens block a later `$module`:
                        // content inside a skipped conditional branch does not.
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

    /// `line[0]` is the `$`, the newline already stripped.
    ///
    /// Conditional directives are always processed — they steer the skip state
    /// and keep nesting matched inside skipped branches. Every other directive
    /// is inert while a branch is skipped: `$define` does not define, `$import`
    /// does not resolve, unknown names do not error. Those directives also
    /// mutate module-level state or file structure, so they error at
    /// `brace_depth > 0`; conditionals are valid at any depth.
    fn process_directive_line(
        &mut self,
        line: &[Token],
        brace_depth: usize,
    ) -> Result<(), PreprocessError> {
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
            // `$if`/`$ifdef` see only the current file's own module and its
            // direct imports (plus globals) — the same scope as substitution.
            let ctx = self
                .file_stack
                .last()
                .expect("a file context is always active while tokens are processed");
            let module = self.file_modules[ctx.file_id as usize]
                .as_deref()
                .unwrap_or("");
            let scoped =
                ScopedDefines::new(&self.defines, module, &ctx.imports, &self.file_modules);
            return self.conditionals.handle(&name, rest, pos, &scoped);
        }
        if !self.conditionals.active() {
            return Ok(());
        }
        // Everything below mutates module-level state or injects file
        // content — none of it belongs inside a block.
        if brace_depth > 0 {
            return Err(PreprocessError::DirectiveInsideBlock(pos));
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

    /// The replacement tokens are stored unexpanded; substitution happens at
    /// use sites.
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
        // The origin's `file_id` (this file) tags the define with its home
        // module; `file_modules` resolves that id for the collision check.
        self.defines.define(
            name.clone(),
            value.to_vec(),
            DefineOrigin::Directive(name_token.pos),
            &self.file_modules,
        )
    }

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
        let this_file = self
            .file_stack
            .last()
            .expect("a file context is always active while tokens are processed")
            .file_id;
        self.defines.undefine(name, this_file, &self.file_modules);
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

/// Full-output variant of `preprocess_str_with` for tests that inspect the
/// module registry. No trailing EOF token is appended.
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
