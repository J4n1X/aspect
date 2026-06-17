use crate::lexer::Position;
use crate::symbol::table::SymbolError;
use thiserror::Error;

/// Parser error types
#[derive(Error, Debug)]
pub enum ParserError {
    #[error("Unexpected token '{0}' at {1}")]
    UnexpectedToken(String, Position),

    #[error("Duplicate declaration of '{0}' at {1}")]
    DuplicateDeclaration(String, Position),

    #[error("Expected '{0}' but found '{1}' at {2}")]
    ExpectedToken(String, String, Position),

    #[error("Type mismatch: expected '{0}' but got '{1}' at {2}")]
    TypeMismatch(String, String, Position),

    #[error("Undefined variable '{0}' at {1}")]
    UndefinedVariable(String, Position),

    #[error("Undefined function '{0}' at {1}")]
    UndefinedFunction(String, Position),

    #[error("Function '{0}' expects {1} arguments but got {2} at {3}")]
    ArgumentCountMismatch(String, usize, usize, Position),

    #[error("Cannot dereference non-pointer type at {0}")]
    InvalidDereference(Position),

    #[error("Redefinition of function '{0}' at {1}")]
    FunctionRedefinition(String, Position),

    #[error("Invalid binary operation at {0}")]
    InvalidBinaryOperation(Position),

    #[error("Expected expression at {0}")]
    ExpectedExpression(Position),

    #[error("Expected statement at {0}")]
    ExpectedStatement(Position),

    #[error("Unexpected end of input")]
    UnexpectedEof,

    #[error("Lexer error: {0}")]
    LexerError(#[from] crate::lexer::LexerError),
}

impl ParserError {
    /// Attach a source position to a [`SymbolError`] raised by the symbol table.
    #[must_use]
    pub(crate) fn from_symbol(err: SymbolError, pos: Position) -> Self {
        match err {
            SymbolError::DuplicateVariable(name) => ParserError::DuplicateDeclaration(name, pos),
            SymbolError::FunctionAlreadyDefined(name) | SymbolError::SignatureMismatch(name) => {
                ParserError::FunctionRedefinition(name, pos)
            }
        }
    }

    /// Extract the source position from this error, if any.
    #[must_use]
    pub fn position(&self) -> Option<Position> {
        match self {
            ParserError::UnexpectedToken(_, pos) => Some(*pos),
            ParserError::DuplicateDeclaration(_, pos) => Some(*pos),
            ParserError::ExpectedToken(_, _, pos) => Some(*pos),
            ParserError::TypeMismatch(_, _, pos) => Some(*pos),
            ParserError::UndefinedVariable(_, pos) => Some(*pos),
            ParserError::UndefinedFunction(_, pos) => Some(*pos),
            ParserError::ArgumentCountMismatch(_, _, _, pos) => Some(*pos),
            ParserError::InvalidDereference(pos) => Some(*pos),
            ParserError::FunctionRedefinition(_, pos) => Some(*pos),
            ParserError::InvalidBinaryOperation(pos) => Some(*pos),
            ParserError::ExpectedExpression(pos) => Some(*pos),
            ParserError::ExpectedStatement(pos) => Some(*pos),
            ParserError::UnexpectedEof | ParserError::LexerError(_) => None,
        }
    }
}
