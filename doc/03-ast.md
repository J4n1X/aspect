# AST Reference

The AST types are defined in `src/parser/ast.rs`. The parser produces a `Program` containing functions, global variables, and a string literal table.

## Program

```rust
pub struct Program {
    pub functions: Vec<Function>,
    pub global_vars: Vec<GlobalVar>,
    pub string_literals: Vec<String>,
}
```

## Function

```rust
pub struct Function {
    pub proto: FunctionProto,
    pub body: Vec<Statement>,
}

pub struct FunctionProto {
    pub name: String,
    pub params: Vec<(LangType, String)>,  // (type, name) pairs
    pub return_type: LangType,
    pub is_extern: bool,
    pub pos: Position,
}
```

## GlobalVar

```rust
pub struct GlobalVar {
    pub var_type: LangType,
    pub name: String,
    pub initializer: Option<Expression>,
    pub pos: Position,
}
```

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
| `Literal(LiteralValue)` | Integer, float, or string literal | `42`, `3.14`, `"hello"` |
| `Variable(String)` | Identifier reference | `x` |
| `Binary { left, op, right }` | Binary operation | `a + b` |
| `Comparison { left, op, right }` | Comparison operation | `a < b` |
| `Reference(Box<Expression>)` | Address-of (`&`) | `&x` |
| `Dereference(Box<Expression>)` | Dereference (`*`) | `*ptr` |
| `UnaryNot(Box<Expression>)` | Logical NOT (`!`) | `!flag` |
| `BitwiseNot(Box<Expression>)` | Bitwise NOT (`~`) | `~mask` |
| `FunctionCall { name, args }` | Function call | `foo(1, 2)` |
| `Cast { expr, target_type }` | Type cast (`as`) | `x as i64` |
| `Alloc { alloc_type, count }` | Array allocation | `i32[10]` |

### LiteralValue

```rust
pub enum LiteralValue {
    Integer(i64),
    Float(f64),
    String(usize),  // Index into Program.string_literals
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
| `Break` | Break from loop | `break` |
| `Continue` | Continue loop | `continue` |

## Type System (`LangType`)

Defined in `src/lexer/tokens.rs` and re-exported by the parser:

```rust
pub struct LangType {
    pub base: TypeBase,          // SInt, UInt, SFloat, or Void
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
| Comparison | `i32` (boolean as integer) |
| Logical NOT (`!`) | `i32` (boolean as integer) |
| Binary op | Left operand's type |
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
}
```

Every AST node carries a `Position` for error diagnostics. Display format: `line:column`.
