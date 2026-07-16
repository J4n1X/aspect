# Codegen Architecture Refactor Plan

## Overview

`src/codegen/generator.rs` is a 1997-line single-file God object with one
`impl CodeGenerator` block containing ~40 methods. It mixes function emission,
statement emission, expression emission, constant folding, scope management,
optimization, and IR output into one struct. There is heavy duplication between
the "runtime IR emission" path and the parallel "compile-time constant" path.

This plan refactors the codegen module by:

1. **Splitting** the monolith along the same axes the `parser/` module already
   uses (`expressions.rs`, `statements.rs`, plus new files where helpful).
2. **Introducing traits** that absorb the repeated dispatch logic (signed/unsigned,
   int/float, runtime/constant, `LangType → LLVM type`).
3. **Extracting state** (scope stack, function table) into small focused structs
   so individual codegen routines borrow only what they need.

The refactor MUST be semantics-preserving — the existing test suite at
`tests/` is the acceptance criterion. **Do not change generated IR.** If a
refactor step changes IR output, revert that step and try a smaller decomposition.

## Goals

- Reduce `generator.rs` from ~2000 LOC to a thin orchestrator (~150 LOC).
- Eliminate the four pairs of duplicated runtime/constant routines
  (`generate_int_binary_op` / `const_int_binary_op`, `cast_value` /
  `const_cast_value`, `generate_literal_typed` / `generate_constant_literal`,
  `generate_expression` / `generate_constant_expression`).
- Replace ad-hoc `matches!(ty.base, TypeBase::SInt)` / `matches!(.., SFloat)`
  call sites with named helpers on `LangType`.
- Make the LLVM-type conversion (`lang_type_to_llvm`, `lang_type_to_llvm_array`,
  `lang_type_element_to_llvm`) callable as methods on `LangType` via an
  extension trait, so call sites read `lang_type.to_llvm(ctx)` instead of
  `lang_type_to_llvm(ctx, lang_type)`.

## Non-Goals

- No new language features. No new optimisations. No new IR shapes.
- No public API changes to `CodeGenerator::new`, `generate`, `optimize`,
  `module`, `get_target_machine`, `print_ir_to_string`, `write_ir_to_file` —
  `src/main.rs:197` and downstream callers must continue to work unchanged.
- Do not touch `src/parser/`, `src/lexer/`, `src/typechecker/`, or `aspect-macros/`.

## Current Smells Inventory

Use these as the checklist for what the refactor must eliminate:

| Smell | Location | Resolution |
|---|---|---|
| 1997-line single file | `generator.rs` | Split into 7+ files |
| Three out-of-impl-but-inside-impl methods with column-0 indentation | `generator.rs:1854`, `:1903`, `:1960` | Move into proper modules with consistent indent |
| Runtime vs constant binary op duplication | `generate_int_binary_op` (921), `generate_float_binary_op` (976), `const_int_binary_op` (1307), `const_float_binary_op` (1371) | Unify via `ValueEmitter` trait (see §4) |
| Runtime vs constant cast duplication | `cast_value` (1172), `const_cast_value` (1409) | Unify via `ValueEmitter` trait |
| Two literal generators | `generate_literal_typed` (818), `generate_constant_literal` (1598) | Unify via `ValueEmitter` trait |
| Two expression walkers | `generate_expression` (658), `generate_constant_expression` (1522) | Unify via `ValueEmitter` trait |
| `matches!(ty.base, TypeBase::SInt/SFloat/UInt)` scattered | many call sites | Add `LangType::is_signed_int()`, `::is_float()`, `::is_pointer()`, `::is_int()` |
| Free `lang_type_to_llvm(ctx, &ty)` style | `types.rs:185`, all call sites | Add trait `LangTypeExt` with `to_llvm(&self, ctx)` |
| `lookup_var_info` returns a clone of `LocalVar` instead of a reference | `generator.rs:1696` | Return `Option<&LocalVar<'ctx>>`; callers copy fields they need |
| Scope stack + global map both queried in `lookup_var_info`, conflating local and global | `generator.rs:1702` | Keep both maps but expose `lookup_local` / `lookup_global` / `lookup_any` separately |
| `current_function` / `current_function_return_type` set/cleared by hand | `generator.rs:203`, `:241` | Encapsulate in a small `FunctionScope` RAII guard, or at minimum a `with_function` helper |

## Target Module Layout

```
src/codegen/
├── mod.rs
├── errors.rs              # unchanged
├── types.rs               # type conversions + LangTypeExt trait
├── ops.rs                 # NEW: operator dispatch helpers
├── scope.rs               # NEW: ScopeStack + LocalVar + GlobalVarInfo
├── value_emitter.rs       # NEW: ValueEmitter trait + RuntimeEmitter + ConstantEmitter
├── expressions.rs         # NEW: expression emission (uses ValueEmitter)
├── statements.rs          # NEW: statement emission (uses RuntimeEmitter only)
├── functions.rs           # NEW: function declaration + body emission
├── globals.rs             # NEW: global variable + string literal emission
└── generator.rs           # SHRUNK: ~150 LOC orchestrator, holds shared state
```

Keep `pub use` re-exports in `mod.rs` so `crate::codegen::CodeGenerator` and
`crate::codegen::CodegenError` still resolve.

## §1 — `LangTypeExt` (in `types.rs`)

`LangType` lives in `src/lexer/` and we don't modify upstream crates' files
casually, but adding an extension trait inside `codegen/types.rs` is fine:

```rust
pub trait LangTypeExt {
    fn is_signed_int(&self) -> bool;
    fn is_unsigned_int(&self) -> bool;
    fn is_int(&self) -> bool;            // signed OR unsigned
    fn is_float(&self) -> bool;
    fn is_pointer(&self) -> bool;        // pointer_depth > 0
    fn is_void(&self) -> bool;

    fn to_llvm<'ctx>(&self, ctx: &'ctx Context)
        -> Result<BasicTypeEnum<'ctx>, CodegenError>;
    fn to_llvm_array<'ctx>(&self, ctx: &'ctx Context)
        -> Result<ArrayType<'ctx>, CodegenError>;
    fn element_to_llvm<'ctx>(&self, ctx: &'ctx Context)
        -> Result<BasicTypeEnum<'ctx>, CodegenError>;

    /// LangType of one pointer-depth less. Used by deref + pointer arithmetic.
    fn pointee(&self) -> LangType;
}

impl LangTypeExt for LangType { /* delegate to existing free functions */ }
```

Keep the free functions for one commit (delegating to the trait) so the diff is
small, then mechanically replace call sites: `lang_type_to_llvm(ctx, &ty)` →
`ty.to_llvm(ctx)`. Remove the free functions in the final cleanup commit.

The `is_array()` method already exists on `LangType` — do **not** redefine it.

## §2 — `ScopeStack` (in `scope.rs`)

Move out of `CodeGenerator`:

```rust
pub struct LocalVar<'ctx> { /* same fields as today */ }
pub struct GlobalVarInfo<'ctx> { /* same fields */ }

pub struct ScopeStack<'ctx> {
    scopes: Vec<HashMap<String, LocalVar<'ctx>>>,
    globals: HashMap<String, GlobalVarInfo<'ctx>>,
}

impl<'ctx> ScopeStack<'ctx> {
    pub fn new() -> Self;
    pub fn enter(&mut self);
    pub fn exit(&mut self);
    pub fn insert_local(&mut self, name: String, var: LocalVar<'ctx>);
    pub fn insert_global(&mut self, name: String, info: GlobalVarInfo<'ctx>);
    pub fn lookup_local(&self, name: &str) -> Option<&LocalVar<'ctx>>;
    pub fn lookup_global(&self, name: &str) -> Option<&GlobalVarInfo<'ctx>>;
    /// Local takes precedence, global is fallback. Returns a unified borrowed view.
    pub fn lookup_any(&self, name: &str) -> Option<VarRef<'_, 'ctx>>;
}

pub enum VarRef<'a, 'ctx> {
    Local(&'a LocalVar<'ctx>),
    Global(&'a GlobalVarInfo<'ctx>),
}
```

Replace `CodeGenerator::lookup_var_info` (returns owned copy) with
`self.scope.lookup_any(name)` returning a borrow. Call sites that need to
escape the borrow copy the fields they want into locals.

## §3 — `ops.rs`: Operator Dispatch

Today, `BinaryOp` is matched in four places. Two improvements:

1. Add helpers that classify operators:

```rust
pub fn is_comparison_op_supported_for_floats(op: &BinaryOp) -> bool;
```

2. Inline the unified-op idea via the `ValueEmitter` trait (§4) — once that
   trait exists, the four binary-op functions collapse into one generic
   function parameterised over the emitter.

The `signed_op!` and `const_signed_op!` macros stay where they are.

## §4 — `ValueEmitter` trait (in `value_emitter.rs`)

**This is the heart of the refactor.** The runtime path and the constant path
do the same arithmetic — they only differ in *how* they materialise the result.
Runtime calls `builder.build_int_add(...)`; constant path extracts `u64`s,
adds in Rust, and reconstructs a `const_int`. Same for sub/mul/div/mod, bitwise
ops, casts, comparisons, literals.

```rust
pub trait ValueEmitter<'ctx> {
    fn context(&self) -> &'ctx Context;

    fn emit_int_binary(
        &self,
        op: &BinaryOp,
        lhs: IntValue<'ctx>,
        rhs: IntValue<'ctx>,
        is_signed: bool,
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError>;

    fn emit_float_binary(
        &self,
        op: &BinaryOp,
        lhs: FloatValue<'ctx>,
        rhs: FloatValue<'ctx>,
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError>;

    fn emit_cast(
        &self,
        value: BasicValueEnum<'ctx>,
        target_llvm: BasicTypeEnum<'ctx>,
        src_lang: &LangType,
        dst_lang: &LangType,
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError>;

    fn emit_int_literal(
        &self,
        val: i64,
        ty: &LangType,
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError>;

    fn emit_float_literal(
        &self,
        val: f64,
        ty: &LangType,
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError>;

    fn emit_widen_ints(
        &self,
        a: IntValue<'ctx>, a_signed: bool,
        b: IntValue<'ctx>, b_signed: bool,
    ) -> Result<(IntValue<'ctx>, IntValue<'ctx>), CodegenError>;

    fn emit_widen_floats(
        &self,
        a: FloatValue<'ctx>,
        b: FloatValue<'ctx>,
    ) -> Result<(FloatValue<'ctx>, FloatValue<'ctx>), CodegenError>;
}
```

Two implementations:

- `RuntimeEmitter<'a, 'ctx>` — borrows `&'a Builder<'ctx>` and `&'ctx Context`.
  Uses `build_int_add`, `build_int_signed_div`, the existing `signed_op!` macro,
  the existing `widen_ints_to_match` / `widen_floats_to_match`.

- `ConstantEmitter<'ctx>` — borrows only `&'ctx Context`. Uses the existing
  bit-pattern extraction logic from `const_int_binary_op` / `const_cast_value`.

The runtime/constant expression walker becomes one generic function:

```rust
pub fn walk_expression<'ctx, E: ValueEmitter<'ctx>>(
    expr: &Expression,
    emitter: &E,
    env: &dyn ExprEnv<'ctx>,
) -> Result<BasicValueEnum<'ctx>, CodegenError>;
```

where `ExprEnv` exposes whatever the walker needs from the surrounding
codegen state (variable lookup, function table, module access for string
literals). The runtime path passes a full `RuntimeEnv`; the constant path
passes a `ConstantEnv` that errors on disallowed expression kinds (function
calls, non-`const` locals, etc.).

**Acceptance:** after this trait lands, `const_int_binary_op`,
`const_float_binary_op`, `generate_int_binary_op`, `generate_float_binary_op`,
`cast_value`, `const_cast_value`, `generate_constant_literal`,
`generate_literal_typed`, `generate_constant_expression` are all deleted, and
their callers go through `walk_expression` with the appropriate emitter.

## §5 — `expressions.rs`

Contains:

- The generic `walk_expression` from §4.
- `ExprEnv` trait + `RuntimeEnv` / `ConstantEnv` impls.
- The `generate_expression` entry-point used by statements (just calls
  `walk_expression` with `RuntimeEmitter` + `RuntimeEnv`).
- `generate_coerced_value`, which keeps its fast-path-for-literals logic and
  delegates to `walk_expression` for the general case.
- `generate_function_call` and `generate_function_call_statement` — these are
  expression-shaped but can only run in the runtime path, so they live with
  the runtime env, not in the trait.
- `generate_alloc` (currently at line 1960, broken indentation).

## §6 — `statements.rs`

Move all `generate_*` methods that take `&Statement` or statement subfields:

- `generate_statement`
- `generate_expression_statement`
- `generate_var_decl`
- `generate_var_assign`
- `generate_deref_assign`
- `generate_return`
- `generate_block`
- `generate_if_statement`
- `generate_while_loop`
- `generate_for_loop`
- helpers: `block_has_terminator`, `value_to_bool`, `get_zero_value`,
  `try_fold_constant_expression` (this one only needs `ConstantEmitter`)

Each becomes either a free function `fn generate_x(state: &mut CodegenState, ...)`
or stays as an `impl CodegenState` method, whichever reads cleaner. Prefer the
`impl` form for anything that touches >2 fields.

## §7 — `functions.rs`

Move `declare_function` and `generate_function`. They need read access to the
function table and write access to scope + builder position.

Add a small RAII helper:

```rust
pub struct FunctionScope<'a, 'ctx> { /* ... */ }
impl Drop for FunctionScope<'_, '_> { fn drop(&mut self) { /* clear current_function */ } }
```

so `current_function = Some(...)` / `current_function = None` isn't manual.

## §8 — `globals.rs`

Move `generate_global_variable`, `generate_string_literal`,
`generate_constant_array_value`, `generate_list_initializer`.

## §9 — `generator.rs` (slim)

What remains:

```rust
pub struct CodeGenerator<'ctx> {
    pub(crate) context: &'ctx Context,
    pub(crate) module: Module<'ctx>,
    pub(crate) builder: Builder<'ctx>,
    pub(crate) target: Target,
    pub(crate) functions: HashMap<String, FunctionValue<'ctx>>,
    pub(crate) function_lang_params: HashMap<String, Vec<LangType>>,
    pub(crate) scope: ScopeStack<'ctx>,
    pub(crate) current_function: Option<FunctionValue<'ctx>>,
    pub(crate) current_function_return_type: Option<LangType>,
    pub(crate) loop_stack: Vec<(BasicBlock<'ctx>, BasicBlock<'ctx>)>,
}

impl<'ctx> CodeGenerator<'ctx> {
    pub fn new(...) -> Self { ... }                  // unchanged
    pub fn generate(&mut self, program: &Program) -> AnyhowResult<()> {
        globals::emit_string_literals(self, &program.string_literals);
        functions::declare_all(self, &program.functions)?;
        globals::emit_globals(self, &program.global_vars)?;
        functions::emit_bodies(self, &program.functions)?;
        Ok(())
    }
    pub fn module(&self) -> &Module<'ctx> { &self.module }
    pub fn get_target_machine(&self) -> Result<TargetMachine, CodegenError> { ... }
    pub fn optimize(&self, level: u8) -> Result<(), CodegenError> { ... }
    pub fn print_ir_to_string(&self) -> String { ... }
    pub fn write_ir_to_file(&self, path: &Path) -> Result<(), CodegenError> { ... }
}
```

Use `pub(crate)` fields so the sibling modules can read/write directly without
piling on accessor methods. This is idiomatic for a tightly-coupled internal
module group.

## Migration Order (one commit per step)

Each step compiles green and passes `cargo test`. Stop and investigate if any
test starts failing — semantics changes are bugs.

1. **Add `LangTypeExt` trait in `types.rs`.** Free functions stay; trait
   delegates to them. No call site changes yet.
2. **Migrate call sites to the trait** (`ty.to_llvm(ctx)` etc.). Mechanical.
3. **Delete the free functions** once no caller references them. (`is_void_type`
   becomes `ty.is_void()`.)
4. **Extract `ScopeStack` into `scope.rs`.** Replace the three fields
   (`variables`, `global_variables`) on `CodeGenerator` with `scope: ScopeStack`.
   Update `enter_scope`, `exit_scope`, `add_variable`, `lookup_var_info` to
   delegate. Verify tests.
5. **Fix the indentation of the three trailing functions** (`generate_constant_array_value`,
   `generate_list_initializer`, `generate_alloc`) — pure whitespace; this is a
   no-op for the compiler but unblocks splitting.
6. **Move statements into `statements.rs`.** One large move; verify tests after.
7. **Move functions into `functions.rs`.**
8. **Move globals into `globals.rs`.**
9. **Add `ValueEmitter` trait + `RuntimeEmitter` + `ConstantEmitter`** in
   `value_emitter.rs`. *Initial impls just call the existing private helpers
   on `CodegenState`* — this proves the trait shape compiles before deleting
   anything.
10. **Add `walk_expression` + `ExprEnv` + envs** in `expressions.rs`. Route
    `generate_expression` through it. Tests still pass.
11. **Route `generate_constant_expression` through `walk_expression`** with
    `ConstantEmitter` + `ConstantEnv`. Delete `generate_constant_expression`.
12. **Delete duplicated routines** (`const_int_binary_op`, `const_float_binary_op`,
    `const_cast_value`, `generate_constant_literal`, `generate_literal_typed`,
    `generate_int_binary_op`, `generate_float_binary_op`) — their logic now
    lives inside the two emitter impls.
13. **Final pass:** add `FunctionScope` RAII, shrink `generator.rs` to the
    orchestrator form in §9. Cargo fmt, cargo clippy --all-targets.

## Verification Strategy

After **every** step:

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
```

In addition, after steps 6–13, do an IR diff against the pre-refactor baseline:

```bash
# Before starting refactor (from master), once:
for f in tests/programs/*.ap; do
    cargo run --quiet -- "$f" > /tmp/baseline/$(basename $f).ll 2>/dev/null || true
done

# After each post-step-5 commit:
for f in tests/programs/*.ap; do
    cargo run --quiet -- "$f" > /tmp/current/$(basename $f).ll 2>/dev/null || true
    diff /tmp/baseline/$(basename $f).ll /tmp/current/$(basename $f).ll
done
```

Allow only whitespace / SSA-name-numbering differences. Any semantic IR change
is a regression.

**Confirm the CLI invocation** — check `src/main.rs:197` for the exact codegen
entry point and adapt the diff script if the public API has changed since this
plan was written.

## Out-of-Scope (future work, not this PR)

- Visitor-pattern AST walker shared with the typechecker.
- Replacing `HashMap<String, …>` lookups with interned symbol IDs.
- Splitting `Builder` ownership so multiple emit-helpers can run in parallel
  (currently single-threaded by `&mut self`).
- Const-folding becoming a separate IR-independent pass (today it's interleaved
  with codegen via `try_fold_constant_expression`).

## Risk Notes

- The `ValueEmitter` trait introduces a lifetime parameter `'ctx` and may
  require a `where Self: 'ctx` bound on `walk_expression`. If trait-object
  dispatch causes lifetime-inference pain, use `enum Emitter<'a, 'ctx> { Runtime(...), Constant(...) }`
  as a fallback — same end result, no dyn.
- `lookup_var_info` returning a borrow instead of a copy will surface borrow-
  checker conflicts in callers that immediately call `&mut self` methods on
  the generator. Resolve by copying the small `Copy` fields (`PointerValue`,
  `BasicTypeEnum`, `LangType`) into locals before the `&mut self` call.
- `Position` is `Copy` — check before assuming you can move it across closures.

## Done Criteria

- `generator.rs` ≤ 200 LOC.
- No `const_*` / `generate_*` duplicated pairs remain.
- `cargo test` green; IR-diff against baseline shows no semantic changes.
- `cargo clippy --all-targets -- -D warnings` clean.
- `crate::codegen::CodeGenerator` public surface (the methods listed in §9)
  unchanged.
