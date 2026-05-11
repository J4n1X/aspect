# Code Generation

The codegen module (`src/codegen/`) emits LLVM IR via Inkwell (pinned to LLVM 19.1).

## Files

| File | Purpose |
|------|---------|
| `generator.rs` | `CodeGenerator` struct — all IR emission logic (~1600 lines) |
| `types.rs` | `LangType` → LLVM type translation functions |
| `errors.rs` | `CodegenError` enum |

## CodeGenerator Struct

```rust
pub struct CodeGenerator<'ctx> {
    context: &'ctx Context,              // LLVM Context (must outlive everything)
    module: Module<'ctx>,                // LLVM Module (translation unit)
    builder: Builder<'ctx>,              // IRBuilder (emits at insertion point)
    target: Target,                      // LLVM Target for native platform

    functions: HashMap<String, FunctionValue<'ctx>>,  // Forward reference table
    variables: Vec<HashMap<String, LocalVar<'ctx>>>,  // Scoped local variable stack
    global_variables: HashMap<String, GlobalVarInfo<'ctx>>,  // Globals + string literals

    current_function: Option<FunctionValue<'ctx>>,    // Current function being emitted
    loop_stack: Vec<(BasicBlock<'ctx>, BasicBlock<'ctx>)>,  // (break_bb, continue_bb) per loop
}
```

### Helper Structs

- `LocalVar<'ctx>` — `ptr: PointerValue`, `llvm_type: BasicTypeEnum`, `lang_type: LangType`
- `GlobalVarInfo<'ctx>` — same triple for globals and string literals

## Type Translation (`types.rs`)

| Function | Behavior |
|----------|----------|
| `lang_type_to_llvm(ctx, lang_type)` | Main conversion. Pointers and arrays → `ptr`. SInt/UInt → `iN`. SFloat → `fN`. Void → error. |
| `is_void_type(lang_type)` | `true` when `base == Void && pointer_depth == 0` |
| `get_int_type(ctx, bits)` | Bits → `i8`/`i16`/`i32`/`i64` |
| `get_float_type(ctx, bits)` | Bits → `f32`/`f64` |
| `lang_type_element_to_llvm(ctx, lang_type)` | Strip pointer/array, return base scalar type |
| `lang_type_to_llvm_array(ctx, lang_type)` | Return `[N x elem]` for statically-sized arrays |

**Key**: Signed vs unsigned makes no difference at the LLVM type level — both map to the same `iN`. Signedness is tracked by `LangType::base` and consulted at instruction selection time.

## Two-Pass Function Compilation

### Pass 1: Declaration (`declare_function`)

For each function in the program:
1. Convert parameter and return types to LLVM types
2. Create `fn_type` (non-variadic)
3. `module.add_function()` — adds to module
4. Set parameter names
5. Store in `self.functions` for forward references

### Pass 2: Body Generation (`generate_function`)

For each non-extern function:
1. Create `entry` basic block, position builder
2. Enter new scope
3. For each parameter: `alloca` in entry block, `store` LLVM param value, add to variable scope
4. Emit all body statements
5. If current block lacks terminator: `ret void` or `ret <zero>`
6. Exit scope

## Expression Codegen

`generate_expression` dispatches on `ExprKind`:

### Literals
- **Default path** (`generate_literal`): `const_int()`, `const_float()`, or string global pointer cast
- **Typed path** (`generate_literal_typed`): Used in var-decl/assign contexts. Checks for overflow against target type's bit width.
- **Constant path** (`generate_constant_literal`): For global initializers, no builder usage

### Variables
- Looks up in scoped `variables` stack (innermost first), falls back to `global_variables`
- Arrays return pointer directly (no load — array-to-pointer decay)
- Scalars emit `build_load` with explicit pointee type

### Binary Operations

Dispatches to specialized methods based on left operand type:

| Method | Type | Operations |
|--------|------|-----------|
| `generate_int_binary_op` | SInt/UInt | Add, Sub, Mul, Div (`sdiv`/`udiv`), Mod (`srem`/`urem`), And, Or, Xor, Shift (`ashr`/`lshr`), LogicalAnd, LogicalOr |
| `generate_float_binary_op` | SFloat | Add, Sub, Mul, Div |
| `generate_pointer_binary_op` | Pointer | Add (GEP), Sub (neg + GEP) |

**Implicit width matching**: If operands have different bit widths, the narrower one is widened via `sext`/`zext` (ints) or `fpext` (floats), with `eprintln!` warnings to stderr.

**Logical And/Or**: NOT short-circuit — uses `select` instruction. Both operands are always evaluated.

### Comparisons
- Float: `FloatPredicate::OEQ/ONE/OLT/OGT/OLE/OGE`, result `zext`'d to `i32`
- Int: `IntPredicate::EQ/NE/SLT|ULT/SGT|UGT/SLE|ULE/SGE|UGE` based on signedness, result `zext`'d to `i32`

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
| int → int (wider) | `sext` or `zext` (source signedness) |
| int → int (narrower) | `trunc` |
| Same type | No-op |

### Unary Operations
- `!expr` → `val == 0` → `i1`, then `zext` to `i32`
- `~expr` → `build_not`

### Allocation
- **Global scope**: Constant count only. Creates `[N x type]` global with zero initializer.
- **Local scope**: `build_array_alloca(type, count)`

## Statement Codegen

| Statement | Handler |
|-----------|---------|
| `Expression(expr)` | `generate_expression_statement` (special-cases void function calls) |
| `VarDecl` | `generate_var_decl` — alloca in entry block, optional initializer |
| `VarAssign` | `generate_var_assign` — lookup + store |
| `DerefAssign` | `generate_deref_assign` — evaluate pointer + store |
| `Return` | `generate_return` — evaluate + `build_return` |
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

## Global Variables

1. Compute LLVM type (arrays → `lang_type_to_llvm_array`)
2. `module.add_global()`
3. Evaluate initializer via `generate_constant_expression` (limited to literals and alloc) or `const_zero()`
4. Register in `global_variables`

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
2. **Signedness is instruction-level**: Consult `LangType::base` at every operation.
3. **Opaque pointers**: `build_load` requires explicit pointee type.
4. **Dead blocks after break/continue**: Prevent panics from emitting after terminators.
5. **Implicit width matching**: Auto-widens narrower operands with `eprintln!` warnings.
6. **Logical And/Or are NOT short-circuit**: Both operands always evaluated.
7. **Context-aware literals**: Integer/float literals in var-decl/assign are typed to target, overflow is a compile error.
8. **Two-pass functions**: Enables forward references.
9. **Array-to-pointer decay**: Arrays never load — return pointer directly.
10. **Pointer arithmetic via GEP**: `build_gep` with correct element type for stride calculation.
11. **Implicit return**: Functions without explicit return get `ret void` or `ret <zero>`.
