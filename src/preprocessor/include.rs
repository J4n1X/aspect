//! `$include "path"` — splice another source file's tokens in.
//!
//! Paths in the directive are resolved relative to the *containing file's*
//! directory (mirroring most C-family preprocessors). The expansion is
//! delegated back to [`super::process_file`], so included files can in
//! turn `$include` more files — the shared [`ExpansionState`] keeps
//! mutually-referential headers from looping (canonical-path include-once).

use std::path::Path;

use crate::lexer::{LexerError, Token, TokenKind};

use super::{process_file, ExpansionState};

/// Process a `$include "path"` directive whose `$` is `tokens[0]`.
/// Returns the number of tokens (including a possible trailing newline /
/// semicolon) the caller should advance past.
pub(crate) fn process(
    tokens: &[Token],
    state: &mut ExpansionState,
    parent_dir: &Path,
) -> Result<usize, LexerError> {
    let pos = tokens[0].pos;
    let rel_path = match tokens.get(2).map(|t| &t.kind) {
        Some(TokenKind::StringLiteral(s)) => s.clone(),
        _ => {
            return Err(LexerError::IncludeError(format!(
                "`$include` expects a string-literal path at {pos}"
            )));
        }
    };
    let mut consumed = 3;
    // Eat a trailing terminator so the directive doesn't leave a stray
    // newline/semicolon for the parser to stumble on.
    if matches!(
        tokens.get(consumed).map(|t| &t.kind),
        Some(TokenKind::Newline | TokenKind::Semicolon)
    ) {
        consumed += 1;
    }

    let resolved = parent_dir.join(&rel_path);
    process_file(&resolved, state)?;
    Ok(consumed)
}
