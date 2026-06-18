use crate::lexer::{LangType, Position};

/// Literal values in the AST
#[derive(Debug, Clone, PartialEq)]
pub enum LiteralValue {
    Integer(i64),
    Float(f64),
    String(usize), // Index into string literals table
    Bool(bool),
}

/// Binary operators
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BinaryOp {
    // Arithmetic
    Add,
    Sub,
    Mul,
    Div,
    Mod,

    // Bitwise
    And,
    Or,
    Xor,
    LeftShift,
    RightShift,

    // Logical (short-circuit)
    LogicalAnd,
    LogicalOr,
}

// Operator precedence lives solely in the parser's `INFIX_OPS` table
// (`src/parser/expressions.rs`); it is the single source of truth for binding
// strength and includes comparison operators, which `BinaryOp` does not model.

/// Comparison operators
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ComparisonOp {
    Greater,
    Less,
    GreaterEqual,
    LessEqual,
    Equal,
    NotEqual,
}

/// Expression variants
#[derive(Debug, Clone, PartialEq)]
pub enum ExprKind {
    Literal(LiteralValue),
    Variable(String),
    Binary {
        left: Box<Expression>,
        op: BinaryOp,
        right: Box<Expression>,
    },
    Comparison {
        left: Box<Expression>,
        op: ComparisonOp,
        right: Box<Expression>,
    },
    Reference(Box<Expression>),   // &expr
    Dereference(Box<Expression>), // *expr
    UnaryNot(Box<Expression>),    // !expr (logical not)
    BitwiseNot(Box<Expression>),  // ~expr (bitwise not)
    FunctionCall {
        name: String,
        args: Vec<Expression>,
    },
    Cast {
        expr: Box<Expression>,
        target_type: LangType,
    },
    Alloc {
        alloc_type: LangType,
        count: Box<Expression>,
    },
    ListInitializer(Vec<Expression>),
    /// Field access `base.field`. `base` is a type-struct value or a
    /// (single-level) pointer-to-struct, which auto-dereferences.
    FieldAccess {
        base: Box<Expression>,
        field: String,
    },
    /// Named struct literal `Name { field = expr, ... }`. The struct is
    /// identified by its interned id; `fields` are in source order.
    StructLiteral {
        struct_id: u32,
        fields: Vec<(String, Expression)>,
    },
    /// A reference to a named function, as a value (function pointer).
    /// Produced for a bare function name `foo` and for `&foo` (the parser
    /// collapses the address-of). Carries the function's name; the FnPtr
    /// type is stamped on `expr_type`.
    FunctionRef(String),
    /// An indirect call through a function-pointer value: `callee(args)`.
    /// Distinct from `FunctionCall` (a direct call by name) because codegen
    /// must look up the signature via the FnPtr id and emit `build_indirect_call`.
    IndirectCall {
        callee: Box<Expression>,
        args: Vec<Expression>,
    },
}

/// Expression with type information
#[derive(Debug, Clone, PartialEq)]
pub struct Expression {
    pub kind: ExprKind,
    pub expr_type: LangType,
    pub pos: Position,
}

impl Expression {
    #[must_use]
    pub fn new(kind: ExprKind, expr_type: LangType, pos: Position) -> Self {
        Self {
            kind,
            expr_type,
            pos,
        }
    }
}

/// Statement variants
#[derive(Debug, Clone, PartialEq)]
pub enum StatementKind {
    Expression(Expression),
    Block(Vec<Statement>),
    Return(Option<Expression>),
    If {
        condition: Expression,
        then_block: Vec<Statement>,
        else_block: Option<Vec<Statement>>,
    },
    While {
        condition: Expression,
        body: Vec<Statement>,
    },
    For {
        init: Option<Box<Statement>>,
        condition: Option<Expression>,
        increment: Option<Box<Statement>>,
        body: Vec<Statement>,
    },
    VarDecl {
        var_type: LangType,
        name: String,
        initializer: Option<Expression>,
    },
    VarAssign {
        name: String,
        value: Expression,
    },
    DerefAssign {
        target: Expression, // Must be a dereference expression
        value: Expression,
    },
    FieldAssign {
        target: Expression, // Must be a field-access expression
        value: Expression,
    },
    Break,
    Continue,
}

/// Statement with position information
#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    pub kind: StatementKind,
    pub pos: Position,
}

impl Statement {
    #[must_use]
    pub fn new(kind: StatementKind, pos: Position) -> Self {
        Self { kind, pos }
    }
}

/// Function prototype
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionProto {
    pub name: String,
    pub params: Vec<(LangType, String)>,
    pub return_type: LangType,
    pub is_extern: bool,
    pub pos: Position,
}

/// Function definition
#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub proto: FunctionProto,
    pub body: Vec<Statement>,
}

/// Global variable
#[derive(Debug, Clone, PartialEq)]
pub struct GlobalVar {
    pub var_type: LangType,
    pub name: String,
    pub initializer: Option<Expression>,
    pub pos: Position,
}

/// Complete program AST
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub functions: Vec<Function>,
    pub global_vars: Vec<GlobalVar>,
    pub string_literals: Vec<String>,
    /// Cross-phase global symbol table (functions, type-structs, aliases),
    /// built by the parser and consumed by the type checker and code generator.
    pub symbols: crate::symbol::module::ModuleSymbols,
    /// Source-file registry indexed by `Position::file_id` — entry file at id 0,
    /// each `$include`-pulled file after that. Empty for synthetic programs
    /// (e.g. checker unit tests that don't go through the preprocessor).
    pub source_files: Vec<std::path::PathBuf>,
}
