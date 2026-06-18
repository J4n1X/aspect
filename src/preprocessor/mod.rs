//! Preprocessor — the stage between the raw lexer and the parser.
//!
//! Reads a source file, lexes it, and walks the resulting token stream
//! expanding any `$<directive>` forms in place before the parser ever sees
//! them. Each directive lives in its own submodule (e.g. [`include`]) and
//! is dispatched by name from [`process_file`]; adding a new directive is a
//! new submodule plus one match arm here.
//!
//! Currently implemented directives:
//! - `$include "path"` — splice another source file's tokens in
//!   ([`include`] module).
//!
//! ## Why a separate stage
//!
//! - The lexer stays pure text-to-tokens; it doesn't touch the filesystem.
//! - The parser sees a single, already-expanded token stream — no special
//!   `$include` AST node, no recursive parser entry.
//! - Future directives that DON'T involve files (e.g. `$define` macros,
//!   `$ifdef` conditionals) can be added without touching the lexer.
//!
//! ## Limitations (intentional, for the first cut)
//!
//! - Directives are recognised wherever they appear in the token stream,
//!   including inside function bodies. Top-level use is the only sensible
//!   placement but it isn't syntactically enforced.
//! - Tokens from included files keep their own line/column, but errors at
//!   the parser/typechecker stage print the *host* file's name (positions
//!   don't yet carry a per-file id). Documented as a follow-up.

pub mod include;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::lexer::{tokenize_with_file_id, LexerError, Position, Token, TokenKind};

/// The output of the preprocessor: the fully-expanded token stream plus the
/// file registry that maps each token's `pos.file_id` back to a source path.
/// Carrying the registry alongside the tokens lets the parser and the type
/// checker attribute errors to the *file the error came from*, not the entry
/// file — important once `$include` brings other files into the stream.
#[derive(Debug, Clone)]
pub struct PreprocessedSource {
    pub tokens: Vec<Token>,
    pub files: Vec<PathBuf>,
}

/// Tokenize a source file, expanding any `$<directive>` forms recursively.
/// Include-once semantics: a file canonicalised once never expands twice.
///
/// # Errors
/// Propagates any [`LexerError`] from the raw lexer plus
/// [`LexerError::IncludeError`] for malformed directives, missing files,
/// and other IO failures.
pub fn tokenize_file(entry: &Path) -> Result<PreprocessedSource, LexerError> {
    let mut state = ExpansionState::default();
    process_file(entry, &mut state)?;
    // Per-file Eofs are dropped during the walk; cap with a single one. The
    // closing EOF has no real source location — fileless `Position::new`.
    state.tokens.push(Token::new(
        TokenKind::Eof,
        Position::new(0, 0),
        String::new(),
    ));
    Ok(PreprocessedSource {
        tokens: state.tokens,
        files: state.files,
    })
}

/// Shared state threaded through the recursive expansion. Public to
/// directive submodules so each can append tokens and recurse into more
/// files without re-establishing the include-once set or file registry.
#[derive(Default)]
pub(crate) struct ExpansionState {
    pub(crate) tokens: Vec<Token>,
    /// Canonical path → file id. The id matches the index into `files`.
    pub(crate) seen: HashMap<PathBuf, u32>,
    /// Indexed registry; `files[id]` is the canonical path of the file that
    /// produced any token whose position carries `file_id == id`.
    pub(crate) files: Vec<PathBuf>,
}

/// Lex one file and walk its tokens, dispatching each `$<directive>` to
/// its handler. Used as the entry point AND as the recursive descent step
/// for `$include`.
pub(crate) fn process_file(
    path: &Path,
    state: &mut ExpansionState,
) -> Result<(), LexerError> {
    let canon = path.canonicalize().map_err(|e| {
        LexerError::IncludeError(format!(
            "cannot resolve include path '{}': {e}",
            path.display()
        ))
    })?;
    // Allocate a fresh file id the first time we see this canonical path;
    // subsequent inclusions of the same file are silent no-ops.
    if state.seen.contains_key(&canon) {
        return Ok(());
    }
    let file_id = u32::try_from(state.files.len())
        .expect("preprocessor source files exceed u32::MAX");
    state.seen.insert(canon.clone(), file_id);
    state.files.push(canon.clone());

    let source = fs::read_to_string(&canon).map_err(|e| {
        LexerError::IncludeError(format!("cannot read '{}': {e}", canon.display()))
    })?;
    let raw = tokenize_with_file_id(source, file_id)?;
    let parent = canon
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();

    let mut i = 0;
    while i < raw.len() {
        match &raw[i].kind {
            TokenKind::Eof => break,
            TokenKind::Dollar => {
                let pos = raw[i].pos;
                let name = directive_name(&raw[i..], pos)?;
                let consumed = match name.as_str() {
                    "include" => include::process(&raw[i..], state, &parent)?,
                    other => {
                        return Err(LexerError::IncludeError(format!(
                            "unknown preprocessor directive `${other}` at {pos}"
                        )));
                    }
                };
                i += consumed;
            }
            _ => {
                state.tokens.push(raw[i].clone());
                i += 1;
            }
        }
    }
    Ok(())
}

/// Read the directive name (`include`, `define`, …) following the `$`.
fn directive_name(tokens: &[Token], pos: Position) -> Result<String, LexerError> {
    match tokens.get(1).map(|t| &t.kind) {
        Some(TokenKind::Identifier(n)) => Ok(n.clone()),
        _ => Err(LexerError::IncludeError(format!(
            "expected a directive name after `$` at {pos}"
        ))),
    }
}
