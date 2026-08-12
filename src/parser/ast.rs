use crate::lexer::{LangType, Position};
use crate::symbol::ids::{EnumId, StructId, SumId};
use crate::symbol::module::Visibility;

#[derive(Debug, Clone, PartialEq)]
pub enum LiteralValue {
    Integer(i64),
    Float(f64),
    String(usize), // Index into string literals table
    Bool(bool),
}

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

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ComparisonOp {
    Greater,
    Less,
    GreaterEqual,
    LessEqual,
    Equal,
    NotEqual,
}

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
        struct_id: StructId,
        fields: Vec<(String, Expression)>,
    },
    /// An enum variant value `EnumName.Variant`, resolved to the variant's enum
    /// id and integer value. A dedicated node — *not* an integer literal — so
    /// the checker never lets it coerce to a bare integer: an enum value only
    /// ever satisfies its own enum type. Lowers to a compile-time `i32`.
    EnumValue {
        enum_id: EnumId,
        value: i64,
    },
    /// Sum construction `SumName.Variant(args…)` (bare `SumName.Variant` for a
    /// payload-less variant). The variant is resolved to its declaration index
    /// at parse time; `args` are the payload values in field order — arity was
    /// already checked by the parser, argument *types* by the checker.
    SumConstruct {
        sum_id: SumId,
        variant: u32,
        args: Vec<Expression>,
    },
    /// Binding-free `is` probe: `scrutinee is Variant` (bare variant only —
    /// any parenthesized pattern is the binding form). An ordinary `bool`
    /// expression at the comparison tier, usable anywhere.
    Is {
        scrutinee: Box<Expression>,
        sum_id: SumId,
        variant: u32,
    },
    /// Binding `is` probe: `scrutinee is Variant(a, _, b)`. **Not an
    /// expression** — legal only as a leaf of the root `&&` spine of an
    /// `if`/`elif`/`while` condition; the parser polices parenthesized and
    /// `||` placements, the checker everything else. Binders are copies
    /// (`None` = `_`), registered at parse time in textual order, scoped to
    /// the success block.
    IsBinding {
        scrutinee: Box<Expression>,
        sum_id: SumId,
        variant: u32,
        binders: Vec<Option<(String, LangType)>>,
    },
    /// A named function as a value (function pointer). Produced for a bare
    /// `foo` and for `&foo` (the parser collapses the address-of).
    FunctionRef(String),
    /// An indirect call through a function-pointer value: `callee(args)`.
    /// Distinct from `FunctionCall` because codegen looks up the signature via
    /// the FnPtr id and emits `build_indirect_call`.
    IndirectCall {
        callee: Box<Expression>,
        args: Vec<Expression>,
    },
    /// `sizeof(T)` — the compile-time size of a type in bytes. Lowered to a
    /// `u64` constant at codegen using the target data layout (so struct
    /// padding and target pointer width are respected). The type checker
    /// stamps the expression type as `u64`.
    SizeOf(LangType),
    /// `null` — the untyped null pointer. Lowered to LLVM's opaque `ptr`
    /// null constant. In `check` mode the typechecker stamps the target
    /// pointer type onto the AST; in `synth` mode it stays as the generic
    /// `u8*` placeholder so the same coercion rules used for any other
    /// pointer-to-pointer comparison apply.
    Null,
    /// A value-block: `{ stmt* }` in *expression* position. Its value comes
    /// from `return <expr>` statements inside, which bind to the innermost
    /// value-block rather than the enclosing function. Distinguished from a
    /// `ListInitializer` at parse time: a brace expression that parses as a
    /// comma-separated list *is* a list; anything else re-parses as statements.
    ValueBlock(Vec<Statement>),
}

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

/// One `case` pattern.
#[derive(Debug, Clone, PartialEq)]
pub enum SwitchPattern {
    /// A constant pattern on an int/bool/enum scrutinee, kept as its parsed
    /// expression (integer/bool literal or `EnumValue`). The checker validates
    /// it against the scrutinee type with the ordinary rules and rejects
    /// non-constant shapes.
    Const(Expression),
    /// `Variant(a, _, b)` or bare `Variant` on a sum scrutinee, resolved at
    /// parse time. `binders` has one entry per payload field, in order:
    /// `Some((name, type))` binds a copy, `None` discards (`_`, or every field
    /// when the pattern is a bare name).
    SumVariant {
        variant: u32,
        binders: Vec<Option<(String, LangType)>>,
    },
}

/// One `case` arm: its pattern list, body, and source position. A pattern
/// that binds must be the arm's only pattern (enforced at parse).
#[derive(Debug, Clone, PartialEq)]
pub struct SwitchArm {
    pub patterns: Vec<SwitchPattern>,
    pub body: Vec<Statement>,
    pub pos: Position,
}

#[derive(Debug, Clone, PartialEq)]
pub enum StatementKind {
    Expression(Expression),
    Block(Vec<Statement>),
    Return(Option<Expression>),
    /// `switch scrutinee { case … { } … default { } }`. The scrutinee is
    /// evaluated exactly once. `complete` is true when the arms alone cover
    /// the scrutinee (fully-listed sum/enum, `bool` with both literals) —
    /// computed dedup-aware by the parser so the registry-less termination
    /// analysis (`stmt_always_returns`) can read coverage without symbols.
    Switch {
        scrutinee: Expression,
        arms: Vec<SwitchArm>,
        default: Option<Vec<Statement>>,
        complete: bool,
    },
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

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionProto {
    pub name: String,
    pub params: Vec<(LangType, String)>,
    pub return_type: LangType,
    /// Whether another Aspect module may name this function through `$import`.
    /// A *name-resolution* property enforced at parse time, nothing to do with
    /// LLVM linkage (see `export`).
    pub vis: crate::symbol::module::Visibility,
    /// Whether the symbol leaves the object file with external linkage, so
    /// non-Aspect code can reference it by name. The default internal linkage
    /// lets `globaldce` strip it when unreachable — why an unused stdlib
    /// doesn't bloat every binary. Orthogonal to `vis`; the two compose.
    pub export: bool,
    pub pos: Position,
}

/// A register name as written after a `:` or inside `clobbers(...)`.
///
/// Stored verbatim and never canonicalised: LLVM picks the correctly-sized
/// physical sub-register from the operand's LLVM type, so the user's exact
/// spelling is what must reach the constraint string.
#[derive(Debug, Clone, PartialEq)]
pub struct AsmReg {
    pub name: String,
    pub pos: Position,
}

/// The instruction-sequence body of an `asm fn`, plus its register contract.
///
/// Registers live here rather than in `FunctionProto::params` because that
/// `Vec<(LangType, String)>` is the shape the symbol table compares for
/// signature equality and every other phase destructures; widening it would
/// touch code with nothing to do with inline asm.
///
/// Parser-established invariants: `param_regs` is parallel to `proto.params`,
/// `return_reg.is_some() == !proto.return_type.is_void_value()`, and `lines`
/// is never empty.
#[derive(Debug, Clone, PartialEq)]
pub struct AsmSpec {
    pub param_regs: Vec<AsmReg>,
    /// `None` for a `-> u0` asm fn, which has no output constraint.
    pub return_reg: Option<AsmReg>,
    /// Source order; may include the pseudo-register `memory`.
    pub clobbers: Vec<AsmReg>,
    /// One line per string literal, joined with `\n` for LLVM.
    pub lines: Vec<String>,
    /// The `asm` keyword, where whole-declaration diagnostics are reported.
    pub pos: Position,
}

/// The instruction-sequence body of a `naked fn`.
///
/// Unlike [`AsmSpec`], a naked function carries no register contract: it has
/// no prologue/epilogue, so parameters arrive in — and results leave through —
/// their platform-ABI registers, which the asm body addresses directly. There
/// is therefore nothing to pin, and `lines` is the whole story.
#[derive(Debug, Clone, PartialEq)]
pub struct NakedSpec {
    /// One line per string literal, joined with `\n` for LLVM. Never empty.
    pub lines: Vec<String>,
    /// The `naked` keyword, where whole-declaration diagnostics are reported.
    pub pos: Position,
}

/// Where a function's body comes from, and thus how it is lowered. A function
/// is exactly one of these — the variants are what make `extern`-with-a-body,
/// or `asm`-and-statements, unrepresentable rather than merely undocumented.
#[derive(Debug, Clone, PartialEq)]
pub enum FunctionBody {
    /// `fn` — Aspect statements. Empty until pass 2 of `do_parse_program`
    /// fills it in, so callers can be declared before their callees.
    Aspect(Vec<Statement>),
    /// `extern fn` — defined in another object file.
    Extern,
    /// `asm fn` — the instructions are the body, with a register contract.
    Asm(AsmSpec),
    /// `naked fn` — the instructions are the body, lowered with LLVM's `naked`
    /// attribute (no prologue/epilogue), so args/results follow the raw ABI.
    Naked(NakedSpec),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub proto: FunctionProto,
    pub body: FunctionBody,
}

#[derive(Debug, Clone, PartialEq)]
pub struct GlobalVar {
    pub var_type: LangType,
    pub name: String,
    pub initializer: Option<Expression>,
    /// Module visibility (`public` exports the name across `$import`); see
    /// [`FunctionProto::vis`]. Enforced at parse time, independent of linkage.
    pub vis: Visibility,
    /// Foreign linkage (`export` gives external linkage); see
    /// [`FunctionProto::export`]. Orthogonal to `vis`; the two compose.
    pub export: bool,
    pub pos: Position,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub functions: Vec<Function>,
    pub global_vars: Vec<GlobalVar>,
    pub string_literals: Vec<String>,
    /// Cross-phase global symbol table (functions, type-structs, aliases),
    /// built by the parser and read by the type checker and code generator.
    pub symbols: std::rc::Rc<crate::symbol::module::ModuleSymbols>,
    /// Source-file registry indexed by `Position::file_id` — entry file at id 0,
    /// each `$import`-pulled file after that. Empty for synthetic programs
    /// (e.g. checker unit tests that don't go through the preprocessor).
    pub source_files: Vec<std::path::PathBuf>,
}
