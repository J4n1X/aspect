# Type Checker

The type checker (`src/typechecker/`) performs semantic validation after parsing.
It uses a **single-pass** approach: errors are emitted immediately as statements
and expressions are walked.

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
    source_file: String,                      // Source file path for diagnostics
    current_function: Option<String>,          // Current function being checked
}
```

The typechecker has its own **independent** scope system, separate from the parser's `SymbolTable`.

## Entry Points

```rust
// Basic construction
TypeChecker::new()

// Set source file for diagnostics (returns self for chaining)
TypeChecker::new().with_source_file(path: String)

// Run the checker; returns all errors at once
checker.check_program(&program) -> Result<(), Vec<TypeCheckError>>

// Format an error with source-file prefix
checker.format_error(&error) -> String
// Output: "path/to/file.tjlb:12:5: error: ..."
```

## Checking Phases

### Phase 1: Register Declarations

- Records all global variable types into `self.globals`
- Records all function signatures (param types + return type) into `self.functions`

### Phase 2: Single-Pass Statement/Expression Walk

Walks each function body in a single pass:
- Creates scopes for blocks, if/else branches, while bodies, for loops
- Adds parameters to scope
- For each statement, calls `check_statement()`; for each expression, calls `check_expression()`
- Errors are pushed into a `Vec<TypeCheckError>` and all returned at the end

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
3. **Void**: only compatible with void
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
src/main.tjlb:12:5: Type mismatch: expected 'u8' but found 'i32' at 12:5
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

