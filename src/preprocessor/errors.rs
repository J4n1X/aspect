use crate::lexer::{LexerError, Position};
use aspect_macros::ErrorPosition;
use std::path::PathBuf;
use thiserror::Error;

/// Preprocessor error types.
///
/// Every variant that originates at a token carries a [`Position`] whose
/// `file_id` indexes the preprocessor's file registry, so
/// [`super::Preprocessor::format_error`] can prefix the message with
/// `file:line:column` exactly like the parser and type checker do.
/// Registry-less variants (CLI `-D` problems, IO failures where no token
/// exists yet) format without the prefix.
#[derive(Error, Debug, ErrorPosition)]
pub enum PreprocessError {
    // `#[position]` delegates to `LexerError::position()`, preserving the
    // hand-written arm that reached into the wrapped lexer error's location.
    #[error("{0}")]
    Lexer(#[position] #[from] LexerError),

    #[error("cannot resolve path '{path}': {reason}")]
    UnresolvedPath { path: PathBuf, reason: String },

    #[error("cannot read '{path}': {reason}")]
    Unreadable { path: PathBuf, reason: String },

    #[error("preprocessor directives must start a line: `$` at {0}")]
    MidLineDirective(Position),

    #[error("expected a directive name after `$` at {0}")]
    MissingDirectiveName(Position),

    #[error("unknown preprocessor directive `${name}` at {pos}{}", .suggestion.as_deref().map(|s| format!("; did you mean `${s}`?")).unwrap_or_default())]
    UnknownDirective {
        name: String,
        suggestion: Option<String>,
        pos: Position,
    },

    #[error("expected a name after `${directive}` at {pos}")]
    ExpectedName {
        directive: &'static str,
        pos: Position,
    },

    #[error("unexpected tokens after `${directive}` at {pos}")]
    TrailingTokens {
        directive: &'static str,
        pos: Position,
    },

    #[error("redefinition of `{name}` at {pos} ({previous}); `$undefine` it first")]
    Redefinition {
        name: String,
        previous: String,
        pos: Position,
    },

    #[error("stray `${directive}` at {pos} with no open `$if`/`$ifdef`/`$ifndef` chain")]
    StrayConditional {
        directive: &'static str,
        pos: Position,
    },

    #[error("`${directive}` at {pos} cannot follow the chain's `$else` (at {else_pos})")]
    ConditionalAfterElse {
        directive: &'static str,
        else_pos: Position,
        pos: Position,
    },

    #[error("unterminated `${directive}` opened at {pos}: missing `$endif`")]
    UnterminatedConditional {
        directive: &'static str,
        pos: Position,
    },

    #[error("`$` at {0} is inside a block; only conditional directives (`$if`/`$ifdef`/`$else`/`$endif`/…) may appear inside a block — `$define`/`$undefine`/`$module`/`$import` must be at the top level")]
    DirectiveInsideBlock(Position),

    #[error("undefined identifier `{name}` in `$if` expression at {pos}; use `defined({name})` to test whether it is defined")]
    UndefinedInIfExpr { name: String, pos: Position },

    #[error("division by zero in `$if` expression at {pos}")]
    IfDivisionByZero { pos: Position },

    #[error("malformed `$if` expression at {pos}: {detail}")]
    MalformedIfExpr { detail: String, pos: Position },

    #[error("`-D {name}` is a redefinition ({previous})")]
    CliRedefinition { name: String, previous: String },

    #[error("invalid `-D` define '{spec}': {reason}")]
    InvalidCliDefine { spec: String, reason: String },

    #[error("malformed `${directive}` path at {pos}: {detail} (module paths are bare `segment/segment/...` identifiers)")]
    MalformedModulePath {
        directive: &'static str,
        detail: String,
        pos: Position,
    },

    #[error("second `$module` directive at {pos}; this file already declared its module at {previous}")]
    DuplicateModuleDirective { pos: Position, previous: Position },

    #[error("`$module` at {0} must appear before any non-directive content in the file")]
    ModuleAfterContent(Position),

    #[error("import `{module}` at {pos} is ambiguous: `{}` (file form) and `{}` (directory form) both exist; a module must be one or the other", .file.display(), .dir.display())]
    AmbiguousModuleForms {
        module: String,
        file: PathBuf,
        dir: PathBuf,
        pos: Position,
    },

    #[error("cannot resolve import `{module}` at {pos}{}", format_candidates(.candidates))]
    ModuleNotFound {
        module: String,
        candidates: Vec<PathBuf>,
        pos: Position,
    },

    #[error("import `{module}` at {pos} loaded '{}', which {} — `$module` declarations are authoritative and must match the import path", .file.display(), format_declaration(.declared))]
    ModuleDeclarationMismatch {
        /// The path the `$import` asked for.
        module: String,
        /// The file the resolution loaded for it.
        file: PathBuf,
        /// What that file declared — `None` when it has no `$module` at all.
        declared: Option<String>,
        pos: Position,
    },
}

/// Render the candidate list of a [`PreprocessError::ModuleNotFound`]: every
/// path resolution tried, or a hint that no `-I` roots were given at all.
fn format_candidates(candidates: &[PathBuf]) -> String {
    if candidates.is_empty() {
        return "; no module search roots are registered (pass `-I <dir>`)".to_string();
    }
    let mut out = "; tried:".to_string();
    for candidate in candidates {
        out.push_str("\n  ");
        out.push_str(&candidate.display().to_string());
    }
    out
}

/// Render the declared-module half of a
/// [`PreprocessError::ModuleDeclarationMismatch`] message.
fn format_declaration(declared: &Option<String>) -> String {
    match declared {
        Some(module) => format!("declares module `{module}`"),
        None => "declares no `$module`".to_string(),
    }
}
