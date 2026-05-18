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
}
