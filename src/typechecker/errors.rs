use crate::lexer::Position;
use crate::parser::LangType;
use thiserror::Error;

/// Type checker error types
#[derive(Error, Debug, Clone)]
pub enum TypeCheckError {
    #[error("Type mismatch: expected '{expected}' but found '{found}' at {position}")]
    TypeMismatch {
        expected: LangType,
        found: LangType,
        position: Position,
    },

    #[error("Undefined variable '{0}' at {1}")]
    UndefinedVariable(String, Position),

    #[error("Undefined function '{0}' at {1}")]
    UndefinedFunction(String, Position),

    #[error("Cannot apply operator '{operator}' to types '{left}' and '{right}' at {position}")]
    InvalidBinaryOperation {
        operator: String,
        left: LangType,
        right: LangType,
        position: Position,
    },

    #[error("Cannot apply unary operator '{operator}' to type '{operand}' at {position}")]
    InvalidUnaryOperation {
        operator: String,
        operand: LangType,
        position: Position,
    },

    #[error("Function '{name}' expects {expected} arguments but got {found} at {position}")]
    ArgumentCountMismatch {
        name: String,
        expected: usize,
        found: usize,
        position: Position,
    },

    #[error("Argument type mismatch in function '{name}': expected '{expected}' but found '{found}' at {position}")]
    ArgumentTypeMismatch {
        name: String,
        expected: LangType,
        found: LangType,
        position: Position,
    },

    #[error("Cannot dereference non-pointer type '{0}' at {1}")]
    InvalidDereference(LangType, Position),

    #[error("Cannot take reference of expression at {0}")]
    InvalidReference(Position),

    #[error("Return type mismatch: expected '{expected}' but found '{found}' at {position}")]
    ReturnTypeMismatch {
        expected: LangType,
        found: LangType,
        position: Position,
    },

    #[error("Missing return statement in function '{0}'")]
    MissingReturn(String),

    #[error("Cannot assign '{found}' to variable of type '{expected}' at {position}")]
    AssignmentTypeMismatch {
        expected: LangType,
        found: LangType,
        position: Position,
    },

    #[error("Condition must be a comparable type, found '{0}' at {1}")]
    InvalidConditionType(LangType, Position),

    #[error("Cannot cast from '{from}' to '{to}' at {position}")]
    InvalidCast {
        from: LangType,
        to: LangType,
        position: Position,
    },

    #[error("Cannot assign to const variable '{name}' at {position}")]
    AssignmentToConst { name: String, position: Position },

    #[error("List initializer has {found} element(s) but array only has room for {expected} at {position}")]
    ListInitLengthMismatch {
        expected: usize,
        found: usize,
        position: Position,
    },
}

impl TypeCheckError {
    /// Return the source position associated with this error, if any.
    #[must_use]
    pub fn position(&self) -> Option<Position> {
        match self {
            Self::TypeMismatch { position, .. }
            | Self::InvalidBinaryOperation { position, .. }
            | Self::InvalidUnaryOperation { position, .. }
            | Self::ArgumentCountMismatch { position, .. }
            | Self::ArgumentTypeMismatch { position, .. }
            | Self::ReturnTypeMismatch { position, .. }
            | Self::AssignmentTypeMismatch { position, .. }
            | Self::InvalidCast { position, .. }
            | Self::AssignmentToConst { position, .. }
            | Self::ListInitLengthMismatch { position, .. } => Some(*position),
            Self::UndefinedVariable(_, pos)
            | Self::UndefinedFunction(_, pos)
            | Self::InvalidDereference(_, pos)
            | Self::InvalidReference(pos)
            | Self::InvalidConditionType(_, pos) => Some(*pos),
            Self::MissingReturn(_) => None,
        }
    }
}
