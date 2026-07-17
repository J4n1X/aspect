# Codebase Refactor — Positions, Runtime/Constant Split, File Decomposition

Status: **largely landed** (branch `refactor/file-decomposition`). Three
independent focus areas, each shippable on its own. No language-surface changes;
this is internal code-health work only.

Progress:
- **§1**: R1 (derive `position()`), R3 (type-lowering position at the boundary),
  R4 (position-less synthetic errors) landed. R2 (drop error-only `pos` params)
  applied only where behaviour-preserving — the codegen indirect-call trio now
  read `callee.pos`; the remaining named helpers keep `pos` because they receive
  extracted primitives (`LangType`, `&str`), not a redundant pos-bearing node, so
  dropping it would move the diagnostic (a change the fragment-based failure tests
  would not catch).
- **§2**: landed as a **code-motion separation**, *not* the fully IR-independent
  `ConstValue` redesign sketched in §2.2 below. Rationale: `ConstValue` requires
  re-architecting constant-value storage; the lower-risk move was to extract
  `const_eval` (`src/codegen/const_eval.rs`) as the constant walker and make
  `walk_expression` runtime-only, deleting `EmitMode`/`emitter()`/`require_runtime`.
  Both paths still return `BasicValueEnum` and share `ConstantEmitter`/
  `RuntimeEmitter` via the `ValueEmitter` trait. Emitted IR is byte-identical to
  the pre-refactor baseline. Treat §2.2's `ConstValue`/`materialize` design as a
  *future* direction, not what shipped.
- **§3**: Tier 1 + Tier 2 (parser and checker splits) landed.
- **Prerequisite** (the original TODO gate — "solve the two bugs first"): met.
  Both blocking bugs are fixed in the accompanying feature work — `if p {}` /
  `while p` pointer truthiness and stack/BSS literal-count `alloc`.

This document is the canonical plan for three refactors requested together:

1. **Simplify position reporting** so `pos` need not be threaded through
   everything (§1).
2. **More distinctly separate runtime code from compile-time-constant code**
   in code generation (§2).
3. **Split the largest files** where a clean seam makes them more readable —
   and explicitly *not* where it wouldn't (§3).

The three are ordered by how invasive they are, but they are decoupled: any
one can land without the others.

---

## Analysis baseline

The file/line references below reflect the working tree at the **inline-assembly
milestone** (local `master` = `38c8e5b`, plus uncommitted WIP). Note that
`origin/master` (`1297ad9`) is one commit behind and does **not** yet contain
the `asm fn` parsing/checking code, so the asm-specific split targets in §3
(`parser/asm.rs`, `checker/asm.rs`) assume the local asm work is present. Line
numbers are illustrative anchors for a design discussion, not a patch — treat
them as "look near here," not as exact coordinates that must survive rebases.

Rough sizes at the time of analysis:

| File | Lines | % tests |
|---|---:|---:|
| `src/parser/expressions.rs` | 2387 | 4% |
| `src/typechecker/checker.rs` | 1952 | 32% |
| `src/preprocessor/conditional.rs` | 1019 | 44% |
| `src/codegen/expressions.rs` | 851 | 0% |
| `src/codegen/value_emitter.rs` | 702 | 0% |
| `src/preprocessor/mod.rs` | 688 | 14% |

`pos` appears ~685 times across `src/`; 104 functions take an explicit
`pos: Position` parameter.

---

## Guiding principles

These apply to all three areas and are the acceptance frame for every step.

- **Semantics-preserving.** The `tests/` suite is the acceptance criterion.
  For codegen changes specifically, **do not change generated IR** — diff
  against a pre-refactor baseline (see [Verification](#verification-strategy)).
  Any semantic IR change is a regression, not a refactor.
- **Incremental, green at every commit.** One logical move per commit; `cargo
  build` + `cargo test` + `cargo clippy --all-targets -- -D warnings` pass
  before the next step. If a step goes red, shrink it.
- **Root cause, not workaround.** Where the current code threads state or
  duplicates logic because a boundary is in the wrong place, move the boundary;
  don't paper over it. (This is a standing project preference.)
- **Split only where it helps.** Line count is not the metric — cognitive load
  is. A long-but-flat file (one `match` with independent arms) is not a
  splitting candidate; a file holding two unrelated subsystems is.

---

## §1 — Position reporting

### 1.1 Current state

`Position` is a 20-byte `Copy` struct — `{ line, column, file_id }`
(`src/lexer/errors.rs:8-13`). It originates in the scanner
(`Scanner::current_position`, `src/lexer/scanner.rs:64`), is stamped onto every
`Token` (`src/lexer/tokens.rs:438`), copied into every AST node's `pos` field
(`src/parser/ast.rs:128,186,207,218,241,270`), and read back out by the type
checker and code generator to populate error values.

**There is exactly one consumer.** Every phase ends at an identical
`format_error` boundary that is the *sole* reader of position data for output:

```rust
// src/typechecker/checker.rs:92 (parser/codegen/preprocessor are identical in shape)
pub fn format_error(&self, err: &TypeCheckError) -> String {
    let Some(pos) = err.position() else { return format!("{err}"); };
    match self.source_files.get(pos.file_id as usize) {
        Some(path) => format!("{}:{}:{}: {}", path.display(), pos.line, pos.column, err),
        None => format!("{err}"),
    }
}
```

Mirror sites: `src/parser/expressions.rs:236`, `src/codegen/generator.rs:228`,
`src/preprocessor/mod.rs:303`; call sites in `src/main.rs:180,197,211`. Each
error enum re-implements a hand-written `position() -> Option<Position>` matcher
(`parser/errors.rs:133`, `typechecker/errors.rs:231`, `codegen/errors.rs:46`,
`preprocessor/errors.rs:159`).

So the ~685 `pos` threads exist to feed a single `file:line:column:` prefix.
That is the lever: **the fan-out from "one pos per node" to "dozens of
error-only helper parameters" is the waste**, not the position data itself.

### 1.2 What cannot be eliminated (the hard floor)

`pos` is *not* purely diagnostic, and several sites need it explicitly. A
refactor that ignores these will break behavior:

- **`file_id` is load-bearing logic.** `check_import_visibility` reads
  `use_pos.file_id` to decide which module a use-site belongs to
  (`parser/expressions.rs:196`). Position provenance drives a real semantic
  check, not just a message.
- **Deliberate "not-my-node" positions.** Binary/cast/call/index/field-access
  report the *sub-expression's* start, not the current token —
  `let pos = left.pos;` (`expressions.rs:437`), `expr.pos` (`:494`),
  `callee.pos` (`:656`), `array_expr.pos` (`:712`), `base.pos` (`:1742`). The
  span is a semantic choice made at capture time; nothing can derive it
  mechanically from the finished node.
- **Synthetic nodes with no source location.** Whole-module codegen failures
  fabricate positions: `Position::new(0,0)` (`generator.rs:305,319,355`),
  `(1,1)` (`codegen/asm.rs:119,129`), registration-time lowering
  (`structs.rs:38`, `functions.rs:97`). These have no AST node to read from.
- **Two-position errors.** `ConditionalAfterElse { else_pos, pos }`
  (`preprocessor/errors.rs:62`), `DuplicateModuleDirective { pos, previous }`
  (`:100`) genuinely carry an origin *and* a conflicting site.
- **Position-keyed symbol registration.** `symbol_table.add_variable(name, ty,
  pos)` stores the declaration site for later duplicate-decl diagnostics
  (`statements.rs:264`).

**Conclusion up front:** total elimination is not possible ("if possible" — it
isn't, fully). The realistic goal is to **cut the fan-out to the ~30 error-only
leaf helpers** and delete the boilerplate, leaving only the load-bearing sites
above explicit.

### 1.3 Tractable simplifications (recommended, ranked)

**R1 — Auto-derive `position()` (highest value, lowest risk).**
The four hand-written `position()` matchers are mechanical `match` arms over
`(_, pos)` tuples and `{ position }` fields, and they drift: add a variant,
forget the arm. The repo already ships a proc-macro crate (`aspect-macros/`).
Add a `#[derive(HasPosition)]` (or a field attribute `#[pos]`) that generates
`position(&self) -> Option<Position>`. Deletes all four matchers and the drift
class. The only design question is the rule for two-position variants — pick
the field named `pos` (origin) as canonical, let the second stay message-only.

**R2 — Drop `pos` from error-only helpers that already hold a node.**
Many helpers take `pos` purely to forward it to an error, *while also receiving
the node the pos came from*. Read `.pos` off the node instead and delete the
parameter. This pattern already coexists in the codebase — `check_register_class`
reads `reg.pos` (`checker.rs:422`), `generate_field_assign` takes no pos and
reads `target.pos`, `emit_pointer_arithmetic` reads `left.pos`
(`expressions.rs:107`). Targets:
- Typechecker: `assert_coercible`, `check_value_block`, `resolve_field`,
  `check_method_access`, `check_call`, `check_pinnable_type`
  (`checker.rs:1243,623,977,1050,1074,389`) — each receives an expression/field
  it can read the pos from.
- Codegen: `generate_var_assign`, `generate_return`, `generate_deref_assign`
  (already reads `target.pos`), and similar (`statements.rs`).

**R3 — Narrow the type-lowering surface (biggest single win in codegen).**
`LangTypeExt::to_llvm/to_llvm_array/element_to_llvm` take `pos: Position`
*only* to attach to a possible `TypeError` — the success path never touches it
(`codegen/types.rs:75-188`). Because `LangType` is a `Copy` value with no
position, this drags `pos` through the *entire* type-lowering call graph. Fix:
remove `pos` from these signatures; have them return a position-less
`CodegenError::TypeError(msg)` (or a dedicated `TypeLoweringError`); attach the
position at the one call site in `walk_expression` via
`.map_err(|e| e.with_pos(expr.pos))`. The `.with_pos` combinator already has a
working precedent: `ParserError::from_symbol(err, pos)` grafts a position onto
a position-less `SymbolError` at a phase boundary (`parser/errors.rs:95`). Same
treatment applies to the `ValueEmitter` leaf methods `emit_int_literal` /
`emit_float_literal`, whose only `pos` use is an error arm
(`value_emitter.rs:81,98`) — and note `RuntimeEmitter::emit_int_binary` /
`emit_float_binary` already take `_pos` **unused** (`value_emitter.rs:139,277`),
proving the parameter is dead weight for one implementer.

**R4 — Reclassify synthetic "0:0" positions as position-less.**
The fabricated `Position::new(0,0)` / `(1,1)` sites (§1.2) currently produce a
lying `entry-file:0:0:` prefix. `CodegenError` already models genuinely
position-less variants correctly (`MainNotFound`, `UnsupportedTarget`,
`errors.rs:25-40`). Move the module-level/registration failures to
position-less variants so `format_error` cleanly prints the bare message. Small,
localized, removes misinformation.

### 1.4 Considered and deferred

- **Ambient `self.current_pos` tracker (deferred).** A mutable "position of the
  node currently being walked" field, set at each `walk`/`check` entry, read by
  leaf helpers. Precedent exists — the `$if` evaluator's `Eval.last_pos`
  (`conditional.rs:401`) and `TypeChecker.current_function` (`checker.rs:38`)
  are exactly this shape. **Deferred because** it introduces staleness: the
  type-lowering and value-emitter calls happen deep inside a node's lowering,
  and the parser's deliberate "report the sub-expression's pos" choices
  (§1.2) mean a single ambient slot would frequently report the *wrong*
  sub-node without disciplined push/pop. R2+R3 get most of the benefit without
  the hazard.
- **`Spanned<T>` / `HasPos` trait (deferred).** Every AST node has an identical
  `pub pos: Position` + `::new(.., pos)`; a `trait HasPos { fn pos(&self) ->
  Position; }` would unify "read `.pos` off any node." **Deferred because** it
  unifies node *access* but does not remove the parser's obligation to *choose*
  which token's pos to capture (§1.2), so it buys little over R2 while touching
  every node type. Tokens are already effectively `Spanned<TokenKind>`
  (`tokens.rs:436`); the pattern is half-present and not worth completing now.

### 1.5 Recommended sequence for §1

1. R1 (derive `position()`) — isolated, mechanical, unblocks confidence.
2. R2 (drop error-only params where a node is in hand) — one phase at a time
   (typechecker, then codegen); each is independently testable.
3. R3 (narrow type-lowering + literal-emit surface via `.with_pos` at the
   boundary) — the codegen-specific win.
4. R4 (position-less synthetic errors) — cleanup.

Net effect: the ~30 error-only helper parameters and 4 boilerplate matchers go
away; the load-bearing sites (§1.2) stay explicit and documented as such.

---

## §2 — Runtime vs constant separation in codegen

### 2.1 Current state

A prior refactor ([`doc/solved/Codegen-Refactor.md`](../solved/Codegen-Refactor.md))
already introduced the `ValueEmitter` trait with two implementations —
`RuntimeEmitter` (builder-based IR) and `ConstantEmitter` (Rust-level folding) —
and a single `walk_expression(expr, gen, mode: EmitMode)`
(`codegen/expressions.rs:152`). That refactor **explicitly deferred** the next
step in its own Out-of-Scope list:

> Const-folding becoming a separate IR-independent pass (today it's interleaved
> with codegen via `try_fold_constant_expression`).

That deferred step *is* this focus area. The current design is a **hybrid** that
is not yet a distinct separation:

- **No first-class const value type.** Both modes return bare
  `inkwell::values::BasicValueEnum<'ctx>`. Nothing at the type level
  distinguishes an LLVM constant from a runtime SSA value; the mismatch is
  caught only by the LLVM verifier (documented hazard,
  `expressions.rs:619-627`).
- **Constant-ness is decided by exceptions.** `try_fold_constant_expression`
  is literally `walk_expression(expr, self, EmitMode::Constant).ok()`
  (`statements.rs:600`) — walk in constant mode, swallow any error, treat `None`
  as "not constant." Control flow via `Result`, not a value property.
- **One walker, four intermixed arm shapes** (`expressions.rs:152-583`):
  always-constant (`Bool`, `SizeOf`, `Null`, `FunctionRef`), delegated-to-emitter
  (`Literal`, `Binary`, `Cast`), internally mode-branching (`Variable`,
  `Reference`, `UnaryNot`, `BitwiseNot`, `StructLiteral`, `ValueBlock`), and
  runtime-only guarded by `require_runtime(mode, .., pos)` (`Comparison`,
  `Dereference`, `FunctionCall`, pointer arithmetic, field access,
  `IndirectCall`).
- **Duplicated operator semantics.** `RuntimeEmitter` and `ConstantEmitter`
  independently implement the *same* arithmetic rules and must stay bug-for-bug
  consistent — the comment "to match the runtime behaviour"
  (`value_emitter.rs:493`, on the logical-op i32 result) makes the coupling
  explicit. There is no shared source of truth for op semantics.
- **Scattered "must be constant" enforcement.** No single predicate; the rule
  is the *union* of `require_runtime` guards, per-arm `EmitMode::Constant =>
  Err(..)` branches, `ConstantEmitter` operand-type errors, and
  `generate_constant_array_value` rejecting non-literal elements
  (`globals.rs:117`).
- **Duplicated aggregate/coercion paths.** Two array lowerers
  (`generate_constant_array_value` in `globals.rs:97` vs `generate_list_initializer`
  in `expressions.rs:742`, overlapping at `:769`); three+ coercion mechanisms
  (`emit_cast`, `coerce_constant_to_type` `globals.rs:167`, an inline
  `ConstantEmitter{}.emit_cast` `statements.rs:150`, and `generate_literal_typed`
  `expressions.rs:670`); the `StructLiteral` runtime path always uses
  `insertvalue` even when all fields are constant (missed fast-path, TODO at
  `expressions.rs:506`).
- **A weak notion of "constant."** `generate_list_initializer` decides
  `all_const` by "every element is a bare int/float literal"
  (`expressions.rs:762`) — a `const` local, `sizeof(T)`, or a nested constant
  expression fails the check and forces the runtime element-store path even
  though `ConstantEmitter` could fold it.
- **Doc/impl drift.** Module docs reference a `generate_constant_expression`
  entry point (`expressions.rs:10`, `value_emitter.rs:9`) that does not exist —
  a leftover from the deferred plan. Clean this up as part of the work.

### 2.2 Recommended direction — a standalone, IR-independent const-evaluator

Make constant evaluation a real, separate thing: a pure function over the AST
that produces a **Rust-side value**, with **no dependency on inkwell/LLVM**, and
one narrow boundary that materializes that value into an LLVM constant.

```rust
// New: src/codegen/const_eval.rs  (no `use inkwell` for the eval itself)

/// A compile-time-known value, independent of any LLVM Context.
pub enum ConstValue {
    Int { bits: u32, signed: bool, value: i128 },
    Float { bits: u32, value: f64 },
    NullPtr,
    FnRef(String),                 // link-time constant: a function address
    Aggregate(Vec<ConstValue>),    // arrays and struct literals
}

/// Pure, recursive, testable without a Context. `Err` means "not a constant."
pub fn const_eval(expr: &Expression, env: &ConstEnv) -> Result<ConstValue, ConstEvalError>;

/// The single boundary where a constant becomes LLVM IR.
impl<'ctx> CodeGenerator<'ctx> {
    fn materialize(&self, cv: &ConstValue, target: &LangType) -> BasicValueEnum<'ctx>;
}
```

What this buys, mapped to the current smells:

- **Distinct separation, structurally.** Const evaluation lives in its own
  module and its own type. `walk_expression` loses the `EmitMode` parameter
  entirely and becomes the *runtime-only* walker: all `EmitMode::Constant`
  arms, the `require_runtime` guards, and the `emitter(cg, mode)` boxing
  (`expressions.rs:39-52`) are deleted.
- **Constant-ness becomes a value property, not an exception.**
  `try_fold_constant_expression` becomes `const_eval(expr, env).ok()` over a
  purpose-built result — no more "walk in a special mode and swallow the error."
  Aggregate const-ness (`all_const`, StructLiteral fast-path) keys off whether
  `const_eval` succeeds on each element, replacing the weak literal-only
  heuristic.
- **Centralized enforcement.** "Must be constant" is exactly `const_eval`
  returning `Err`. Globals, `const` locals, and global `alloc` counts share one
  entry point instead of four scattered checks.
- **Testability.** The const-evaluator is unit-testable with no LLVM `Context`
  — feed it AST, assert `ConstValue`. Today the constant path can only be
  exercised through full codegen.
- **Unified aggregate lowering.** One routine builds a `ConstValue::Aggregate`;
  `materialize` emits `const_array` / `const_named_struct`; globals use it
  directly, locals use it for the fast path and fall back to GEP-store only when
  `const_eval` fails on some element. The two array lowerers collapse to one.

### 2.3 The duplication that remains, and why

Runtime arithmetic (`build_int_add`, …) and constant folding (Rust `i128`
arithmetic) are **genuinely different operations** — one emits an instruction,
the other computes a number. That is not duplication to eliminate; it is the
actual runtime/constant boundary made honest. What the current design duplicates
*unnecessarily* is having both paths re-derive the *same operator semantics*
(signedness, wrapping vs `nsw`, the logical-op i32 result). Keep the shared
**classification** helpers (`int_cmp_pred`, `signed_op!`, the op→behavior
decisions) as the single source of truth that both `materialize`/runtime consult;
only the final materialization differs.

**Divergence risk & mitigation.** The current single-walker design guarantees
the runtime and constant paths cover the *same* set of `ExprKind`s. Splitting
`const_eval` out risks the two drifting (a new `ExprKind` handled in one but not
the other). Mitigate with: (a) both `const_eval` and `walk_expression` `match`
exhaustively on `ExprKind` (no `_ =>` catch-all), so the compiler forces a
decision for every new variant; (b) a property test that evaluates a corpus of
constant expressions through `const_eval` + `materialize` and through a
runtime-emitted-then-constant-folded reference, asserting identical bit patterns.

### 2.4 Alternatives considered

- **`enum GenValue { Const(..), Runtime(..) }` tag (lighter, deferred).** Tag
  every value at the type level but keep the single mode-threaded walker. Less
  disruptive, but it does not achieve the *separation* goal — const and runtime
  logic stay interleaved in one walker; it only makes the leak detectable
  earlier than the verifier. Reasonable as an intermediate step if the full
  extraction proves too large in one go.
- **`walk_runtime` / `walk_const` split without a Rust value type (rejected).**
  Two walkers returning `BasicValueEnum` removes the mode flag but duplicates
  every shared arm (`Binary`, `Cast`, `Literal`) and keeps const-eval tied to a
  `Context`, forfeiting IR-independence and testability. The `ConstValue`
  approach is strictly better.

### 2.5 Recommended sequence for §2

1. Introduce `ConstValue` + `const_eval` + `ConstEnv`, initially delegating to
   the existing `ConstantEmitter` internals so nothing else changes (proves the
   shape compiles, IR unchanged).
2. Add `materialize`; route global initializers and `const`-local folding
   through `const_eval` + `materialize`. Delete `generate_constant_expression`
   doc references.
3. Unify the two array lowerers and add the `StructLiteral` constant fast-path,
   keyed off `const_eval`.
4. Remove `EmitMode` from `walk_expression`; delete the `Constant`-mode arms,
   `require_runtime`, and the `emitter(cg, mode)` dispatch. `ConstantEmitter`
   is absorbed into `const_eval`.
5. Fold the redundant coercion paths (`coerce_constant_to_type`, inline
   `ConstantEmitter{}.emit_cast`) into `materialize`.

IR-diff against baseline after every step (see below); this area is the one most
likely to accidentally change emitted IR.

---

## §3 — File decomposition

### 3.1 The module idiom, and the one visibility rule

Three of four big subsystems already use **"one shared struct, methods spread
across sibling files via `impl` blocks."** Sibling files re-open the struct:
`parser` (`impl Parser` in `expressions.rs:115`, `statements.rs:59`), `codegen`
(`impl CodeGenerator` in `asm.rs`, `globals.rs`, `expressions.rs`,
`statements.rs`, `structs.rs`, `functions.rs`). The idiom to add a submodule:
declare `pub mod foo;` in `mod.rs`, write another `impl <SharedStruct>` in
`foo.rs`.

**The outlier is `typechecker`** — everything is one `impl TypeChecker` in
`checker.rs`, which is exactly why it is 1952 lines.

**The one rule that dictates sibling-vs-child:** a struct's private fields are
visible in the defining module *and its descendants*, not in siblings. The
parser pays for its flat layout by marking cross-file fields `pub(crate)`
(`Parser.current`, `string_literals`, `context_stack`, `errors`). `TypeChecker`'s
fields are **all private** (`checker.rs:34-53`). Therefore:
- Split into a **sibling** module ⇒ promote touched fields to `pub(crate)`.
- Split into a **child** module (`checker.rs` → `checker/mod.rs` + children) ⇒
  private access is retained for free.

For `checker.rs`, use the **child-module form** to avoid promoting 8 private
fields.

### 3.2 Ranked plan

**Tier 1 — highest value, low/zero risk, do first**

| # | Action | Reduction | Notes |
|---|---|---:|---|
| T1 | Extract `mod tests` from `conditional.rs` and `checker.rs` into co-located `#[cfg(test)] mod tests;` child files | ~451 + ~619 | `use super::*` keeps private access; zero logic risk; halves both files instantly |
| T2 | `conditional.rs` → split the `$if` constant-expression evaluator into `preprocessor/expr_eval.rs` | ~280 out | **Cleanest seam in the codebase:** the evaluator (`single_name`, `expand_operands`, `Eval`, `infix_prec`, `apply`, `mask_shift`, `:286-567`) shares *no* mutable state with the chain state machine — only 2 items become `pub(crate)`. Fits the existing per-directive layout. |
| T3 | `checker.rs` → child module `checker/asm.rs` (asm checking, `:221-426`) | ~205 out | Self-contained, gated on `self.target`; template is `codegen/asm.rs` |

**Tier 2 — high value, more mechanical**

| # | Action | Reduction | Notes |
|---|---|---:|---|
| T4 | `parser/expressions.rs` → `parser/asm.rs` (`:1942-2096`) | ~155 out | Cleanest parser seam; uses only already-`pub(crate)` cursor helpers → no new promotions; mirrors `codegen/asm.rs` |
| T5 | `parser/expressions.rs` → `parser/program.rs` (top-level driver + two-pass, `:1135-1489`) and `parser/declarations.rs` (struct/method/function/global, `:1490-1740` + `:2097-2291`) | ~755 out | Separates "the expression parser" from "the program driver"; requires promoting `pending_bodies`, `alias_prescan_sites`, `module`, `symbol_table`, `source_files`, `file_modules` to `pub(crate)`. After this, `expressions.rs` is ~1150 and genuinely the expression parser. |
| T6 | `checker.rs` → `checker/statements.rs` (`:445-677`) + `checker/expressions.rs` (`synth_expression` `:678-976`, resolution `:977-1113`, `check_expression`/coerce/type-algebra `:1115-1306`) | ~1060 out | The bidirectional-checker seam (synth vs checked); child modules alongside T3, no field promotion. Methods are mutually recursive across the seam but only through `&mut self`, so the boundary is clean. `checker/mod.rs` keeps struct + config + `check_program` + globals + scope helpers (~300). |

**Tier 3 — marginal, optional**

- `preprocessor/mod.rs`: extract `mod tests` + helpers (`:569-688`, ~120) and
  optionally `suggest_directive`/`levenshtein` → `preprocessor/suggest.rs`
  (~27). Driver body is already cohesive.
- `parser/core.rs`: move the `Parser` struct (`:83-114`) + cursor helpers out of
  `expressions.rs` (fixes the "struct defined in expressions.rs" wart; forces
  `pub use expressions::Parser` → `pub use core::Parser` in `parser/mod.rs:9`).
  Cosmetic.

**Tier 4 — do NOT split**

- **`codegen/expressions.rs` (851).** One cohesive recursive walker
  (`walk_expression`) plus its private helpers and entry points. The 433-line
  `match` is long but *flat* — each arm is an independent `ExprKind` with no
  cross-references. Splitting severs tightly-coupled recursion for no
  readability gain. (If §2 lands, this file *shrinks* naturally as the
  `Constant`-mode arms leave.)
- **`codegen/value_emitter.rs` (702).** The length is irreducible *symmetry* —
  a trait plus two parallel impls whose whole value is being read side-by-side
  (compare `emit_int_binary` runtime `:133` against constant `:413`). Splitting
  `RuntimeEmitter`/`ConstantEmitter` into separate files would *reduce*
  readability. (§2 may absorb `ConstantEmitter` into `const_eval` instead —
  that's a semantic move, not a mechanical file split.)

### 3.3 Recommended sequence for §3

Do Tier 1 first (T1→T2→T3) as a low-risk warm-up — the test extractions and the
`conditional.rs` split are almost pure motion. Then Tier 2 as appetite allows.
Skip Tier 3/4. Note §3 interacts with §2 (T4/§2 both touch codegen file
boundaries; land §2's `const_eval` extraction before or after T4, not
interleaved) and with §1 (T6's checker split is easier once §1's R2 has removed
the error-only `pos` params from those same helpers).

---

## Cross-area sequencing

The three areas are independent, but a sensible global order minimizes churn on
the same lines:

1. **§3 Tier 1** (test extractions + `conditional.rs`) — low-risk, immediate
   readability, touches files the other areas don't.
2. **§1 R1–R2** (derive `position()`, drop error-only params) — mechanical,
   shrinks the helper signatures the later splits will move.
3. **§2** (the `const_eval` extraction) — the largest single effort; do it as a
   contiguous block with IR-diffing, not interleaved with anything.
4. **§1 R3–R4** (type-lowering surface, position-less synthetics) — codegen-local,
   easiest after §2 has already reshaped `walk_expression`.
5. **§3 Tier 2** (parser/checker splits) — last, once the churn above has settled
   so moves are clean.

`aspect-macros/` is touched by both §1 R1 (the `position()` derive) and, if
pursued later, tooling elsewhere — coordinate those in one macro-crate change.

---

## Verification strategy

After **every** step:

```bash
cargo build
cargo test
cargo clippy --all-targets -- -D warnings
```

For any codegen-affecting change (all of §2, §1 R3–R4), additionally IR-diff
against a pre-refactor baseline:

```bash
# once, before starting, from a clean master:
mkdir -p /tmp/ir-baseline
for f in tests/programs/*.ap; do
  cargo run --quiet -- compile -I lib "$f" --print > "/tmp/ir-baseline/$(basename "$f").ll" 2>/dev/null || true
done

# after each codegen step:
for f in tests/programs/*.ap; do
  cargo run --quiet -- compile -I lib "$f" --print > "/tmp/ir-current/$(basename "$f").ll" 2>/dev/null || true
  diff "/tmp/ir-baseline/$(basename "$f").ll" "/tmp/ir-current/$(basename "$f").ll"
done
```

Allow only whitespace / SSA-name-numbering differences. Any semantic IR change
is a regression — revert and decompose smaller. (Confirm the exact `compile`
invocation against `src/main.rs`; the flag set has changed before.)

---

## Non-goals

- **No language-surface changes.** No new syntax, types, IR shapes, or
  optimizations. `demos/` are not tests (per project convention) and are not the
  acceptance signal — `tests/` is.
- **No public API changes** to `CodeGenerator::{new, generate, optimize, module,
  get_target_machine, print_ir_to_string, write_ir_to_file, jit_execute,
  jit_execute_main}` — `src/main.rs` and the integration harness must keep
  working unchanged.
- **Do not split** `codegen/expressions.rs` or `codegen/value_emitter.rs` (§3.2
  Tier 4).
- **Do not** attempt full `pos` elimination — the §1.2 hard floor stays
  explicit.
- Out of scope for now: a visitor-pattern AST walker shared between typechecker
  and codegen; interned symbol IDs replacing `HashMap<String, _>`; the ambient
  `current_pos` tracker (§1.4).

---

## Risk notes

- **§1:** `file_id` provenance must survive any lazy-attachment change — the
  import-visibility check depends on it (`expressions.rs:196`). The two-position
  errors need an explicit canonical-field rule for the `position()` derive.
- **§2:** highest IR-regression risk — the `ConstantEmitter` folding must remain
  bit-for-bit identical when absorbed into `const_eval` (watch the logical-op
  i32 result, `value_emitter.rs:493`, and signed division-by-zero handling).
  `i128` for `ConstValue::Int` must be range-checked against the target width in
  `materialize` exactly as `generate_literal_typed` does today
  (`expressions.rs:670`). Keep `match ExprKind` exhaustive in both walkers to
  prevent coverage drift.
- **§3:** sibling splits force `pub(crate)` field promotions (parser T5); child
  splits (checker T3/T6) avoid that but change the module path — verify
  `pub use` re-exports in `mod.rs` keep `crate::typechecker::TypeChecker` and
  friends resolving. `Position` and most inkwell handles are `Copy`; the
  mutually-recursive checker methods borrow only through `&mut self`, so the T6
  seam has no field-borrow tangle.

---

## Done criteria

Per area, independently:

- **§1:** no hand-written `position()` matchers remain (all derived); the ~30
  error-only helper `pos` parameters are gone; `LangTypeExt` lowering methods and
  `emit_*_literal` no longer take `pos`; synthetic `0:0` positions are
  reclassified position-less; `cargo test` green.
- **§2:** `const_eval` is a standalone module with no `use inkwell` in its
  evaluation core and unit tests that run without a `Context`; `walk_expression`
  no longer takes `EmitMode`; `require_runtime`, the `Constant`-mode arms, and
  the second array lowerer are deleted; `generate_constant_expression` doc drift
  removed; IR-diff against baseline shows no semantic change.
- **§3:** Tier 1 done (both `mod tests` extracted; `expr_eval.rs` split;
  `checker/asm.rs` extracted); Tier 2 done to appetite; `conditional.rs` ≤ ~250
  and `checker.rs`/`checker/mod.rs` ≤ ~300; no file in Tier 4 was split;
  `cargo clippy --all-targets -- -D warnings` clean.
