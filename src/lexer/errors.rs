use aspect_macros::ErrorPosition;
use thiserror::Error;

/// Position in a source file.
///
/// `file_id` indexes into the program's `source_files` registry, populated
/// during preprocessing. Synthetic positions (e.g. codegen-side errors that
/// have no real source location) default to id 0 — the entry file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: usize,
    pub column: usize,
    pub file_id: u32,
}

impl Position {
    /// Construct a position with the default `file_id` of 0 — the entry file
    /// for compiled programs, or "unattributed" for synthetic positions.
    #[must_use]
    pub fn new(line: usize, column: usize) -> Self {
        Self {
            line,
            column,
            file_id: 0,
        }
    }

    /// Construct a position with an explicit `file_id`, used by the lexer
    /// when tokenising imported files.
    #[must_use]
    pub fn with_file(line: usize, column: usize, file_id: u32) -> Self {
        Self {
            line,
            column,
            file_id,
        }
    }

    #[must_use]
    pub fn start() -> Self {
        Self::new(1, 1)
    }
}

impl std::fmt::Display for Position {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

/// Lexer error types
#[derive(Error, Debug, ErrorPosition)]
pub enum LexerError {
    #[error("Unexpected character '{0}' at {1}")]
    UnexpectedChar(char, Position),

    #[error("Unterminated string literal at {0}")]
    UnterminatedString(Position),

    #[error("Unterminated block comment at {0}")]
    UnterminatedBlockComment(Position),

    #[error("Invalid number format '{0}' at {1}")]
    InvalidNumber(String, Position),

    #[error("Invalid escape sequence '\\{0}' at {1}")]
    InvalidEscape(char, Position),

    #[error("Unexpected end of input")]
    UnexpectedEof,
}
