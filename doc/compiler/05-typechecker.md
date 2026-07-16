# Type Checker

The type checker (`src/typechecker/`) performs semantic validation after parsing.
It uses a **single-pass, bidirectional** approach: errors are emitted immediately
as statements and expressions are walked, and a target type is pushed *into* an
expression whenever the surrounding context supplies one.

The checker takes the AST by **mutable** reference and **stamps the resolved
`expr_type`** onto literal and arithmetic nodes as it goes, so codegen reads the
final type directly instead of re-deriving it. See
[Bidirectional Checking](#bidirectional-checking) below.

## Files

| File | Purpose |
|------|---------|
| `checker.rs` | `TypeChecker` struct — single-pass checking |
| `types.rs` | Pure helper functions for type coercibility and literal compatibility |
| `errors.rs` | `TypeCheckError` enum (15 variants) |

## TypeChecker Struct

```rust
pub struct TypeChecker {
    functions: HashMap<String, FunctionSig>,  // Known function signatures
    scopes: Vec<HashMap<String, LangType>>,   // Variable type scopes (stack)
    globals: HashMap<String, LangType>,       // Global variable types
    current_function: Option<String>,         // Current function being checked
    source_file: String,                      // Source file path for diagnostics
    errors: Vec<TypeCheckError>,              // Accumulated errors
}
```

The typechecker has its own **independent** scope system, separate from the parser's `SymbolTable`.

## Entry Points

```rust
// Basic construction
TypeChecker::new()

// Set source file for diagnostics (returns self for chaining)
TypeChecker::new().with_source_file(path: String)

// Run the checker; mutates the AST (stamps expr_type) and returns all errors at once
checker.check_program(&mut program) -> Result<(), Vec<TypeCheckError>>

// Format an error with source-file prefix
checker.format_error(&error) -> String
// Output: "path/to/file.ap:12:5: error: ..."
```

## Checking Phases

### Phase 1: Register Declarations

- Records all global variable types into `self.globals`
- Records all function signatures (param types + return type) into `self.functions`

### Phase 2: Single-Pass Statement/Expression Walk

Walks each function body in a single pass:
- Creates scopes for blocks, if/else branches, while bodies, for loops
- Adds parameters to scope
- For each statement, calls `check_statement()`; each expression is visited in
  one of the two modes described below (`synth_expression` or `check_expression`)
- Errors are pushed into a `Vec<TypeCheckError>` and all returned at the end

An exhaustive match on `FunctionBody` picks what a function even gets: an
`Extern` body has nothing to walk, and an `Asm` body has no statements —
it has a register contract, checked by `check_asm_function` instead.

### `asm fn` register validation (`check_asm_function`)

Validated here rather than in the parser because it depends on the
**compilation target**, which the parser doesn't know. The register model
lives in `src/asm.rs` — pure data, no LLVM, so it works for a target this
binary has no backend for (`rax` under `--target aarch64-*` must be a clean
error, not a silent accept).

Every collision check compares the register *family*, never the spelling:
`rax` and `eax` are one physical register, and LLVM diagnoses nothing if two
operands name it — it silently drops one. Rejected: unknown registers,
`rsp`/`rbp`, two operands in one family, a clobber that is also an operand,
and an operand register too narrow for its declared type.

## Bidirectional Checking

Every expression is visited in exactly one of two modes:

| Mode | Signature | Used when |
|------|-----------|-----------|
| **Synthesis** | `synth_expression(&mut Expression) -> LangType` | nothing constrains the type: conditions (`if`/`while`/`for`), the callee/index, cast and dereference operands, expression statements |
| **Checking** | `check_expression(&mut Expression, target: &LangType)` | the context supplies a target: declaration initialisers, assignment RHS, `return` value, function-call arguments, list-initialiser elements |

Checking mode **pushes the target down** into a child whenever the child's type
*is* the parent's type, and stamps `expr_type` on the way:

| `ExprKind` | check(target) behaviour |
|------------|-------------------------|
| `Literal(Integer)` | if `literal_int_fits(n, target)` → stamp `expr_type = target`; else `TypeMismatch` at the literal |
| `Literal(Float)` | if `literal_float_compatible(target)` → stamp `expr_type = target`; else `TypeMismatch` |
| `Literal(String)` | type is fixed; assert coercible to `target` |
| `Binary` (numeric target) | check **both** operands against `target`; stamp result `= target` |
| `BitwiseNot` | check operand against `target`; stamp result `= target` |
| `Reference` | check inner against `target` with `pointer_depth - 1` |
| `ListInitializer` | decay `target` to its element type; check every element against it |
| `Comparison`, `UnaryNot`, `Cast`, `FunctionCall`, `Variable`, `Alloc`, `Dereference`, `Binary` (pointer target) | synthesise, then assert the result is coercible to `target` |

**Propagation rule of thumb**: propagate the target into a child when the
operator preserves the type (arithmetic, bitwise-not, reference, list-init
elements). Do *not* propagate when the operator changes the type (comparison,
unary-not, cast, function call).

Because literals are stamped at their final width during checking, a constant
like `u8 x = 1 + 2` arrives at codegen already typed `u8` — codegen emits `i8`
arithmetic directly instead of computing in `i32` and truncating.

### Narrow-width comparisons

Comparisons run in synthesis mode (their result is always `bool`, never the
operands' type), so the target-propagation above does not apply. But a single
local refinement still pays off: when one operand is an integer literal that
*fits* the other operand's concrete integer type, the literal adopts that type
(`narrow_literal_to_sibling`). So `u8 i; ... i < 10` compares at `i8` instead of
zero-extending `i` to `i32` to match the literal's default width. This is safe
because the literal fits the sibling's exact type, so the boolean result is
unchanged — only the emitted comparison width differs. It is *not* applied to
arithmetic operands in synthesis position, where changing the width would change
the computed value (e.g. an index `arr[i + 1]`).

## Type Helpers (`types.rs`)

All functions are pure (no side effects):

| Function | Purpose |
|----------|---------|
| `types_coercible(from, to)` | Returns `true` if `from` can be implicitly coerced to `to` |
| `literal_int_fits(val: i64, to)` | Returns `true` if integer `val` fits in type `to` |
| `literal_float_compatible(to)` | Returns `true` if type `to` can hold a float literal |
| `cast_valid(from, to)` | Returns `true` if explicit `as` cast is valid |

### Coercibility Rules (`types_coercible`)

1. **Exact match** → `true`
2. **Array-to-pointer decay**: `i32[10]` is coercible to `i32*`
3. **Void values**: `u0` (pointer depth 0 after decay) is only compatible with `u0`.
   `u0*` is an ordinary pointer for coercion purposes and follows the pointer
   rules below — so `T* <-> u0*` is implicit in both directions. What makes
   `u0*` special is *use*, not assignment: dereferencing/subscripting it and
   pointer arithmetic on it are rejected (`OpaqueDereference`, invalid binary
   op) until it is cast to a sized pointer type.
4. **Pointer depth mismatch** (after decay) → `false`
5. **Integer widening** (non-pointer, non-array): `size_bits(from) <= size_bits(to)` → `true`
   - Both `SInt↔UInt` families are treated as integers here (widening is allowed even across sign)
6. **Float widening**: `from.size_bits <= to.size_bits` AND both `SFloat` → `true`
7. **Int ↔ Float cross-family** → `false` (requires explicit `as` cast)

### Literal Compatibility

- Integer literals: checked by **value** — `literal_int_fits(val, to)` passes if the value
  fits in the target type's range (signed or unsigned), regardless of the literal's parser-assigned
  type. Example: `42` fits in `u8`, `i8`, `i16`, `u16`, etc.
- Float literals: pass for any `SFloat` target type (`literal_float_compatible`)

This means `u8 x = 255` is valid but `u8 x = 256` is a type error at compile time.

### Cast Rules (`cast_valid`)

| Cast | Valid? |
|------|--------|
| Pointer ↔ Integer (SInt/UInt) | Yes |
| Float ↔ Pointer | No |
| Pointer → Pointer (any depth) | Yes |
| Integer → Float | Yes (explicit) |
| Float → Integer | Yes (explicit) |
| Integer → Integer | Yes |

## Error Diagnostics

Errors include position information. The `format_error()` method prepends the source file path:

```
src/main.ap:12:5: Type mismatch: expected 'u8' but found 'i32' at 12:5
```

The `TypeCheckError::position()` method returns the `Option<Position>` for each variant
(returns `None` for `MissingReturn`, which has no source location).

## Scope Management

| Method | Behavior |
|--------|----------|
| `enter_scope()` | Push new `HashMap` |
| `exit_scope()` | Pop top `HashMap` |
| `define_var(name, type)` | Insert into innermost scope |
| `lookup_var(name)` | Search scopes innermost→outermost, then globals |

Scopes are created for: function bodies, if/else blocks, while bodies, for loops
(single scope wrapping init+condition+increment+body), standalone blocks.

## Error Handling

Returns `Result<(), Vec<TypeCheckError>>` — collects **all** errors before reporting.

| Error Variant | Trigger |
|--------------|---------|
| `TypeMismatch` | Expected/found type mismatch |
| `UndefinedVariable` | Variable not in any scope |
| `UndefinedFunction` | Function not found |
| `InvalidBinaryOperation` | Binary op on incompatible types |
| `InvalidUnaryOperation` | Unary op on void type |
| `ArgumentCountMismatch` | Wrong number of function arguments |
| `ArgumentTypeMismatch` | Argument type incompatible with parameter |
| `InvalidDereference` | Dereferencing non-pointer |
| `InvalidReference` | Taking address of non-lvalue |
| `ReturnTypeMismatch` | Return type incompatible with function signature |
| `MissingReturn` | Non-void function has no return path |
| `InvalidConditionType` | Condition is void non-pointer |
| `InvalidCast` | Invalid cast operation |
| `AssignmentToConst` | Assigning to const variable |
| `AssignmentTypeMismatch` | RHS type not coercible to LHS |
| `ListInitLengthMismatch` | Too many elements in list initializer |

