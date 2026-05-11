# Type Checker

The type checker (`src/typechecker/`) performs semantic validation after parsing. It uses a **constraint-based** approach: collect constraints in one pass, verify them all in another.

## Files

| File | Purpose |
|------|---------|
| `checker.rs` | `TypeChecker` struct — three-phase checking |
| `types.rs` | `TypeConstraint` enum (12 variants), `TypeExpr`, `ConstraintContext` |
| `errors.rs` | `TypeCheckError` enum (13+ variants) |

## TypeChecker Struct

```rust
pub struct TypeChecker {
    functions: HashMap<String, FunctionSig>,  // Known function signatures
    scopes: Vec<HashMap<String, LangType>>,   // Variable type scopes (stack)
    globals: HashMap<String, LangType>,       // Global variable types
    constraints: Vec<TypeConstraint>,          // Collected constraints
    current_function: Option<String>,          // Current function being checked
}
```

The typechecker has its own **independent** scope system, separate from the parser's `SymbolTable`.

## Three-Phase Checking

### Phase 1: Register Declarations (`register_declarations`)

- Records all global variable types into `self.globals`
- Records all function signatures (param types + return type) into `self.functions`

### Phase 2: Collect Constraints (`collect_function_constraints`)

Walks each function body:
- Creates scopes for blocks, if/else branches, while bodies, for loops
- Adds parameters to scope
- For each statement/expression, resolves types and pushes `TypeConstraint` entries into `self.constraints`

### Phase 3: Verify Constraints (`verify_constraints`)

Iterates all collected constraints and checks each one via `verify_constraint()`. Errors are collected into `Vec<TypeCheckError>` and returned all at once (not fatal on first error).

## Entry Point

```rust
TypeChecker::new().check_program(&program) -> Result<(), Vec<TypeCheckError>>
```

## Constraint Types

| Constraint | Verification Rule |
|-----------|-------------------|
| `Equal { expected, found, context }` | Strict type equality |
| `Compatible { expected, found, context }` | `types_compatible()` — allows implicit conversions |
| `BinaryOp { left, right, pos }` | `types_compatible(left, right)` OR pointer arithmetic (`ptr ± int`) |
| `UnaryOp { operand, pos }` | Operand must not be `Void` |
| `Dereference { operand, pos }` | Must have `pointer_depth > 0` |
| `Reference { operand, pos }` | Always passes |
| `FunctionCall { name, expected_args, found_args, pos }` | Arg count match + each arg `types_compatible` with param |
| `Return { expected, found, pos }` | `types_compatible` with function signature |
| `Cast { from, to, pos }` | `cast_valid()` — nearly everything is valid |
| `Condition { operand, pos }` | Must not be `void` non-pointer |
| `AssignmentToConst { name, pos }` | Always fails |

## Type Compatibility Rules

`types_compatible(expected, actual)` implements these rules:

1. **Exact match** → compatible
2. **Array-to-pointer decay**: if either type is an array, decay it (`pointer_depth + 1`, clear `array_size`), then check match. E.g., `i32[10]` is compatible with `i32*`.
3. **Void rule**: void is only compatible with void
4. **Pointer depth mismatch** (after decay) → incompatible
5. **Numeric implicit conversion** (non-pointer, non-array):
   - `SInt ↔ UInt` (any bit width) → compatible
   - `SFloat ↔ SFloat` (any bit width) → compatible
   - `SInt/UInt ↔ SFloat` → **NOT** compatible (requires explicit cast)

## Cast Rules

`cast_valid(from, to)`:

| Cast | Valid? |
|------|--------|
| Pointer ↔ Integer (SInt/UInt) | Yes |
| Float ↔ Pointer | No |
| Pointer → Pointer (any depth) | Yes |
| Integer → Float | Yes (explicit) |
| Float → Integer | Yes (explicit) |
| Integer → Integer | Yes |

## Constraint Context

`ConstraintContext` provides context-specific error messages:

| Context | Display |
|---------|---------|
| `Assignment` | "assignment" |
| `Initialization` | "initialization" |
| `Return` | "return statement" |
| `Argument { func_name, arg_index }` | "argument N of function 'name'" |
| `Comparison` | "comparison" |
| `Arithmetic` | "arithmetic operation" |

Note: context is stored but currently discarded during verification (available for future improvements).

## Scope Management

The typechecker's scope system:

| Method | Behavior |
|--------|----------|
| `enter_scope()` | Push new `HashMap` |
| `exit_scope()` | Pop top `HashMap` |
| `define_var(name, type)` | Insert into innermost scope |
| `lookup_var(name)` | Search scopes innermost→outermost, then globals |

Scopes are created for: function bodies, if/else blocks, while bodies, for loops (single scope wrapping init+condition+increment+body), standalone blocks.

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
| `ReturnTypeMismatch` | Return type incompatible with function signature |
| `InvalidConditionType` | Condition is void non-pointer |
| `InvalidCast` | Invalid cast operation |
| `AssignmentToConst` | Assigning to const variable |
