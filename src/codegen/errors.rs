use crate::lexer::Position;
use aspect_macros::ErrorPosition;
use thiserror::Error;

#[derive(Error, Debug, ErrorPosition)]
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

    /// LLVM couldn't resolve the `--target` triple to a usable target machine.
    /// Positionless: the failure is about the whole compilation's target, not
    /// any one place in the source.
    #[error("unsupported compilation target '{triple}': {reason}")]
    UnsupportedTarget { triple: String, reason: String },

    /// A codegen failure with no meaningful source location (whole-module
    /// backend operations, registration-time type lowering). Positionless by
    /// design, so diagnostics skip a fabricated `0:0` prefix.
    #[error("{0}")]
    Internal(String),
}

/// The codegen type-lowering call graph (`LangTypeExt::to_llvm` and friends)
/// is value-only — a `LangType` carries no position — so it returns this
/// positionless error and the caller grafts a position on at the phase
/// boundary via [`TypeLoweringError::with_pos`].
#[derive(Debug)]
pub struct TypeLoweringError(pub String);

impl TypeLoweringError {
    /// Attach `pos`, yielding a positioned [`CodegenError::TypeError`].
    #[must_use]
    pub fn with_pos(self, pos: Position) -> CodegenError {
        CodegenError::TypeError(self.0, pos)
    }

    /// A positionless [`CodegenError::Internal`], for the few call sites with
    /// no source location to attach.
    #[must_use]
    pub fn without_pos(self) -> CodegenError {
        CodegenError::Internal(self.0)
    }
}
