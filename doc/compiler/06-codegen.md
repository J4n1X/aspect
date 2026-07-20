# Code Generation

The codegen module (`src/codegen/`) emits LLVM IR via Inkwell (pinned to LLVM 19.1).

## Files

| File | Purpose |
|------|---------|
| `generator.rs` | `CodeGenerator` struct + orchestration (`new`, `generate`, public API) |
| `expressions.rs` | `walk_expression` — the runtime expression walker + `CodeGenerator` expression entry-points |
| `const_eval.rs` | `const_eval` — the compile-time-constant expression evaluator (folds via `ConstantEmitter`) |
| `statements.rs` | `generate_statement` and all statement generators |
| `functions.rs` | `declare_function`, `generate_function`, `FunctionScope` RAII |
| `asm.rs` | `asm fn` and `naked fn` lowering: `constraint_string`, `generate_asm_function`, `generate_naked_function` |
| `globals.rs` | `generate_global_variable`, `generate_string_literal`, `generate_constant_array_value`, `generate_list_initializer` |
| `structs.rs` | Type-struct codegen: LLVM type registration, lowering through the struct cache, and the address (lvalue) path behind field access, field assignment, and `&expr` |
| `scope.rs` | `ScopeStack`, `LocalVar`, `GlobalVarInfo`, `VarRef` |
| `types.rs` | `LangTypeExt` trait + LLVM type helpers + operation helpers |
| `value_emitter.rs` | `ValueEmitter` trait + `RuntimeEmitter` + `ConstantEmitter` |
| `errors.rs` | `CodegenError` enum |

## CodeGenerator Struct

```rust
pub struct CodeGenerator<'ctx> {
    pub(crate) context: &'ctx Context,
    pub(crate) module: Module<'ctx>,
    pub(crate) builder: Builder<'ctx>,
  pub(crate) target_machine: TargetMachine,

    pub(crate) functions: HashMap<String, FunctionValue<'ctx>>,
    /// Parameter LangTypes per function name — arg coercion at call sites.
    pub(crate) function_lang_params: HashMap<String, Vec<LangType>>,
    /// Return LangType per function name — detects struct-by-value (`sret`) returns.
    pub(crate) function_return_types: HashMap<String, LangType>,
    /// While generating a struct-returning function, the hidden `sret`
    /// out-pointer that `return` stores through. `None` for scalar/void returns.
    pub(crate) current_sret: Option<PointerValue<'ctx>>,
    /// Named LLVM struct type per type-struct id (built in the registration pass).
    pub(crate) struct_types: HashMap<u32, StructType<'ctx>>,
    /// Ordered field layout per type-struct id: `(name, type)` in GEP-index order.
    pub(crate) struct_fields: HashMap<u32, Vec<(String, LangType)>>,
    /// Function-pointer signatures, indexed by `TypeBase::FnPtr(u32)`.
    /// Cloned from `program.symbols.fnptr_sigs` during `generate`.
    pub(crate) fnptr_sigs: Vec<crate::symbol::module::FnPtrSig>,
    pub(crate) scope: ScopeStack<'ctx>,

    pub(crate) current_function: Option<FunctionValue<'ctx>>,
    pub(crate) current_function_return_type: Option<LangType>,
    pub(crate) loop_stack: Vec<(BasicBlock<'ctx>, BasicBlock<'ctx>)>,
}
```

The target machine is created once in `CodeGenerator::new` and reused for optimization and object emission.

`CodeGenerator::new` uses LLVM's default relocation model for the triple; `CodeGenerator::new_with_reloc` takes an explicit `RelocMode` instead — this is how the `compile --relocation-model` CLI flag produces position-independent code. Because the reloc model is baked into the single cached `TargetMachine`, it governs *both* the optimization passes and the emitted object. When `RelocMode::PIC` is selected the module is also stamped with a `PIC Level` (value 2) flag so the emitted IR/object is self-describing, as clang does for `-fPIC` (using `FlagBehavior::Override`, since inkwell 0.9 does not expose clang's `Max` behavior — the recorded value is identical and the merge semantics only differ when linking two flagged IR modules, which never happens here).

Variable storage has been extracted into `ScopeStack` (see `scope.rs`).

### Helper Structs

- `LocalVar<'ctx>` — `ptr: PointerValue`, `llvm_type: BasicTypeEnum`, `lang_type: LangType`, `const_value: Option<BasicValueEnum>` (folded constant for `const` locals)
- `GlobalVarInfo<'ctx>` — `ptr: PointerValue`, `llvm_type: BasicTypeEnum`, `lang_type: LangType`

## Scope Stack (`scope.rs`)

`ScopeStack` replaces the old `variables` + `global_variables` pair on `CodeGenerator`.

| Method | Purpose |
|--------|---------|
| `enter()` / `exit()` | Push/pop a lexical scope |
| `insert_local` | Add a local variable to the current scope |
| `insert_global` | Register a global variable |
| `lookup_local` | Search local scopes (innermost first) |
| `lookup_global` | Search global map |
| `lookup_any` | Returns a `VarRef` (enum over `Local`/`Global`) |
| `iter_scopes` | Iterator over local scope maps |

## Type Translation (`types.rs`)

The `LangTypeExt` trait adds methods directly to `LangType`:

| Method | Behaviour |
|--------|-----------|
| `to_llvm(ctx, pos)` | Main conversion. Pointers and arrays → `ptr`. SInt/UInt → `iN`. SFloat → `fN`. Type-struct *values* → error (no cache access). `pos` is the source location the error reports. |
| `to_llvm_array(ctx, pos)` | Returns `[N x elem]` for statically-sized arrays (struct elements → error) |
| `element_type()` | Strip pointer/array, return base scalar `LangType` |
| `is_int()` | True for SInt/UInt with `pointer_depth == 0` |
| `is_float()` | True for SFloat with `pointer_depth == 0` |
| `is_void()` | True for Void with `pointer_depth == 0` |
| `is_array()` | True when `array_size.is_some()` |

**Key**: Signed vs unsigned makes no difference at the LLVM type level — both map to the same `iN`. Signedness is tracked by `LangType::base` and consulted at instruction selection time.

**Type-structs**: the trait methods are context-only and cannot resolve
`TypeBase::Struct(id)` against the generator's named-struct cache. Every
codegen site where a struct *value* type can appear must go through the
cache-aware `CodeGenerator::lang_type_to_llvm(ty, pos)` (scalars/pointers
fall through to `to_llvm`) or `CodeGenerator::lang_type_to_llvm_array` for
`[N x T]` allocas/globals. This matters for: pointer-arithmetic GEPs
(`Pair* + i` scales by struct size), dereference loads (`*(Pair*)` —
subscripts desugar to these), struct-array allocas (`Pair[2]`), and
struct-array globals. Regression test: `tests/programs/struct_arrays.ap`.

### Operation Helpers

| Helper | Purpose |
|--------|---------|
| `signed_op!(builder, is_signed, signed_method, unsigned_method, args...)` | Macro: dispatch to signed or unsigned variant of a builder method |
| `widen_ints_to_match(builder, a, a_signed, b, b_signed)` | Widen the narrower integer value using `sext`/`zext` |
| `widen_floats_to_match(builder, a, b)` | Widen the narrower float value using `fpext` |
| `int_cmp_pred(op, is_signed)` | Return `IntPredicate` for comparison op + signedness |
| `float_cmp_pred(op)` | Return ordered `FloatPredicate` for comparison op |

## Value Emitter (`value_emitter.rs`)

`ValueEmitter<'ctx>` is a trait encapsulating leaf-level operations:

| Method | Purpose |
|--------|---------|
| `emit_int_binary` | Integer binary operation |
| `emit_float_binary` | Float binary operation |
| `emit_cast` | Type cast |
| `emit_int_literal` | Emit an integer literal |
| `emit_float_literal` | Emit a float literal |
| `emit_widen_ints` | Widen two ints to match |
| `emit_widen_floats` | Widen two floats to match |

Two implementations:
- **`RuntimeEmitter<'a,'ctx>`** — borrows `builder: &'a Builder<'ctx>` + `context`; emits actual LLVM IR
- **`ConstantEmitter<'ctx>`** — borrows only `context`; performs Rust-level constant folding and returns LLVM constants without touching the builder

## Expression Walker (`expressions.rs`) and Constant Evaluator (`const_eval.rs`)

Runtime and compile-time-constant code generation are handled by two distinct
recursive walkers that share the same leaf-level `ValueEmitter` operations:

- **`walk_expression(expr, gen)`** (`expressions.rs`) — the walker for *runtime*
  expressions; emits LLVM IR via `RuntimeEmitter`.
- **`const_eval(expr, gen)`** (`const_eval.rs`) — the evaluator for *constant*
  expressions; folds via `ConstantEmitter` and returns LLVM constants without
  touching the builder. It errors — with the same diagnostics the runtime-only
  kinds always produced — on expression kinds that have no constant form
  (function/indirect calls, comparisons, dereferences, field access,
  value-blocks, pointer arithmetic, address-of-field/non-lvalue).

```rust
pub(crate) fn walk_expression<'ctx>(
    expr: &Expression,
    gen: &mut CodeGenerator<'ctx>,
) -> Result<BasicValueEnum<'ctx>, CodegenError>

pub(crate) fn const_eval<'ctx>(
    expr: &Expression,
    gen: &mut CodeGenerator<'ctx>,
) -> Result<BasicValueEnum<'ctx>, CodegenError>
```

Emitters are created transiently (after all recursive sub-expression calls
return) to avoid borrow-checker conflicts. The constant `Variable` case checks
local const values first, then falls back to global initializers. The shared
per-op dispatch (`emit_binary_dispatch`) is parameterised over a `&dyn
ValueEmitter`, so both walkers reuse it with their respective emitter.

### Public Entry-Points on `CodeGenerator`

| Method | Purpose |
|--------|---------|
| `generate_expression` | Runtime walk via `walk_expression` |
| `generate_coerced_value` | Runtime walk + auto-widen to target type; literal fast-path via `generate_literal_typed` |
| `generate_literal_typed` | Overflow-checked literal at a known target type |
| `generate_function_call` | Emit a call instruction |
| `generate_function_call_statement` | Call in statement position (handles void) |
| `generate_alloc` | Stack (`array_alloca`) or global (`[N x type]`) allocation |

## Function Linkage (`functions.rs`)

Decided in `declare_function` via `linkage_for`:

| Function | Linkage | Why |
|---|---|---|
| `extern fn` | external | a body-less declaration; internal linkage on one is invalid IR |
| `main`, `_start` | external | the C runtime calls one, the linker enters at the other, and the JIT looks up `main` by name |
| `public fn` / `public asm fn` | external | explicitly exported for foreign code |
| everything else | **internal** | the default |

Global variables follow the same rule in `globals.rs`: `public` → external,
otherwise `private` linkage.

A program and every module it imports become a *single* LLVM module, so
internal linkage costs nothing at the call site — a private function is still
callable from anywhere — and buys everything at the link: `globaldce` may
delete an unreachable internal function, and may **never** delete an external
one, since another object file might call in.

Defaulting to external is what put the whole unused standard library in every
binary, at every `-O` level.

`optimize()` therefore runs `globaldce` even at `-O0`. It is not an
optimization in the sense `-O0` disclaims: it removes only symbols nothing can
reach, so no code you could step through changes.

## Two-Pass Function Compilation (`functions.rs`)

### Pass 1: Declaration (`declare_function`)

For each function in the program:
1. Collect parameter `LangType`s into `function_lang_params` for call-site coercion
2. Convert parameter and return types to LLVM types
3. Create `fn_type` (non-variadic)
4. `module.add_function()` — adds to module
5. Set parameter names
6. Store `FunctionValue` in `self.functions`

### Pass 2: Body Generation (`generate_function`)

Uses `FunctionScope` RAII to set/clear `current_function` + `current_function_return_type`:

1. `FunctionScope::new(gen, function, return_type)` — sets both fields
2. Create `entry` basic block, position builder
3. Enter new scope
4. For each parameter: `alloca` in entry block, `store` LLVM param value, add to scope
5. Emit all body statements
6. If current block lacks terminator: `ret void` or `ret <zero>`
7. Exit scope; `FunctionScope::drop()` clears both fields automatically

Which of the two body generators runs is decided by an exhaustive match on
`FunctionBody` (see [03-ast](03-ast.md)): `Aspect` bodies go to
`generate_function`, `Asm` to `generate_asm_function`, `Extern` emits nothing
beyond its pass-1 declaration.

## `asm fn` Lowering (`asm.rs`)

An `asm fn` becomes a **real** LLVM function — internal, `alwaysinline` —
whose body is a single inline-asm call plus a return:

```llvm
define internal i64 @syscall3(i64 %nr, i64 %a1, i64 %a2, i64 %a3) alwaysinline {
entry:
  %asm.ret = call i64 asm sideeffect inteldialect "syscall",
        "={rax},{rax},{rdi},{rsi},{rdx},~{rcx},~{r11},~{memory},~{dirflag},~{fpsr},~{flags}"
        (i64 %nr, i64 %a1, i64 %a2, i64 %a3)
  ret i64 %asm.ret
}
```

That is the whole trick: because it is a genuine function, pass 1 declares it
from its `proto` with no special casing, and Aspect call sites stay ordinary
calls that the existing type checker, symbol table and `build_abi_call` handle
unchanged. `asm fn` is a *declaration* form, not an expression form. At `-O0`
it remains a real call; at `-O1`+ `alwaysinline` folds it to the bare
instruction.

`constraint_string` builds the constraint from the `AsmSpec` — output, then
inputs in parameter order, then clobbers, the order LangRef mandates. Two
things are compiler-decided and cannot be opted out of:

- **`sideeffect`** is always set, or LLVM deletes the asm whenever its result
  is unused.
- **`~{dirflag},~{fpsr},~{flags}`** is always appended, as clang does for
  every x86 asm block. Almost any instruction can touch EFLAGS, and
  `sideeffect` does *not* protect against it: without `~{flags}`, a caller
  holding a live comparison across the asm keeps its `cmp` above the block and
  branches on flags the asm destroyed — a silent wrong answer at `-O2` only.

An input naming the same register as the output (the in-out `rax` syscall
case) stays an ordinary untied `{rax}`; the numeric-tie form (`0`) exists to
constrain the register *allocator*, and nothing is left to allocate once both
ends are pinned by name.

## `naked fn` Lowering (`asm.rs`)

A `naked fn` also becomes a real LLVM function, but the opposite of an `asm fn`
in one decisive way: it carries LLVM's **`naked`** attribute (plus `noinline`),
so the backend emits **no prologue or epilogue**. There is no register contract
and no operands — with no prologue, arguments stay in their platform-ABI
incoming registers (SysV: `rdi`, `rsi`, …) and any result leaves through the
ABI return register, so the assembly body owns the *entire* calling convention:

```llvm
; Function Attrs: naked noinline
define internal i32 @add_abi(i32 %a, i32 %b) #0 {
entry:
  call void asm sideeffect inteldialect "mov eax, edi\0Aadd eax, esi\0Aret", ""()
  unreachable
}
```

The body is a single side-effecting, no-operand inline-asm block followed by
`unreachable` — control leaves only through the asm's own `ret`/`jmp`/`syscall`,
never by falling through. `generate_naked_function` sets the attributes, builds
that one asm call, and terminates with `unreachable`; linkage stays whatever
declaration decided (so `_start`, being implicitly public, is external).

Because it is still an ordinary function, call sites are ordinary calls. This is
the piece `asm fn` deliberately cannot be: an `asm fn` passes operands *into* a
function that has a prologue, whereas a naked fn has none — which is exactly what
a freestanding `_start` needs to read `argc`/`argv` off the stack (`argc` at
`[rsp]`, `argv` at `[rsp+8]`) and jump to `main`. The `unreachable` terminator is
safe against noreturn-inference killing callers — verified by the corpus running
every program at both `-O0` and `-O2` and requiring they agree.

## Statement Codegen (`statements.rs`)

| Statement | Handler |
|-----------|---------|
| `Expression(expr)` | `generate_expression_statement` (special-cases void function calls) |
| `VarDecl` | `generate_var_decl` — alloca in entry block; tries `try_fold_constant_expression` for `const` vars |
| `VarAssign` | `generate_var_assign` — lookup + `generate_coerced_value` |
| `DerefAssign` | `generate_deref_assign` — evaluate pointer + store |
| `Return` | `generate_return` — `generate_coerced_value` with `current_function_return_type` |
| `If` | `generate_if_statement` — condition→i1, then/else/ifcont blocks |
| `While` | `generate_while_loop` — cond/body/end blocks |
| `For` | `generate_for_loop` — init/cond/body/inc/end blocks |
| `Block` | `generate_block` — enter scope, emit statements, exit scope |
| `Break` | Branch to loop's break BB, create dead block |
| `Continue` | Branch to loop's continue BB, create dead block |

### `try_fold_constant_expression`

Delegates to `const_eval(expr, self).ok()`.
Returns `Option<BasicValueEnum>` — `None` for any non-constant sub-expression.

If folding succeeds, `generate_var_decl` stores the constant to the alloca and records it in `LocalVar::const_value`. Subsequent reads bypass the `load` instruction.

## Expression Codegen

### Value Coercion (`generate_coerced_value`)

All code paths that generate a value destined for a known target type (var-decl initializer, var-assign RHS, return value, function arguments, array element initializers) go through:

```rust
fn generate_coerced_value(expr: &Expression, target: Option<&LangType>) -> BasicValueEnum
```

- If `expr` is a numeric literal and `target` is a scalar type → `generate_literal_typed` (overflow-checked, typed directly to target)
- Otherwise → generate normally, then auto-widen via `RuntimeEmitter::emit_cast` if types differ

### Variables
- `ScopeStack::lookup_any` — searches locals (innermost first), then globals
- **Const locals**: if `const_value` is `Some`, returns the folded value directly (no `load`)
- Arrays return pointer directly (array-to-pointer decay)
- Scalars emit `build_load` with explicit pointee type

### Binary Operations

| Type | Operations |
|------|-----------|
| SInt/UInt | Add, Sub, Mul, `signed_op!(Div)`, `signed_op!(Mod)`, And, Or, Xor, Shift, LogicalAnd, LogicalOr |
| SFloat | Add, Sub, Mul, Div (with `widen_floats_to_match`) |
| Pointer | Add (GEP), Sub (neg + GEP) |

**Implicit width matching**: `widen_ints_to_match` / `widen_floats_to_match` silently widens the narrower operand.

**Logical And/Or**: NOT short-circuit — uses `select` instruction.

### Comparisons
- Widens operands before comparing
- Uses `int_cmp_pred(op, is_signed)` and `float_cmp_pred(op)` for predicate selection
- Result is an `i1` value; fed to `br` directly in conditionals
- **Pointer comparisons**: pointer-to-pointer comparisons are supported — `build_int_compare` is called with both pointer values directly (unsigned predicates via `int_cmp_pred(op, false)`), relying on Inkwell accepting `ptr` operands for `icmp`

### Casts (`RuntimeEmitter::emit_cast`)

| Source → Target | LLVM Instruction |
|-----------------|-----------------|
| int → pointer | `inttoptr` |
| pointer → pointer | `pointer_cast` |
| int → float (signed) | `sitofp` |
| int → float (unsigned) | `uitofp` |
| float → int (signed target) | `fptosi` |
| float → int (unsigned target) | `fptoui` |
| pointer → int | `ptrtoint` |
| int → int (wider, `i1` source) | always `zext` (boolean) |
| int → int (wider, other) | `sext` or `zext` (source signedness) |
| int → int (narrower) | `trunc` |
| Same type | No-op |

### Unary Operations
- `!expr` → `val == 0` → `i1` (a `bool`); it feeds `br` directly. Any
  widening is the ordinary `generate_coerced_value` cast to the target
  type (`zext` i1→i8 for `bool` storage, i1→i32 for an `i32` target),
  not a property of `!`.
- `~expr` → `build_not`

### Allocation
- **Global scope**: Constant count only. Creates `[N x type]` global with zero initializer.
- **Local scope**: `build_array_alloca(type, count)`

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
- Create **dead basic block** to prevent "instruction after terminator" panics

## Memory Management

### Alloca Placement Strategy

All `alloca` instructions are placed in the **function entry block**, not at the declaration site. The builder is temporarily repositioned to the entry block, the alloca is emitted, then the builder is restored. This is critical for LLVM's `mem2reg` pass.

### Loads and Stores
- All variable reads: `build_load(llvm_type, ptr, name)` with explicit pointee type
- All variable writes: `build_store(ptr, value)`
- Array variables: return pointer directly (no load)

## String Literals (`globals.rs`)

1. Create global `.str.{index}` of type `[N+1 x i8]` (null-terminated)
2. Set as constant
3. Register in scope with `lang_type = u8*`
4. In expressions: pointer cast to `i8*`

## Global Variables and Constant Expressions (`globals.rs`)

1. Compute LLVM type (arrays → `CodeGenerator::lang_type_to_llvm_array`, scalars → `lang_type_to_llvm`; both cache-aware for type-structs)
2. `module.add_global()`
3. For **array** initializers: `generate_constant_array_value` → LLVM `ConstantArray`
4. For **scalar** initializers: `const_eval(expr, self)`
5. For no initializer: `const_zero()`
6. Register in scope

### Constant Folding

All constant expression evaluation (global initializers, `const` local folding) goes through
`const_eval(expr, gen)` (`const_eval.rs`), which uses `ConstantEmitter`.

LLVM 19 removed almost all `LLVMConst*` arithmetic functions. `ConstantEmitter` performs
all arithmetic in Rust and reconstructs LLVM constants via `IntType::const_int` /
`FloatType::const_float`.

## List Initializers (`globals.rs`)

`generate_list_initializer(array_ptr, var_type, elements)` stores values into an allocated array pointer.

### Constant path (all elements are integer or float literals)

Calls `generate_constant_array_value` → single `build_store` of a `ConstantArray`.

```rust
int_ty.const_array(&vals)    // → [N x iM] ConstantArray
float_ty.const_array(&vals)  // → [N x fM] ConstantArray
```

Remaining slots are zero-padded.

### Runtime path (any element is a non-literal expression)

Each element is computed individually and stored via two-index GEP:

```
gep([N x elem], array_ptr, [i64 0, i64 i])  // &array[i]
```

## Optimization

```rust
codegen.optimize(level: u8, verify_each: bool) -> Result<(), CodegenError>
```

| Level | Pipeline |
|-------|---------|
| 0 | `globaldce` only (see [Function Linkage](#function-linkage-functionsrs)) |
| 1 | `default<O1>` |
| 2+ | `default<O2>` |
| 3 | `default<O3>` |

Extra options by level:
- `O1`: no extra pass options beyond the default pipeline
- `O2`: no extra pass options beyond the default pipeline
- `O3`: `loop_interleaving(true)`, `loop_slp_vectorization(true)`, `merge_functions(true)`, `call_graph_profile(true)`
- Any level outside `0..=3` falls back to the `O2` pipeline with no extra pass options.

## Emission Helpers

- `print_ir_to_string()` returns textual LLVM IR.
- `write_ir_to_file(path)` writes LLVM IR (`.ll`) to disk.
- `write_object_to_file(path)` emits target object code (`.o`) through LLVM's `FileType::Object`.

## Critical Gotchas

1. **Entry-block alloca hoisting**: All allocas must be in the entry block for `mem2reg`.
2. **Signedness is instruction-level**: Consult `LangType::base` at every operation; use `signed_op!` macro.
3. **Opaque pointers**: `build_load` requires explicit pointee type.
4. **Dead blocks after break/continue**: Prevent panics from emitting after terminators.
5. **Implicit width matching**: Auto-widens narrower operands silently.
6. **Logical And/Or are NOT short-circuit**: Both operands always evaluated.
7. **Context-aware literals via `generate_coerced_value`**: Uses `generate_literal_typed` with overflow checking.
8. **Two-pass functions**: Enables forward references.
9. **Array-to-pointer decay**: Arrays never load — return pointer directly.
10. **Pointer arithmetic via GEP**: `build_gep` with correct element type for stride calculation.
11. **Implicit return**: Functions without explicit return get `ret void` or `ret <zero>`.
12. **LLVM 19 const API is sparse**: All constant folding is done in Rust via `ConstantEmitter`. Avoid `const_*` builder methods.
13. **`const` locals are compile-time folded**: If the initializer folds, reads bypass the alloca/load.
14. **`FunctionScope` RAII**: `current_function` and `current_function_return_type` are set/cleared automatically via `Drop`.
15. **`RuntimeEmitter` borrow rule**: Create emitters transiently after all recursive `&mut gen` calls. `Builder::build_*` takes `&self`, so no mutable borrow conflict.

## JIT Execution

The codegen exposes two methods for running the just-emitted module via
Inkwell's `ExecutionEngine`, without writing IR to disk or invoking an external
interpreter:

```rust
codegen.jit_execute(func_name: &str, args: &[&GenericValue<'ctx>], opt_level: u8)
    -> AnyhowResult<u64>
codegen.jit_execute_main(args: &[&str], opt_level: u8) -> AnyhowResult<i32>
```

`jit_execute` is the generic entry point: the caller supplies LLVM
`GenericValue`s (build them with `IntType::create_generic_value`,
`FloatType::create_generic_value`, or
`GenericValue::create_generic_value_of_pointer`) and is responsible for keeping
any pointed-at storage alive for the duration of the call. The arg count is
validated against `func.count_params()`; mismatches return an error rather than
invoking undefined behavior. Returns the call's integer result as `u64` (or
`0` for void-returning functions).

`jit_execute_main` is a thin wrapper for the canonical
`main(u32 argc, u8 **argv) -> i32` entry point. It:

1. Looks up `main` via `get_function("main")` and validates the arity (2).
2. Builds a `Vec<CString>` and a null-terminated `Vec<*mut c_char>` from
   `args` — kept on the wrapper's stack frame so the raw `argv` pointer stays
   valid through the synchronous JIT call.
3. Constructs `argc` as a `u32` `GenericValue` and `argv` via
   `create_generic_value_of_pointer`.
4. Delegates to `jit_execute("main", ...)` and truncates the result to `i32`.

Both methods build the `ExecutionEngine` with an `OptimizationLevel` derived
from `opt_level` (`0`→`None`, `1`→`Less`, `2`→`Default`, `3`→`Aggressive`;
out-of-range values fall back to `Default`).

Consumers: the CLI `interpret` subcommand (`src/main.rs`) and the integration
test harness (`tests/integration_tests.rs`) both call `jit_execute_main`,
prepending the source path as `argv[0]` per C convention.

## LLVM Optimization Hints

Codegen attaches a few attributes/metadata that encode Aspect's semantics so the
optimizer can act on them:

| Hint | Where | Meaning |
|------|-------|---------|
| `nsw` on `add`/`sub`/`mul` | `value_emitter.rs` (`emit_int_binary`, signed only) | Signed overflow is **undefined** in Aspect. Unsigned arithmetic stays plain (defined wrapping). |
| `inbounds` on `getelementptr` | `expressions.rs` (indexing + pointer add/sub) | Pointer arithmetic must stay within the pointed-to allocation; out-of-bounds is UB. |
| `!range !{i8 0, i8 2}` on `bool` loads | `expressions.rs` (variable load) | A `bool` is stored as i8 but only ever 0 or 1, so the optimizer can fold branches/selects that test it. |

`bool` is dual-represented (Clang-style): its **value** form is `i1` (produced
directly by `icmp`, `&&`/`||` selects, and `!`), while its **storage** form is
`i8`. Stores zero-extend i1→i8; conditions read the value via `value_to_bool`,
which takes the raw i1 with no extra compare. `nounwind` was considered but
**not** applied — Aspect functions can call externs whose unwinding behaviour we
don't control.
