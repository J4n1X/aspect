use crate::lexer::Position;
use crate::symbol::table::SymbolError;
use aspect_macros::ErrorPosition;
use thiserror::Error;

/// Parser error types
#[derive(Error, Debug, ErrorPosition)]
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

    /// A cross-module use of a type-struct that is not `public` — naming it
    /// or calling its methods: the defining module is imported, but the type
    /// itself was not exported. `defining`/`referring` arrive pre-rendered
    /// via [`ParserError::private_type`].
    #[error("type-struct '{name}' is private to {defining} and cannot be used from {referring} — declare it `public type` to export it at {pos}")]
    PrivateType {
        name: String,
        defining: String,
        referring: String,
        pos: Position,
    },

    /// A cross-module use of a free function or global variable that is not
    /// `public`: the defining module is imported, but the symbol itself was
    /// not exported to the module namespace. `defining`/`referring` arrive
    /// pre-rendered via [`ParserError::private_symbol`].
    #[error("{kind} '{name}' is private to {defining} and cannot be used from {referring} — declare it `public` to export it at {pos}")]
    PrivateSymbol {
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

    /// Render a module path for an error message: the anonymous root module
    /// (the empty string) reads as `the root module`, anything else as
    /// `module '<path>'`.
    fn describe_module(module: &str) -> String {
        if module.is_empty() {
            "the root module".to_string()
        } else {
            format!("module '{module}'")
        }
    }

    /// Build a [`ParserError::NotImported`], rendering each module path via
    /// [`ParserError::describe_module`].
    #[must_use]
    pub(crate) fn not_imported(
        kind: &'static str,
        name: impl Into<String>,
        defining_module: &str,
        referring_module: &str,
        pos: Position,
    ) -> Self {
        ParserError::NotImported {
            kind,
            name: name.into(),
            defining: Self::describe_module(defining_module),
            referring: Self::describe_module(referring_module),
            pos,
        }
    }

    /// Build a [`ParserError::PrivateType`], rendering each module path via
    /// [`ParserError::describe_module`].
    #[must_use]
    pub(crate) fn private_type(
        name: impl Into<String>,
        defining_module: &str,
        referring_module: &str,
        pos: Position,
    ) -> Self {
        ParserError::PrivateType {
            name: name.into(),
            defining: Self::describe_module(defining_module),
            referring: Self::describe_module(referring_module),
            pos,
        }
    }

    /// Build a [`ParserError::PrivateSymbol`], rendering each module path via
    /// [`ParserError::describe_module`].
    #[must_use]
    pub(crate) fn private_symbol(
        kind: &'static str,
        name: impl Into<String>,
        defining_module: &str,
        referring_module: &str,
        pos: Position,
    ) -> Self {
        ParserError::PrivateSymbol {
            kind,
            name: name.into(),
            defining: Self::describe_module(defining_module),
            referring: Self::describe_module(referring_module),
            pos,
        }
    }
}
