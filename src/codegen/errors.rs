use crate::lexer::Position;
use aspect_macros::ErrorPosition;
use thiserror::Error;

/// Code generation error types
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

    /// LLVM couldn't resolve the requested `--target` triple to a usable
    /// target/target machine — e.g. an `aarch64-*` triple when only the x86
    /// backend is compiled into this `aspc` binary, or a malformed triple
    /// string. Has no source position: the failure is about the target the
    /// whole compilation was invoked with, not any one place in the source.
    #[error("unsupported compilation target '{triple}': {reason}")]
    UnsupportedTarget { triple: String, reason: String },

    /// A codegen failure with no meaningful source location: whole-module
    /// backend operations (running LLVM passes, emitting the object file) and
    /// registration-time type lowering that runs before any statement context.
    /// Position-less by design, so diagnostics print the bare message instead
    /// of a fabricated `0:0` prefix.
    #[error("{0}")]
    Internal(String),
}

/// A type-lowering failure with **no** source position attached.
///
/// The codegen type-lowering call graph (`LangTypeExt::to_llvm` and friends)
/// is value-only — a `LangType` carries no position — so those helpers cannot
/// name a source location on their own. They return this position-less error;
/// the caller grafts the relevant position on at the phase boundary via
/// [`TypeLoweringError::with_pos`], mirroring how `ParserError::from_symbol`
/// attaches a position to a position-less `SymbolError`.
#[derive(Debug)]
pub struct TypeLoweringError(pub String);

impl TypeLoweringError {
    /// Attach `pos`, yielding a positioned [`CodegenError::TypeError`].
    #[must_use]
    pub fn with_pos(self, pos: Position) -> CodegenError {
        CodegenError::TypeError(self.0, pos)
    }

    /// Convert to a position-less [`CodegenError::Internal`], for the few call
    /// sites (struct-field registration, arg-less sret lowering) that have no
    /// source location to attach.
    #[must_use]
    pub fn without_pos(self) -> CodegenError {
        CodegenError::Internal(self.0)
    }
}
