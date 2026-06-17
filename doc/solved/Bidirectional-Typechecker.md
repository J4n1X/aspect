# Bidirectional Typechecker Plan

## Status: ✅ Implemented (2026-06-17)

Landed in `src/typechecker/checker.rs`. Notes where reality differed from this plan:

- **Parser default is `i32`, not `i64`.** `parse_primary` stamps integer
  literals as `i32` when they fit, `i64` otherwise — so the synth default and
  the "leave as-is" arms use `i32` for small literals. The check-mode logic is
  unchanged by this (it overwrites with the target either way).
- **Step 8 (codegen narrowing) was a no-op.** `ConstantEmitter::emit_int_literal`
  never narrowed — it builds a constant at the `LangType` it is handed. The
  actual boundary coercion already lived in `generate_coerced_value` /
  `generate_literal_typed` (codegen/expressions.rs). No codegen change was
  needed; the stamped `expr_type` simply makes those paths emit no-op casts.
- **IR is byte-identical** for all 18 programs in `tests/programs/` at `-O0`
  (verified by diff). The visible win shows up in runtime mixed-width
  arithmetic (`u8 x = y + 1` now emits `add i8` instead of `zext → add i32 →
  trunc`), which the existing corpus does not exercise.
- **`check_expr_coercible` / `expr_coercible_to` deleted.** Their literal-fit
  logic now lives in `check_expression`'s `Literal` arms; non-literal leaves use
  `assert_coercible` (a thin `types_coercible` wrapper).
- All 12 plan test cases implemented as `#[cfg(test)]` unit tests in
  `checker.rs`; the full integration suite passes unchanged.

## Overview

Replace the current synthesis-only type checker with a **bidirectional** checker
that propagates a target type *into* expressions when context provides one,
instead of synthesising a type bottom-up and then patching it at the use site.

Concretely: addresses [TODO.md:7](../../TODO.md) — "propagate target type into
literal `expr_type` during type checking, so codegen emits constants at the
correct width without post-hoc coercion."

Today, [src/typechecker/checker.rs:336](../../src/typechecker/checker.rs)
implements `check_expression(&self, expr) -> LangType` — a pure synthesis pass.
Literals get a default `expr_type` from the parser, the checker computes the
type of every node bottom-up, and *mismatches are caught at boundaries* (assignment
RHS, return, function arguments) via `check_expr_coercible`
([checker.rs:500](../../src/typechecker/checker.rs)). Codegen then
re-narrows literal constants via `ConstantEmitter` at the right width.

The bidirectional refactor flips this: when the surrounding context supplies a
target type, the checker pushes it *down* into the expression tree, validates
each literal against the target (using the existing `literal_int_fits` /
`literal_float_compatible` predicates), and **stamps `expr_type` on the AST in
place**. Codegen then reads the stamped type and never has to widen or narrow
a literal post-hoc.

## Goals

- Two checker modes per expression: `synth(e)` and `check(e, target)`.
- Literal `expr_type` is set by the checker, not the parser's default.
- `check_expr_coercible` is absorbed into `check_expression` for literals.
- Codegen's `ConstantEmitter` no longer re-narrows literal widths — the AST
  arrives already correctly typed at constant-evaluation sites.
- Better error messages: "literal `257` does not fit in `u8`" reported at the
  literal's position, not at the assignment.

## Non-Goals

- No new language features. No generics, no overload resolution, no
  Hindley-Milner inference.
- The checker still emits errors directly into `self.errors`; no
  constraint-collection phase.
- No changes to `LangType` or `ExprKind` shape. AST nodes are mutated in place
  via their existing `expr_type` field.
- No changes to codegen *semantics*. The IR for any well-typed program must be
  identical before and after this refactor. The existing
  [tests/integration_tests.rs](../../tests/integration_tests.rs) suite is the
  acceptance criterion.
- The parser still assigns a default `expr_type` to literals during construction
  (so AST nodes are always well-formed); the checker overwrites it where context
  applies.

## Current State

| Concern | Location | Today's behaviour |
|---|---|---|
| Expression checking | `checker.rs:336` `check_expression(&self, &Expression) -> LangType` | Pure synthesis. Returns the bottom-up type. |
| Literal handling | `checker.rs:338` | Returns `expr.expr_type` as-is (set by parser). |
| Coercibility at boundaries | `checker.rs:500` `check_expr_coercible` | Special-cases literal fit at assignment / return / arg sites. |
| Result of `Binary` | `checker.rs:363` `wider_type` | Synthesises the wider operand type, ignoring context. |
| Literal narrowing | `codegen/value_emitter.rs` `ConstantEmitter::emit_int_literal` | Re-emits the constant at the type stamped on the literal node. |

The result is that a literal inside `u8 x = 1 + 2` is synthesised as `i64`,
the addition is synthesised as `i64`, coercibility is checked at the assignment,
and codegen narrows back to `u8`. The bidirectional refactor short-circuits all
of this by pushing `u8` *into* `1 + 2` at type-check time.

## Approach

### Two modes

```rust
impl TypeChecker {
    /// Synthesise the type of `expr` with no contextual expectation.
    /// Used at "extraction" sites where nothing constrains the type:
    /// callee resolution, indexing, condition of `if`/`while`.
    fn synth_expression(&mut self, expr: &mut Expression) -> LangType;

    /// Check `expr` against an expected type.  Stamps `expr.expr_type` and
    /// pushes the expectation into children where it applies.
    /// Used at every site that has a target: assignment RHS, return value,
    /// function-call arguments, explicit `let T x = ...`, cast targets'
    /// inner expression (only when the cast is a no-op of literal width).
    fn check_expression(&mut self, expr: &mut Expression, target: &LangType);
}
```

Both functions take `&mut Expression`. `Expression` already carries a mutable
`expr_type: LangType` field; the checker writes to it. This is the only AST
mutation introduced by this plan.

The signature change ripples up:
- `check_program(&mut self, &mut Program)` — must accept a mutable AST.
- `check_global_var(&mut self, &mut GlobalVar)`
- `check_function(&mut self, &mut Function)`
- `check_statement(&mut self, &mut Statement)`
- `check_expression` / `synth_expression` as above.

### Synth/check rule per `ExprKind`

| `ExprKind` | synth | check(target) |
|---|---|---|
| `Literal(Integer(n))` | leave `expr_type` as parser default (`i64`) | if `literal_int_fits(n, target)` → stamp `expr_type = target`; else error |
| `Literal(Float(f))` | leave default (`f64`) | if `literal_float_compatible(target)` → stamp `expr_type = target`; else error |
| `Literal(String)` | stamp as `i8*` | stamp as `i8*`; verify `target` is also `i8*` |
| `Variable(name)` | look up; stamp `expr_type = found_type` | synth, then assert `types_coercible(found, target)` |
| `Binary { left, op, right }` | synth both; result = `wider_type` | **check both against `target`**, result = `target`; falls back to synth if `target` is non-numeric or pointer-typed |
| `Comparison { .. }` | synth both for validity; result = `i32` | synth both; result type is `i32` — never propagate `target` inward |
| `Reference(inner)` | synth inner; result = `inner_type` with `pointer_depth + 1` | derive inner target (target with `pointer_depth - 1`), check inner against it |
| `Dereference(inner)` | synth inner; result = `inner_type` with `pointer_depth - 1` (or array element) | derive inner target (target with `pointer_depth + 1`), check inner against it |
| `FunctionCall { name, args }` | look up sig; check each arg against its param type; result = return type | synth, then assert `types_coercible(ret, target)` |
| `Cast { expr, target_type }` | synth inner; result = `target_type` | synth (cast forces the type — the outer target is checked for coercibility only) |
| `Alloc { count, .. }` | check `count` against `u64`; result = pointer type | synth, assert coercible |
| `UnaryNot(inner)` | check `inner` for non-void; result = `i32` | as synth; never propagate inward |
| `BitwiseNot(inner)` | check inner; result = inner type | check inner against `target` |
| `ListInitializer(elements)` | synth each; result = inner-element pointer/array type | derive element target from `target` (decay array → element), check each element against it |

**Propagation rule of thumb**: propagate the target into a child when the child's
type *is* the parent's type (arithmetic, bitwise-not, reference/dereference,
list-init elements). Do *not* propagate when the operator changes the type
(comparison, unary-not, cast, function call).

### Boundary sites switch from "synth + coerce" to "check"

| Site | Today | After |
|---|---|---|
| `let T x = e;` ([checker.rs:165](../../src/typechecker/checker.rs) statement handler) | synth `e`; call `check_expr_coercible(e, T)` | `check_expression(&mut e, &T)` |
| `return e;` | synth `e`; coerce-check against current function return type | `check_expression(&mut e, &return_type)` |
| `f(a, b, ...)` arg check ([checker.rs:412](../../src/typechecker/checker.rs)) | synth each arg; `expr_coercible_to(arg, param)` | `check_expression(&mut arg, &param_type)` |
| `lhs = rhs;` (assignment) | synth `rhs`; coerce-check against `lhs` | `check_expression(&mut rhs, &lhs_type)` |
| Global variable initialiser | synth + coerce | `check_expression(&mut init, &decl_type)` |
| `if (cond)` / `while (cond)` | synth `cond` | `synth_expression(&mut cond)` — no target; the existing "must be numeric/pointer" rule still runs in synth mode |

### Why `check_expr_coercible` mostly disappears

`check_expr_coercible` exists today to special-case literal-fit at boundary
sites (a `u32` parameter can accept a literal `42` typed as `i64`, because
`literal_int_fits` says so). Once the checker pushes the target down, the
literal arrives at the leaf with the target in hand and validates fit there.
`check_expr_coercible` collapses to the literal cases inside
`check_expression` for `Literal(Integer)` and `Literal(Float)`, and
`types_coercible` is used at non-literal leaves (`Variable`, `FunctionCall`).

## File-by-file Changes

### `src/typechecker/checker.rs`

- Rename existing `check_expression(&self, &Expression) -> LangType` to
  `synth_expression(&mut self, &mut Expression) -> LangType`.
- Add `check_expression(&mut self, &mut Expression, target: &LangType)`.
- Update `check_program`, `check_global_var`, `check_function`,
  `check_statement` to take `&mut`.
- Delete `check_expr_coercible` and its sole helpers
  (`expr_coercible_to`) once all callers migrate.
- At each boundary site enumerated above, switch from the synth + coerce
  pattern to a single `check_expression` call.

### `src/typechecker/types.rs`

- No public-API changes. `literal_int_fits`, `literal_float_compatible`,
  `types_coercible`, `cast_valid` are reused as-is. Their callers move from
  `check_expr_coercible` into `check_expression`'s literal arms.

### `src/typechecker/mod.rs`

- If `check_program` is part of the public surface, its signature changes from
  `&Program` to `&mut Program`. Update re-exports.

### `src/main.rs`

- The compilation pipeline already owns `Program` mutably (it builds it from
  the parser). Update the `check_program` call site to pass `&mut program`.

### `src/codegen/value_emitter.rs`

- `ConstantEmitter::emit_int_literal` no longer needs to narrow at the literal
  type — by the time codegen runs, the literal's `expr_type` is already the
  final width. The narrowing logic becomes a sanity assertion (debug-only) or
  is deleted. Verify by diffing IR output before and after.

### `src/codegen/expressions.rs`

- No changes expected. `walk_expression` reads `expr.expr_type` at literal
  sites — that field is now authoritative.

### Tests

- [tests/integration_tests.rs](../../tests/integration_tests.rs) is the
  acceptance bar. No existing test should change behaviour.

## Test Plan

### New typechecker unit tests (`src/typechecker/checker.rs` `#[cfg(test)]`)

1. **Literal fits target on assignment**: `u8 x = 200;` — type-checks, `x`'s
   initialiser has `expr_type == u8` after checking.
2. **Literal overflows target**: `u8 x = 300;` — error at the literal's
   position, not at the `=`.
3. **Binary propagates target**: `u8 x = 1 + 2;` — both literals stamped as
   `u8`, the `+` stamped as `u8`.
4. **Binary mixed literal and variable**: `u8 y = 0; u8 x = y + 1;` — `1` is
   stamped `u8`, `y` synthesises to `u8`, result is `u8`.
5. **Comparison does not propagate**: `i32 c = a < b;` — comparison result is
   `i32` regardless of operand types; literals inside operands fall back to
   synth.
6. **Function call arg fit**: `f(300)` where `f` takes `u8` — error at the
   literal.
7. **Return propagates**: function returning `u16` with `return 65535;` —
   literal stamped `u16`. With `return 65536;` — error at the literal.
8. **Dereference target propagation**: `u8 x = *p;` with `p: u8*` — synth
   path; `*p` already produces `u8`, coercibility holds.
9. **Reference target propagation**: `u8* p = &x;` — `&x`'s inner is checked
   against `u8`.
10. **Cast does not propagate**: `u32 x = (u32) 300;` — the literal `300` is
    synthesised at parser default (i64), the cast forces u32. Casting then
    `u32 x` accepts the cast result via coerce.
11. **List initialiser propagates element type**: `u8 arr[3] = {1, 2, 3};` —
    every literal stamped `u8`.
12. **List initialiser overflow**: `u8 arr[3] = {1, 2, 300};` — error at
    `300`.

### Existing integration tests

- The full [tests/integration_tests.rs](../../tests/integration_tests.rs) suite
  must pass with **byte-identical exit codes**. Any test failure indicates a
  semantics regression and must be investigated before proceeding.
- Snapshot the IR output of two or three representative programs with
  `--emit ir` before starting, and diff after each refactor step. The IR must
  not change for already-correct programs.

## Implementation Order

1. Add `synth_expression` as a clone of the current `check_expression` taking
   `&mut Expression` (no behaviour change yet — still synthesis-only).
2. Thread `&mut` through `check_program`, `check_global_var`, `check_function`,
   `check_statement`. Run the test suite. **Commit.**
3. Add an empty `check_expression(&mut self, &mut Expression, &LangType)` that
   currently just calls `synth_expression` and then asserts
   `types_coercible`. Run the test suite. **Commit.**
4. Migrate boundary sites one at a time (assignment, return, function args,
   var decl init, global init), each with `check_expression`. After each
   migration, run the suite and diff IR. **Commit per site.**
5. Implement real check-mode behaviour per `ExprKind`, starting with
   `Literal(Integer)` and `Literal(Float)`. Move the literal-fit checks out of
   `check_expr_coercible`. Run the suite. **Commit.**
6. Implement check-mode propagation for `Binary`, `Reference`, `Dereference`,
   `BitwiseNot`, `ListInitializer`. Run the suite after each.
7. Delete `check_expr_coercible` and `expr_coercible_to`. Run the suite.
   **Commit.**
8. Simplify `ConstantEmitter::emit_int_literal` to drop post-hoc narrowing.
   Diff IR — must be identical for the existing test corpus. **Commit.**
9. Add the new unit tests from the Test Plan section.
10. Update `doc/05-typechecker.md` to document synth/check modes and the
    propagation rule.
