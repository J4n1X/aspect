use crate::lexer::Position;
use crate::symbol::table::SymbolError;
use aspect_macros::ErrorPosition;
use thiserror::Error;

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

    #[error("enum '{enum_name}' has no variant '{variant}' at {pos}")]
    UnknownVariant {
        enum_name: String,
        variant: String,
        pos: Position,
    },

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

    /// The use site's module does not *directly* import the defining module —
    /// import visibility is non-transitive.
    #[error("{kind} '{name}' is defined in {defining}, which {referring} does not import at {pos}")]
    NotImported {
        kind: &'static str,
        name: String,
        defining: String,
        referring: String,
        pos: Position,
    },

    /// The defining module is imported, but the type-struct itself was not
    /// exported (`public type`).
    #[error("type-struct '{name}' is private to {defining} and cannot be used from {referring} — declare it `public type` to export it at {pos}")]
    PrivateType {
        name: String,
        defining: String,
        referring: String,
        pos: Position,
    },

    /// The defining module is imported, but the enum itself was not exported
    /// (`public enum`).
    #[error("enum '{name}' is private to {defining} and cannot be used from {referring} — declare it `public enum` to export it at {pos}")]
    PrivateEnum {
        name: String,
        defining: String,
        referring: String,
        pos: Position,
    },

    /// The defining module is imported, but this free function or global was
    /// not exported to the module namespace (`public`).
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

    #[error("asm fn parameter '{0}' must be pinned to a register (e.g. 'i64 {0}: rdi') at {1}")]
    AsmMissingParamRegister(String, Position),

    #[error("asm fn '{0}' must pin its return value to a register (e.g. '-> i64: rax') at {1}")]
    AsmMissingReturnRegister(String, Position),

    #[error("asm fn '{0}' returns u0 and cannot pin a return register at {1}")]
    AsmVoidReturnRegister(String, Position),

    #[error("asm fn '{0}' body must contain at least one assembly string literal at {1}")]
    AsmEmptyBody(String, Position),

    #[error("Unexpected end of input")]
    UnexpectedEof,

    #[error("Lexer error: {0}")]
    LexerError(#[from] crate::lexer::LexerError),
}

impl ParserError {
    #[must_use]
    pub(crate) fn from_symbol(err: SymbolError, pos: Position) -> Self {
        match err {
            SymbolError::DuplicateVariable(name) => ParserError::DuplicateDeclaration(name, pos),
            SymbolError::FunctionAlreadyDefined(name) | SymbolError::SignatureMismatch(name) => {
                ParserError::FunctionRedefinition(name, pos)
            }
        }
    }

    /// The anonymous root module (the empty string) reads as `the root
    /// module`, anything else as `module '<path>'`.
    fn describe_module(module: &str) -> String {
        if module.is_empty() {
            "the root module".to_string()
        } else {
            format!("module '{module}'")
        }
    }

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

    #[must_use]
    pub(crate) fn private_enum(
        name: impl Into<String>,
        defining_module: &str,
        referring_module: &str,
        pos: Position,
    ) -> Self {
        ParserError::PrivateEnum {
            name: name.into(),
            defining: Self::describe_module(defining_module),
            referring: Self::describe_module(referring_module),
            pos,
        }
    }

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
