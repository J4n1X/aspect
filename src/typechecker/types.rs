use crate::lexer::{LangType, Position};

/// A simplified expression representation for type checking purposes.
/// This captures the type structure of an expression along with its location.
#[derive(Debug, Clone)]
pub enum TypeExpr {
    /// A nested type expression (e.g., pointer to something, result of operation)
    Nested {
        result_type: LangType,
        inner: Box<TypeExpr>,
        pos: Position,
    },
    /// A leaf type expression (literal, variable, etc.)
    Single {
        expr_type: LangType,
        pos: Position,
    },
}

impl TypeExpr {
    /// Create a new single (leaf) type expression
    #[must_use]
    pub fn single(expr_type: LangType, pos: Position) -> Self {
        TypeExpr::Single { expr_type, pos }
    }

    /// Create a nested type expression
    #[must_use]
    pub fn nested(result_type: LangType, inner: TypeExpr, pos: Position) -> Self {
        TypeExpr::Nested {
            result_type,
            inner: Box::new(inner),
            pos,
        }
    }

    /// Get the resulting type of this expression
    #[must_use]
    pub fn result_type(&self) -> &LangType {
        match self {
            TypeExpr::Nested { result_type, .. } => result_type,
            TypeExpr::Single { expr_type, .. } => expr_type,
        }
    }

    /// Get the position of this expression
    #[must_use]
    pub fn position(&self) -> Position {
        match self {
            TypeExpr::Nested { pos, .. } | TypeExpr::Single { pos, .. } => *pos
        }
    }
}

/// A type constraint that needs to be checked.
/// These are collected during the first pass and verified in the second pass.
#[derive(Debug, Clone)]
pub enum TypeConstraint {
    /// Two types must be equal
    Equal {
        expected: LangType,
        found: LangType,
        pos: Position,
        context: ConstraintContext,
    },
    /// A type must be compatible with another (e.g., for assignment)
    Compatible {
        expected: LangType,
        found: LangType,
        pos: Position,
        context: ConstraintContext,
    },
    /// Binary operation requires compatible operand types
    BinaryOp {
        left: LangType,
        right: LangType,
        operator: String,
        pos: Position,
    },
    /// Unary operation requires a specific type category
    UnaryOp {
        operand: LangType,
        operator: String,
        pos: Position,
    },
    /// Dereference requires pointer type
    Dereference {
        operand: LangType,
        pos: Position,
    },
    /// Reference creates a pointer type
    Reference {
        operand: LangType,
        pos: Position,
    },
    /// Function call argument types
    FunctionCall {
        func_name: String,
        expected_args: Vec<LangType>,
        found_args: Vec<LangType>,
        pos: Position,
    },
    /// Return type must match function signature
    Return {
        expected: LangType,
        found: LangType,
        func_name: String,
        pos: Position,
    },
    /// Cast must be valid
    Cast {
        from: LangType,
        to: LangType,
        pos: Position,
    },
    /// Condition must be a numeric/pointer type
    Condition {
        found: LangType,
        pos: Position,
    },
}

/// Context for where a constraint originated (for better error messages)
#[derive(Debug, Clone)]
pub enum ConstraintContext {
    Assignment,
    Initialization,
    Return,
    Argument { func_name: String, arg_index: usize },
    Comparison,
    Arithmetic,
    Other(String),
}

impl std::fmt::Display for ConstraintContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConstraintContext::Assignment => write!(f, "assignment"),
            ConstraintContext::Initialization => write!(f, "initialization"),
            ConstraintContext::Return => write!(f, "return statement"),
            ConstraintContext::Argument { func_name, arg_index } => {
                write!(f, "argument {} of function '{}'", arg_index + 1, func_name)
            }
            ConstraintContext::Comparison => write!(f, "comparison"),
            ConstraintContext::Arithmetic => write!(f, "arithmetic operation"),
            ConstraintContext::Other(s) => write!(f, "{s}"),
        }
    }
}
