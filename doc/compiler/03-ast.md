# AST Reference

The AST types are defined in `src/parser/ast.rs`. The parser produces a `Program` containing functions, global variables, a string literal table, the cross-phase global symbol table (`symbols`), and the source-file registry (`source_files`).

## Program

```rust
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
```

`symbols` is the registry against which `TypeBase::Struct(id)` and
`TypeBase::FnPtr(id)` resolve their interned ids — without it, neither struct
nor function-pointer types can be interpreted.

## Function

```rust
pub struct Function {
    pub proto: FunctionProto,
    pub body: FunctionBody,
}

pub struct FunctionProto {
    pub name: String,
    pub params: Vec<(LangType, String)>,  // (type, name) pairs
    pub return_type: LangType,
    pub vis: Visibility,                  // `public` => external linkage
    pub pos: Position,
}
```

`vis` reuses `symbol::module::Visibility`. It governs *linkage* only —
whether the symbol leaves the object file — not who may call the function.
See [06-codegen](06-codegen.md#function-linkage).

A function is exactly one *kind*, and `FunctionBody` is what says which:

```rust
pub enum FunctionBody {
    Aspect(Vec<Statement>),  // fn        — empty until pass 2 fills it
    Extern,                  // extern fn — defined in another object file
    Asm(AsmSpec),            // asm fn    — the instructions are the body
}
```

The kind is an enum rather than a `bool` plus an `Option` because the three
kinds are mutually exclusive: `extern`-with-a-body and `asm`-with-statements
are unrepresentable rather than merely undocumented, and every consumer that
lowers a function matches exhaustively instead of cross-checking fields.

### AsmSpec

The register contract of an `asm fn` (see [06-codegen](06-codegen.md) for the
lowering).

```rust
pub struct AsmSpec {
    pub param_regs: Vec<AsmReg>,      // parallel to proto.params
    pub return_reg: Option<AsmReg>,   // None for a `-> u0` asm fn
    pub clobbers: Vec<AsmReg>,        // may include the pseudo-register `memory`
    pub lines: Vec<String>,           // one per string literal, joined with \n
    pub pos: Position,
}

pub struct AsmReg {
    pub name: String,  // verbatim; never canonicalised
    pub pos: Position,
}
```

Registers live here rather than widening `FunctionProto::params`, whose
`(LangType, String)` shape the symbol table compares for signature equality
and every other phase destructures.

## GlobalVar

```rust
pub struct GlobalVar {
    pub var_type: LangType,
    pub name: String,
    pub initializer: Option<Expression>,
    pub vis: Visibility,          // `public` => external linkage
    pub pos: Position,
}
```

`vis` means the same as on [`FunctionProto`](#function): linkage only.
A private global gets `private` linkage and is collected when unused.

## Expression

```rust
pub struct Expression {
    pub kind: ExprKind,
    pub expr_type: LangType,  // Resolved during parsing
    pub pos: Position,
}
```

### ExprKind Variants

| Variant | Description | Example |
|---------|-------------|---------|
| `Literal(LiteralValue)` | Integer, float, string, or bool literal | `42`, `3.14`, `"hello"`, `true` |
| `Variable(String)` | Identifier reference | `x` |
| `Binary { left, op, right }` | Binary operation | `a + b` |
| `Comparison { left, op, right }` | Comparison operation | `a < b` |
| `Reference(Box<Expression>)` | Address-of (`&`) | `&x` |
| `Dereference(Box<Expression>)` | Dereference (`*`) | `*ptr` |
| `UnaryNot(Box<Expression>)` | Logical NOT (`!`) | `!flag` |
| `BitwiseNot(Box<Expression>)` | Bitwise NOT (`~`) | `~mask` |
| `FunctionCall { name, args }` | Function call | `foo(1, 2)` |
| `Cast { expr, target_type }` | Type cast (`as`) | `x as i64` |
| `Alloc { alloc_type, count }` | Runtime-sized stack allocation (`count` is an arbitrary expression) | `i32[n]` |
| `ListInitializer(Vec<Expression>)` | Brace list | `{ 1, 2, 3 }` |
| `FieldAccess { base, field }` | Struct field read (auto-derefs a single-level pointer-to-struct) | `p.x` |
| `StructLiteral { struct_id, fields }` | Struct literal | `Point { x = 1, y = 2 }` |
| `FunctionRef(String)` | A function as a value, from a bare name or `&foo` | `&double` |
| `IndirectCall { callee, args }` | Call through a function-pointer value | `op(21)` |
| `SizeOf(LangType)` | Compile-time size, stamped `u64` | `sizeof(Point)` |
| `Null` | The untyped null pointer | `null` |
| `ValueBlock(Vec<Statement>)` | Block in expression position, valued by its inner `return` | `{ return 7 }` |

### LiteralValue

```rust
pub enum LiteralValue {
    Integer(i64),
    Float(f64),
    String(usize),  // Index into Program.string_literals
    Bool(bool),     // `true` / `false`
}
```

### BinaryOp

```rust
pub enum BinaryOp {
    // Arithmetic
    Add, Sub, Mul, Div, Mod,
    // Bitwise
    And, Or, Xor, LeftShift, RightShift,
    // Logical (short-circuit)
    LogicalAnd, LogicalOr,
}
```

### ComparisonOp

```rust
pub enum ComparisonOp {
    Greater, Less, GreaterEqual, LessEqual, Equal, NotEqual,
}
```

## Statement

```rust
pub struct Statement {
    pub kind: StatementKind,
    pub pos: Position,
}
```

### StatementKind Variants

| Variant | Description | Example |
|---------|-------------|---------|
| `Expression(Expression)` | Expression as statement | `foo()` |
| `Block(Vec<Statement>)` | Block scope | `{ ... }` |
| `Return(Option<Expression>)` | Return (optional value) | `return 42` |
| `If { condition, then_block, else_block }` | Conditional | `if x > 0 { ... }` |
| `While { condition, body }` | While loop | `while x < 10 { ... }` |
| `For { init, condition, increment, body }` | For loop | `for (i32 i = 0; i < 10; i += 1) { ... }` |
| `VarDecl { var_type, name, initializer }` | Variable declaration | `i32 x = 10` |
| `VarAssign { name, value }` | Variable assignment | `x = 20` |
| `DerefAssign { target, value }` | Dereference assignment | `*ptr = 42` or `arr[i] = val` |
| `FieldAssign { target, value }` | Struct-field assignment (`target` must be a field-access expression) | `p.x = 9` |
| `Break` | Break from loop | `break` |
| `Continue` | Continue loop | `continue` |

## Type System (`LangType`)

Defined in `src/lexer/tokens.rs` and re-exported by the parser:

```rust
pub struct LangType {
    pub base: TypeBase,          // SInt, UInt, SFloat, Void, Bool, Struct(id), or FnPtr(id)
    pub size_bits: u32,          // 8, 16, 32, 64 (0 for void)
    pub pointer_depth: u32,      // 0 = value, 1 = *, 2 = **
    pub is_const: bool,          // const qualifier
    pub array_size: Option<u32>, // Some(n) for arrays, None for non-arrays
}

pub enum TypeBase {
    SInt,    // Signed integer (i8, i16, i32, i64)
    UInt,    // Unsigned integer (u8, u16, u32, u64)
    SFloat,  // Floating point (f32, f64)
    Void,    // Void (u0)
    Bool,    // The Aspect boolean: an i1 logical value stored as i8
             // (`size_bits: 8` is the storage width). The type of
             // comparisons and `!`.
    Struct(u32), // A type-struct, identified by an interned id into the
                 // program's ModuleSymbols struct registry. The id, not the
                 // name, is stored so `LangType` stays Copy/Eq.
    FnPtr(u32),  // A function pointer, identified by an interned id into the
                 // function-signature registry. `fn(args) -> R` *is* the pointer.
}
```

### Key Methods

| Method | Description |
|--------|-------------|
| `langtype_from_str(s)` | Parse from string like `"i32"`, `"u64"`, `"f32"`, `"u0"` |
| `is_array()` | `true` if `array_size.is_some()` |
| `element_type()` | Strip array modifier |
| `decay_to_pointer()` | Array → pointer (`pointer_depth + 1`, clear `array_size`) |
| `with_const(bool)` | Builder pattern |
| `with_pointer_depth(u32)` | Builder pattern |
| `with_array_size(u32)` | Builder pattern |

## Type Inference During Parsing

The parser resolves expression types as it parses:

| Expression | Inferred Type |
|-----------|--------------|
| Integer literal (fits i32) | `i32` |
| Integer literal (doesn't fit i32) | `i64` |
| Float literal | `f64` |
| String literal | `u8*` (pointer_depth=1, base=UInt, size_bits=8) |
| Variable | Looked up from symbol table |
| Function call | Function's return type from symbol table |
| Comparison | `bool` — the parser transiently stamps `i32`, but the type checker overwrites it with `LangType::BOOL` (`TypeBase::Bool`, i1 logical value / i8 storage). It coerces implicitly into an integer target, so `i32 c = a < b` is legal; the comparison never propagates its result type into its operands |
| Logical NOT (`!`) | `bool` — same as a comparison, not `i32` |
| Binary op | Left operand's type — **except** when the left is a non-pointer and the right is a pointer, in which case the right operand's type wins, so `1 + ptr` is pointer-typed and scaled (commutative pointer arithmetic). See `parse_expr_prec` in `expressions.rs` |
| Cast | Target type |
| Alloc | Pointer to alloc_type (`pointer_depth + 1`) |
| Reference | Operand type with `pointer_depth + 1` |
| Dereference | Operand type with `pointer_depth - 1` |
| Array variable in expression | Decayed to pointer |

## Position

```rust
pub struct Position {
    pub line: usize,
    pub column: usize,
    /// Indexes into `Program::source_files`; entry file is 0.
    pub file_id: u32,
}
```

Every AST node carries a `Position` for error diagnostics. Build one with
`Position::new(line, column)` (file_id defaults to 0) or
`Position::with_file(line, column, file_id)` — the latter is what the lexer uses
for imported files.

Display format is `line:column`. Note `Display` deliberately omits the file: it
cannot reach the file registry, so callers wanting a filename must resolve
`file_id` against `Program::source_files` themselves. That is why the field is
easy to miss even though it appears in every AST dump.
