use thiserror::Error;

/// Position in source file
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Position {
    pub line: usize,
    pub column: usize,
}

impl Position {
    #[must_use]
    pub fn new(line: usize, column: usize) -> Self {
        Self { line, column }
    }

    #[must_use]
    pub fn start() -> Self {
        Self { line: 1, column: 1 }
    }
}

impl std::fmt::Display for Position {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "{}:{}", self.line, self.column)
    }
}

/// Lexer error types
#[derive(Error, Debug)]
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
