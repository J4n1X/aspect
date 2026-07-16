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

    #[error("Undefined type '{0}' at {1}")]
    UndefinedType(String, Position),

    #[error("Duplicate type '{0}' at {1}")]
    DuplicateType(String, Position),

    #[error("{0} at {1}")]
    MethodCallForm(String, Position),

    #[error("Expected '{0}' but found '{1}' at {2}")]
    ExpectedToken(String, String, Position),

    #[error("Type mismatch: expected '{0}' but got '{1}' at {2}")]
    TypeMismatch(String, String, Position),

    #[error("Undefined variable '{0}' at {1}")]
    UndefinedVariable(String, Position),

    #[error("Undefined function '{0}' at {1}")]
    UndefinedFunction(String, Position),

    /// A cross-module reference to a symbol whose defining module the use
    /// site's module does not *directly* import (import visibility is
    /// non-transitive). `defining`/`referring` arrive pre-rendered —
    /// `module 'std/math'`, or `the root module` for the anonymous root
    /// module `""` — via [`ParserError::not_imported`].
    #[error("{kind} '{name}' is defined in {defining}, which {referring} does not import at {pos}")]
    NotImported {
        kind: &'static str,
        name: String,
        defining: String,
        referring: String,
        pos: Position,
    },

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

    /// An `asm fn` parameter with no `: <register>` pin. Only pinned operands
    /// are admitted, so this is a parse error rather than a defaulted choice.
    #[error("asm fn parameter '{0}' must be pinned to a register (e.g. 'i64 {0}: rdi') at {1}")]
    AsmMissingParamRegister(String, Position),

    /// A non-void `asm fn` whose return value is not pinned to a register.
    #[error("asm fn '{0}' must pin its return value to a register (e.g. '-> i64: rax') at {1}")]
    AsmMissingReturnRegister(String, Position),

    /// A `-> u0` `asm fn` with a return pin: a void asm fn has no output
    /// register and no output constraint, so there is nothing to pin.
    #[error("asm fn '{0}' returns u0 and cannot pin a return register at {1}")]
    AsmVoidReturnRegister(String, Position),

    /// An `asm fn` whose body contains no assembly string literal.
    #[error("asm fn '{0}' body must contain at least one assembly string literal at {1}")]
    AsmEmptyBody(String, Position),

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

    /// Build a [`ParserError::NotImported`], rendering each module path for
    /// the message: the anonymous root module (the empty string) reads as
    /// `the root module`, anything else as `module '<path>'`.
    #[must_use]
    pub(crate) fn not_imported(
        kind: &'static str,
        name: impl Into<String>,
        defining_module: &str,
        referring_module: &str,
        pos: Position,
    ) -> Self {
        let describe = |module: &str| {
            if module.is_empty() {
                "the root module".to_string()
            } else {
                format!("module '{module}'")
            }
        };
        ParserError::NotImported {
            kind,
            name: name.into(),
            defining: describe(defining_module),
            referring: describe(referring_module),
            pos,
        }
    }

    /// Extract the source position from this error, if any.
    #[must_use]
    pub fn position(&self) -> Option<Position> {
        match self {
            ParserError::UnexpectedToken(_, pos) => Some(*pos),
            ParserError::DuplicateDeclaration(_, pos) => Some(*pos),
            ParserError::UndefinedType(_, pos) => Some(*pos),
            ParserError::DuplicateType(_, pos) => Some(*pos),
            ParserError::MethodCallForm(_, pos) => Some(*pos),
            ParserError::ExpectedToken(_, _, pos) => Some(*pos),
            ParserError::TypeMismatch(_, _, pos) => Some(*pos),
            ParserError::UndefinedVariable(_, pos) => Some(*pos),
            ParserError::UndefinedFunction(_, pos) => Some(*pos),
            ParserError::NotImported { pos, .. } => Some(*pos),
            ParserError::ArgumentCountMismatch(_, _, _, pos) => Some(*pos),
            ParserError::InvalidDereference(pos) => Some(*pos),
            ParserError::FunctionRedefinition(_, pos) => Some(*pos),
            ParserError::InvalidBinaryOperation(pos) => Some(*pos),
            ParserError::ExpectedExpression(pos) => Some(*pos),
            ParserError::ExpectedStatement(pos) => Some(*pos),
            ParserError::AsmMissingParamRegister(_, pos) => Some(*pos),
            ParserError::AsmMissingReturnRegister(_, pos) => Some(*pos),
            ParserError::AsmVoidReturnRegister(_, pos) => Some(*pos),
            ParserError::AsmEmptyBody(_, pos) => Some(*pos),
            ParserError::UnexpectedEof | ParserError::LexerError(_) => None,
        }
    }
}
