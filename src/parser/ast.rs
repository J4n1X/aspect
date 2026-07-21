use crate::{lexer::{LangType, Position}, symbol::module::Visibility};

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
        struct_id: u32,
        fields: Vec<(String, Expression)>,
    },
    /// An enum variant value `EnumName.Variant`, resolved by the parser to the
    /// variant's interned enum id and its integer value (the variant's index).
    /// Typed as the enum type (`TypeBase::Enum`), which lowers to a compile-time
    /// `i32` constant in codegen. A dedicated node — *not* an integer literal —
    /// so the checker never lets it coerce to a bare integer or narrow to a
    /// sibling's width: an enum value only ever satisfies its own enum type.
    EnumValue {
        enum_id: u32,
        value: i64,
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
    /// An *unresolved* method or fn-pointer-field call `base.name(args)` whose
    /// receiver type is not known until type-checking. The parser resolves such
    /// calls at parse time (`build_method_call`) and never emits this node; it
    /// exists so metaprogram-generated AST (Three-Hook-Metasystem Phases 3/4),
    /// which has no parse-time receiver types, can defer method-vs-field
    /// dispatch, static-vs-instance resolution, and `Type$method` mangling to
    /// the checker. The checker resolves it and **rewrites the node in place**
    /// into a `FunctionCall` (method) or `IndirectCall` (fn-ptr field), so
    /// codegen never sees a `MethodCall`. See `synth_method_call`.
    MethodCall {
        base: Box<Expression>,
        name: String,
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
    /// A value-block: `{ stmt* }` in *expression* position. Its value is
    /// produced by `return <expr>` statements inside, which bind to the
    /// innermost value-block rather than the enclosing function (so a
    /// wrapping block captures every exit path of the code it encases).
    /// The type checker verifies that every control path returns a value
    /// and stamps the block's type; `break`/`continue` pass through to
    /// enclosing loops. Distinguished from a `ListInitializer` at parse
    /// time: a brace expression that parses as a comma-separated list *is*
    /// a list; anything else is re-parsed as statements.
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

/// An `@name` / `@name(args)` attribute: inert metadata the parser attaches
/// to the item, field, or statement it precedes. The parser never interprets
/// attributes — meaning is assigned by later phases (rules, transforms), or
/// never. Args are parsed as ordinary expressions but never type-checked.
#[derive(Debug, Clone, PartialEq)]
pub struct Attribute {
    pub name: String,
    /// `(...)` arguments in source order; empty for the bare `@name` form.
    pub args: Vec<Expression>,
    /// The `@` sigil.
    pub pos: Position,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Statement {
    pub kind: StatementKind,
    pub pos: Position,
    /// Leading attributes in source order — which is outside-in: in
    /// `@a @b x`, `a` is applied last (`a(b(x))`).
    pub attrs: Vec<Attribute>,
}

impl Statement {
    #[must_use]
    pub fn new(kind: StatementKind, pos: Position) -> Self {
        Self {
            kind,
            pos,
            attrs: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FunctionProto {
    pub name: String,
    pub params: Vec<(LangType, String)>,
    pub return_type: LangType,
    /// Module visibility: whether another Aspect module may name this function
    /// through `$import`. `public` exports it across the module boundary;
    /// private (the default) confines it to its defining module. This is a
    /// *name-resolution* property enforced at parse time — it has nothing to
    /// do with LLVM linkage (see `export`).
    pub vis: crate::symbol::module::Visibility,
    /// Foreign linkage: whether the symbol leaves the object file with external
    /// linkage, so non-Aspect code (C, a separate link step, a C runtime) can
    /// reference it by name. `export` opts in; the default is internal linkage,
    /// which lets `globaldce` strip the symbol when unreachable — the whole
    /// reason an unused stdlib doesn't bloat every binary. Orthogonal to `vis`:
    /// the two compose (`public export fn`).
    pub export: bool,
    /// Leading attributes in source order (outside-in, leftmost applied last).
    pub attrs: Vec<Attribute>,
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
    /// Leading attributes in source order (outside-in, leftmost applied last).
    pub attrs: Vec<Attribute>,
    pub pos: Position,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    pub functions: Vec<Function>,
    pub global_vars: Vec<GlobalVar>,
    pub string_literals: Vec<String>,
    /// Cross-phase global symbol table (functions, type-structs, aliases),
    /// built by the parser and consumed by the type checker and code generator.
    pub symbols: crate::symbol::module::ModuleSymbols,
    /// Source-file registry indexed by `Position::file_id` — entry file at id 0,
    /// each `$import`-pulled file after that. Empty for synthetic programs
    /// (e.g. checker unit tests that don't go through the preprocessor).
    pub source_files: Vec<std::path::PathBuf>,
}
