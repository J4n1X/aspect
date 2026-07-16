use crate::lexer::Position;
use thiserror::Error;

/// Code generation error types
#[derive(Error, Debug)]
pub enum CodegenError {
    #[error("Undefined variable '{0}' at {1}")]
    UndefinedVariable(String, Position),

    #[error("Undefined function '{0}' at {1}")]
    UndefinedFunction(String, Position),

    #[error("Type error: {0} at {1}")]
    TypeError(String, Position),

    #[error("Invalid operation: {0} at {1}")]
    InvalidOperation(String, Position),

    #[error("Unexpected statement at {0}")]
    UnexpectedStatement(Position),

    #[error("LLVM error: {0}")]
    LLVMError(#[from] inkwell::builder::BuilderError),

    #[error("Main function not found")]
    MainNotFound,

    #[error("Main function must return i32")]
    InvalidMainSignature,

    #[error("Missing return statement in function '{0}' at {1}")]
    MissingReturn(String, Position),

    /// LLVM couldn't resolve the requested `--target` triple to a usable
    /// target/target machine — e.g. an `aarch64-*` triple when only the x86
    /// backend is compiled into this `aspc` binary, or a malformed triple
    /// string. Has no source position: the failure is about the target the
    /// whole compilation was invoked with, not any one place in the source.
    #[error("unsupported compilation target '{triple}': {reason}")]
    UnsupportedTarget { triple: String, reason: String },
}

impl CodegenError {
    /// Extract the source position from this error, if any.
    #[must_use]
    pub fn position(&self) -> Option<Position> {
        match self {
            Self::UndefinedVariable(_, pos)
            | Self::UndefinedFunction(_, pos)
            | Self::TypeError(_, pos)
            | Self::InvalidOperation(_, pos)
            | Self::UnexpectedStatement(pos)
            | Self::MissingReturn(_, pos) => Some(*pos),
            Self::LLVMError(_)
            | Self::MainNotFound
            | Self::InvalidMainSignature
            | Self::UnsupportedTarget { .. } => None,
        }
    }
}
