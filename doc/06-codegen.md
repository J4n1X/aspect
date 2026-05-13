# Code Generation

The codegen module (`src/codegen/`) emits LLVM IR via Inkwell (pinned to LLVM 19.1).

## Files

| File | Purpose |
|------|---------|
| `generator.rs` | `CodeGenerator` struct — all IR emission logic |
| `types.rs` | `LangType` → LLVM type translation + operation helpers |
| `errors.rs` | `CodegenError` enum |

## CodeGenerator Struct

```rust
pub struct CodeGenerator<'ctx> {
    context: &'ctx Context,              // LLVM Context (must outlive everything)
    module: Module<'ctx>,                // LLVM Module (translation unit)
    builder: Builder<'ctx>,              // IRBuilder (emits at insertion point)
    target: Target,                      // LLVM Target for native platform

    functions: HashMap<String, FunctionValue<'ctx>>,  // Forward reference table
    function_lang_params: HashMap<String, Vec<LangType>>,  // Param types for coercion
    variables: Vec<HashMap<String, LocalVar<'ctx>>>,  // Scoped local variable stack
    global_variables: HashMap<String, GlobalVarInfo<'ctx>>,  // Globals + string literals

    current_function: Option<FunctionValue<'ctx>>,    // Current function being emitted
    current_function_return_type: Option<LangType>,   // Return type for coercion in return stmts
    loop_stack: Vec<(BasicBlock<'ctx>, BasicBlock<'ctx>)>,  // (break_bb, continue_bb) per loop
}
```

### Helper Structs

- `LocalVar<'ctx>` — `ptr: PointerValue`, `llvm_type: BasicTypeEnum`, `lang_type: LangType`, `const_value: Option<BasicValueEnum>` (folded constant for `const` locals)
- `GlobalVarInfo<'ctx>` — `ptr: PointerValue`, `llvm_type: BasicTypeEnum`, `lang_type: LangType`

## Type Translation (`types.rs`)

### Conversion Functions

| Function | Behavior |
|----------|----------|
| `lang_type_to_llvm(ctx, lang_type)` | Main conversion. Pointers and arrays → `ptr`. SInt/UInt → `iN`. SFloat → `fN`. Void → error. |
| `is_void_type(lang_type)` | `true` when `base == Void && pointer_depth == 0` |
| `get_int_type(ctx, bits)` | Bits → `i8`/`i16`/`i32`/`i64` |
| `get_float_type(ctx, bits)` | Bits → `f32`/`f64` |
| `lang_type_element_to_llvm(ctx, lang_type)` | Strip pointer/array, return base scalar type |
| `lang_type_to_llvm_array(ctx, lang_type)` | Return `[N x elem]` for statically-sized arrays |

**Key**: Signed vs unsigned makes no difference at the LLVM type level — both map to the same `iN`. Signedness is tracked by `LangType::base` and consulted at instruction selection time.

### Operation Helpers

| Helper | Purpose |
|--------|---------|
| `signed_op!(builder, is_signed, signed_method, unsigned_method, args...)` | Macro: dispatch to signed or unsigned variant of a builder method |
| `widen_ints_to_match(builder, a, a_signed, b, b_signed)` | Widen the narrower integer value using `sext`/`zext` |
| `widen_floats_to_match(builder, a, b)` | Widen the narrower float value using `fpext` |
| `int_cmp_pred(op, is_signed)` | Return `IntPredicate` for comparison op + signedness |
| `float_cmp_pred(op)` | Return ordered `FloatPredicate` for comparison op |

These helpers isolate all sign-aware LLVM decisions in one place so the calling code
does not need to repeat `if is_signed { ... } else { ... }` branches.

## Two-Pass Function Compilation

### Pass 1: Declaration (`declare_function`)

For each function in the program:
1. Collect parameter `LangType`s into `function_lang_params` for call-site coercion
2. Convert parameter and return types to LLVM types
3. Create `fn_type` (non-variadic)
4. `module.add_function()` — adds to module
5. Set parameter names
6. Store `FunctionValue` in `self.functions`

### Pass 2: Body Generation (`generate_function`)

For each non-extern function:
1. Set `current_function_return_type` to the function's return type
2. Create `entry` basic block, position builder
3. Enter new scope
4. For each parameter: `alloca` in entry block, `store` LLVM param value, add to variable scope
5. Emit all body statements
6. If current block lacks terminator: `ret void` or `ret <zero>`
7. Exit scope; clear `current_function_return_type`

## Expression Codegen

`generate_expression` dispatches on `ExprKind`:

### Value Coercion (`generate_coerced_value`)

All code paths that generate a value destined for a known target type (var-decl initializer,
var-assign RHS, return value, function arguments, array element initializers) go through:

```rust
fn generate_coerced_value(expr: &Expression, target: Option<&LangType>) -> BasicValueEnum
```

Behaviour:
- If `expr` is a numeric literal and `target` is a scalar type → use `generate_literal_typed`
  (overflow-checked, directly typed to target)
- Otherwise → generate normally, then auto-widen via `cast_value` if types differ

### Literals

- **Typed path** (`generate_literal_typed`): Checks for integer overflow against target type's
  bit width and range. Used in all coercion paths.
- **Default path** (`generate_literal`): Used inside `generate_expression` (e.g. in binary op
  operands where no outer target type is known); generates with the expression's own type.
- **Constant path** (`generate_constant_literal`): For global initializers (no builder usage).

### Variables
- Looks up in scoped `variables` stack (innermost first), falls back to `global_variables`
- **Const locals**: if `const_value` is `Some`, returns the folded value directly (no `load` emitted)
- Arrays return pointer directly (no load — array-to-pointer decay)
- Scalars emit `build_load` with explicit pointee type

### Binary Operations

Dispatches to specialized methods based on left operand type:

| Method | Type | Operations |
|--------|------|-----------|
| `generate_int_binary_op` | SInt/UInt | Add, Sub, Mul, `signed_op!(Div)`, `signed_op!(Mod)`, And, Or, Xor, Shift, LogicalAnd, LogicalOr |
| `generate_float_binary_op` | SFloat | Add, Sub, Mul, Div (with `widen_floats_to_match`) |
| `generate_pointer_binary_op` | Pointer | Add (GEP), Sub (neg + GEP) |

**Implicit width matching**: If operands have different bit widths, `widen_ints_to_match` /
`widen_floats_to_match` silently widens the narrower operand. No warnings are emitted.

**Logical And/Or**: NOT short-circuit — uses `select` instruction. Both operands always evaluated.

### Comparisons
- Uses `widen_ints_to_match` / `widen_floats_to_match` before comparing
- Uses `int_cmp_pred(op, is_signed)` and `float_cmp_pred(op)` for predicate selection
- Result is an `i1` value emitted directly by `icmp`/`fcmp` — **no `zext` to `i32`**
- The `i1` feeds `br` directly in conditionals; when stored to an integer variable it is
  widened via `zext` (always unsigned-extend for booleans, regardless of target type)

### Casts (`cast_value`)

| Source → Target | LLVM Instruction |
|-----------------|-----------------|
| int → pointer | `inttoptr` |
| pointer → pointer | `pointer_cast` |
| int → float (signed) | `sitofp` |
| int → float (unsigned) | `uitofp` |
| float → int (signed target) | `fptosi` |
| float → int (unsigned target) | `fptoui` |
| pointer → int | `ptrtoint` |
| int → int (wider, `i1` source) | always `zext` (boolean — never sign-extend) |
| int → int (wider, other) | `sext` or `zext` (source signedness) |
| int → int (narrower) | `trunc` |
| Same type | No-op |

### Unary Operations
- `!expr` → `val == 0` → `i1`, then `zext` to `i32` (logical NOT always yields `i32`)
- `~expr` → `build_not`

### Allocation
- **Global scope**: Constant count only. Creates `[N x type]` global with zero initializer.
- **Local scope**: `build_array_alloca(type, count)`

## Statement Codegen

| Statement | Handler |
|-----------|---------|
| `Expression(expr)` | `generate_expression_statement` (special-cases void function calls) |
| `VarDecl` | `generate_var_decl` — alloca in entry block; for `const` vars tries `try_fold_constant_expression` first; falls back to `generate_coerced_value` |
| `VarAssign` | `generate_var_assign` — lookup + `generate_coerced_value` |
| `DerefAssign` | `generate_deref_assign` — evaluate pointer + store |
| `Return` | `generate_return` — `generate_coerced_value` with `current_function_return_type` |
| `If` | `generate_if_statement` — condition→i1, then/else/ifcont blocks |
| `While` | `generate_while_loop` — cond/body/end blocks |
| `For` | `generate_for_loop` — init/cond/body/inc/end blocks |
| `Block` | `generate_block` — enter scope, emit statements, exit scope |
| `Break` | Branch to loop's break BB, create dead block |
| `Continue` | Branch to loop's continue BB, create dead block |

## Control Flow

### If/Else
1. Condition → `i1` via `value_to_bool`
2. Create blocks: `then`, `else`, `ifcont`
3. `br i1 %cond, then, else`
4. Emit then/else statements; branch to `ifcont` if no terminator
5. Position builder at `ifcont`

### While Loop
1. Create blocks: `while.cond`, `while.body`, `while.end`
2. Push `(end_bb, cond_bb)` to `loop_stack`
3. Condition → `i1`, branch to body or end
4. Body statements; branch to cond if no terminator
5. Pop loop stack, position at `while.end`

### For Loop
1. Enter scope, emit init
2. Create blocks: `for.cond`, `for.body`, `for.inc`, `for.end`
3. Push `(end_bb, inc_bb)` to `loop_stack`
4. Condition (or `true` if absent), branch to body or end
5. Body statements; branch to inc if no terminator
6. Increment statement; branch to cond
7. Pop loop stack, position at `for.end`, exit scope

### Break / Continue
- Branch to appropriate target from `loop_stack`
- Create **dead basic block** to prevent "instruction after terminator" panics — LLVM's `dce` pass eliminates unreachable code

## Memory Management

### Alloca Placement Strategy

All `alloca` instructions are placed in the **function entry block**, not at the declaration site. The builder is temporarily repositioned to the entry block, the alloca is emitted, then the builder is restored. This is critical for LLVM's `mem2reg` pass.

### Loads and Stores
- All variable reads: `build_load(llvm_type, ptr, name)` with explicit pointee type
- All variable writes: `build_store(ptr, value)`
- Array variables: return pointer directly (no load)

## String Literals

1. Create global `.str.{index}` of type `[N+1 x i8]` (null-terminated)
2. Set as constant
3. Register in `global_variables` with `lang_type = u8*`
4. In expressions: pointer cast to `i8*`

## Global Variables and Constant Expressions

1. Compute LLVM type (arrays → `lang_type_to_llvm_array`)
2. `module.add_global()`
3. For **array** initializers: `generate_constant_array_value` → LLVM `ConstantArray` (all elements must be literals)
4. For **scalar** initializers: `generate_constant_expression` (see below)
5. For no initializer: `const_zero()`
6. Register in `global_variables`

### `generate_constant_expression`

Evaluates a scalar global initializer to an LLVM constant *without emitting any IR builder calls*.
Supports:

| `ExprKind` | Behaviour |
|------------|----------|
| `Literal` | Delegates to `generate_constant_literal` |
| `Alloc` | Delegates to `generate_alloc` (count must be a constant) |
| `Variable(name)` | Looks up the LLVM global by name and returns its initializer |
| `Reference(Variable)` | Returns the global's pointer value |
| `Binary` | Delegates to `const_int_binary_op` or `const_float_binary_op` |
| `BitwiseNot` | `IntValue::const_not()` |
| `UnaryNot` | Extracts as `u64`, computes `== 0`, reconstructs as `i32` |
| `Cast` | Delegates to `const_cast_value` |

### Constant Arithmetic Helpers (LLVM 19)

LLVM 19 removed almost all `LLVMConst*` arithmetic functions. All constant integer and float
arithmetic is therefore performed in Rust and the results are reconstructed as LLVM constants.

| Helper | Purpose |
|--------|---------|
| `const_int_binary_op` | Extracts both operands as `u64`, performs the op in Rust (`wrapping_add`, etc.), returns `IntType::const_int(result, signed)` |
| `const_float_binary_op` | Extracts both operands via `FloatValue::get_constant()`, performs op in Rust, returns `FloatType::const_float(result)` |
| `const_cast_value` | Cast between any two constant types; widening uses Rust extraction + `IntType::const_int`; truncation uses `IntValue::const_truncate`; int↔float via `get_sign_extended_constant` / `get_constant` + `FloatType::const_float` |

In `types.rs`, `const_widen_ints_to_match` widens two `IntValue` constants to the same bit-width
using Rust extraction (not `const_s_extend`/`const_z_ext`, which were removed in LLVM 18).

### `try_fold_constant_expression`

A side-effect-free variant used for **`const` local variable initializers**.
Returns `Option<BasicValueEnum>` — `None` means the expression is dynamic (function call, non-const local, etc.).

Folding is attempted for:
- `Literal` — always folds
- `Variable(name)` — folds if the referenced variable is a previously-folded `const` local or a global with a known initializer
- `Binary`, `BitwiseNot`, `UnaryNot`, `Cast` — folds if all sub-expressions fold
- `Reference(Variable)` — folds to the alloca pointer of a local or the global ptr

If `try_fold_constant_expression` succeeds, `generate_var_decl` stores the constant to the alloca
and records it in `LocalVar::const_value`. Subsequent reads of that variable (via `generate_expression`)
return the constant directly, bypassing the `load` instruction.

## List Initializers

`generate_list_initializer(array_ptr, var_type, elements)` stores values into an already-allocated array pointer.

### Constant path (all elements are integer or float literals)

If every element is a literal, `generate_constant_array_value` builds an LLVM `ConstantArray` in one call and stores it with a single `build_store`. This is more efficient and produces better LLVM IR.

```rust
// Int elements
let vals: Vec<IntValue> = ...;
int_ty.const_array(&vals)   // → [N x iM] ConstantArray

// Float elements
let vals: Vec<FloatValue> = ...;
float_ty.const_array(&vals) // → [N x fM] ConstantArray
```

Remaining slots (when fewer literals than array size) are zero-padded automatically.

### Runtime path (any element is a non-literal expression)

Each element is computed individually and stored via two-index GEP. The two-index form correctly dereferences a `[N x elem]*` array pointer:

```
// [0, i] → ptr[0][i] → the i-th element of the array
gep([N x elem], array_ptr, [i64 0, i64 i])
```

Remaining slots are zero-filled with the same two-index pattern.

### Global array initializers

Global arrays must use the constant path (all-literal). `generate_constant_array_value` is called directly from `generate_global_variable` and the result passed to `global_var.set_initializer()`.

## Optimization

```rust
codegen.optimize(level: u8) -> Result<(), CodegenError>
```

| Level | Pipeline |
|-------|---------|
| 0 | No-op |
| 1 | `default<O1>` |
| 2+ | `default<O2>` |
| 3 | `default<O3>` |

Options: `verify_each(true)`, `loop_interleaving(true)`, `loop_vectorization(true)`, `loop_unrolling(true)`, `merge_functions(true)`.

## Critical Gotchas

1. **Entry-block alloca hoisting**: All allocas must be in the entry block for `mem2reg`.
2. **Signedness is instruction-level**: Consult `LangType::base` at every operation; use `signed_op!` macro.
3. **Opaque pointers**: `build_load` requires explicit pointee type.
4. **Dead blocks after break/continue**: Prevent panics from emitting after terminators.
5. **Implicit width matching**: Auto-widens narrower operands silently (no warnings).
6. **Logical And/Or are NOT short-circuit**: Both operands always evaluated.
7. **Context-aware literals via `generate_coerced_value`**: All assignment/call/return paths use a single helper that generates typed literals with overflow checking.
8. **Two-pass functions**: Enables forward references.
9. **Array-to-pointer decay**: Arrays never load — return pointer directly.
10. **Pointer arithmetic via GEP**: `build_gep` with correct element type for stride calculation.
11. **Implicit return**: Functions without explicit return get `ret void` or `ret <zero>`.
12. **LLVM 19 const API is sparse**: Only `const_not`, `const_add/sub/mul`, `const_xor`, `const_truncate`, `const_to_pointer`, `PointerValue::const_to_int` survive. All others (`const_and/or`, `const_shl/ashr`, `const_signed_div`, `const_s_extend/z_ext`, `const_int_compare`, float arithmetic) were removed in LLVM 15–18. Use Rust-level arithmetic + `IntType::const_int` / `FloatType::const_float` for constant folding.
13. **`const` locals are compile-time folded**: If a `const` variable's initializer is a statically-computable expression, it is folded at codegen time and reads bypass the alloca/load.
