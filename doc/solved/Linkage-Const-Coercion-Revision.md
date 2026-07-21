# Proposal: Splitting `public`'s Linkage Meaning, Const Pointee Protection, Tighter Pointer Coercion, and a Signedness-Widening Warning

---

## RESOLVED DECISIONS (2026-07-21) — final confirmation pass + user answers

The `language-designer` final pass returned **Approved with changes** on all four. The user then resolved the three decision-conflicts it surfaced:

- **A — Full flip + migrate stdlib.** `export` is a new keyword = external LLVM linkage; `public` now means *module visibility* (nameable via `$import`) and is **enforced private-by-default for functions and globals** (mirroring the existing `public type` gate). `linkage` and `visibility` become two independent axes on `FunctionProto`/`GlobalVar` (they compose: `public export`). The whole stdlib's cross-module free-function/global surface is annotated `public` as part of this change (kept **internal linkage** — `public` ≠ `export`, so `globaldce` still strips unused stdlib). `FunctionSymbol`/`VarSymbol` gain a `vis` field so the parser's name-resolution can gate it. `extern` remains incompatible with both `public` and `export`.
- **B — "const means truly immutable unless cast."** Chosen over both preset options. Keeps the single `is_const: bool` flag but extends it to block *every* mutation — rebind (already), field-write (already), and **write-through-deref at every level** (new). Const propagates downward for free via the Dereference-staleness fix (a const pointer yields a const pointee). No per-level bitmask, no dual-position `T* const` syntax. The only escape is an explicit `as` cast (already permitted by `cast_valid`). This also makes Proposal C rule 4 sound at all depths (no partial-const type is constructible).
- **C — rules 1, 2, 4; comparisons stay permissive.** As decided; comparisons get a dedicated permissive pointer predicate decoupled from `types_coercible`.
- **D — cases (b) AND (c); retract docs; clean corpus.** Warn on wider+sign-change and same-width sign-change. Rewrite handbook §4/§15 to retract the "implicit sign flip is fine" blessing; add explicit `as` casts across `lib/std` and `demos` to keep builds warning-clean. `as`-cast is the per-site silencer; `-Werror`/suppression deferred with a doc note.

Implementation order: **A → B → C → D** (B before C; C rule 4 needs B's deref fix).

---


**Scope:** Four related but separable language-surface changes: (A) linkage keyword split, (B) const pointee protection, (C) pointer coercion tightening, (D) a new signedness-widening warning. B and C share a coercion mechanism and interact; A and D are independent of the others.

---

## 0. Motivating context: `public` is a three-way overload

`public` currently means three unrelated things depending on where it appears:

1. **Linkage** — on a top-level `fn` or global var, `public` gives the symbol external linkage (survives `globaldce`, visible to the linker/C). Default is private/internal linkage.
2. **Module visibility** — `public type` exports a type-struct so other modules' `$import` can name it; a plain `type` is only usable inside its defining module.
3. **Struct-member visibility** — `public` on a field or method inside a type-struct body opts it into access from outside the type (encapsulation); private is default.

Only meaning #1 is in scope for proposal A. #2 and #3 keep using `public` as-is.

`public extern fn` (`src/parser/program.rs:78-83`) and `public` on an `alias` (`:96-102`) are already errors today. `main`/`_start` are force-external via `IMPLICITLY_PUBLIC` (`src/codegen/functions.rs:27`), independent of keyword spelling.

---

## Proposal A — split the linkage meaning out of `public`

### Two naming alternatives

- **A1**: add a new keyword `export`. `export fn foo(...)`, `export i32 counter = 0` replace `public fn`/`public i32` for linkage. `public` stops being usable for linkage.
- **A2**: rename current `extern fn` (body lives in another object file) to `foreign fn`, and repurpose `extern` to mean external linkage.

### Reviewer recommendation: **A1**

- A2's rationale doesn't hold up: in C, top-level symbols are externally linked *by default*, and `extern` is overwhelmingly used to *declare* a symbol defined elsewhere — exactly Aspect's **current** `extern fn` meaning. A2 flips an already-shipped keyword against both its current Aspect meaning and its most common C usage — a worse mnemonic trap, not a better one.
- A2 isn't a pure rename at the grammar level either: `extern`/`asm`/`naked` sit on one mutually-exclusive "kind" axis (`Parser::parse_kind_modifier`, `src/parser/program.rs:189-213`) today. Repurposing `extern` for linkage moves it onto the orthogonal `vis`/`Visibility` axis (parallel to `public`, `:67-71`), while the renamed `foreign` meaning stays on the "kind" axis — a structural split, with open sub-questions (what does `foreign asm fn` mean? does `extern foreign fn` need the same rejection `public extern fn` gets today?).
- A1 needs none of that: `export` slots in exactly where `public` already sits, so `export extern fn` / `export asm fn` / `export naked fn` inherit today's rules verbatim.

### Required changes (not optional)

1. **What happens to old `public fn` / `public i32 x`?** Must be a **hard compile error** pointing at `export`, not a silent no-op — silently dropping linkage would fail invisibly at the *linker* stage instead of in the compiler's own diagnostics.
2. Blast radius is small: zero linkage `public` uses in `lib/std` (its only `public` hits are `public type`, unaffected). In-repo linkage `public` appears in `tests/programs/public_fn.ap`, `tests/programs/public_global.ap`, `tests/modules/shapes.ap`, `tests/programs/attributes_inert.ap`, and the failure fixture `tests/programs/failures/public_extern_fn.ap` (its expected-message fragment needs updating too).
3. Docs: `doc/handbook.md` §8 "`public` — exporting a symbol" and `doc/compiler/09-syntax-reference.md` § Visibility both need full rewrites.
4. No conflict with `const fn` or struct-member `public` — confirmed orthogonal under either option.

### Decision

**Chosen option (A1 / A2):** export becomes a keyword for *external linkage*, while *public* now defines visibility. This gets rid of nested submodules previously needed to obscure functions that we didn't want to export.

**What happens to bare `public fn`/`public i32` after the change:** Due to the changes in how public works, these functions still compile, however, they're now no longer externally linkable, just visible outside the module. This allows for very granular and verbose definitions of the way symbols are exported. Symbol example: `public export const u32 VALUE = 0` is a constant visible outside the module (by importing the module elsewhere) and is visible to other Translation Units. 

---

## Proposal B — `const` should protect the pointee, not just the binding

### Current state (verified empirically, not just read)

`const`-ness is a single `is_const: bool` field on `LangType` (`src/lexer/tokens.rs:195`) — one flag for the *whole* type, not per pointer-indirection-level. `VarAssign` and `FieldAssign` check it and reject writes (`src/typechecker/checker/statements.rs`); **`DerefAssign` (`*p = value`) never checks `is_const` at all** — confirmed by direct test: `const i32 *p; *p = 20;` compiles and runs today with no error.

Structural gap: `const` is fused into a single token at the **lexer** level (`Scanner::scan_type_after_const`, `src/lexer/scanner.rs`) and only succeeds when the token after `const` is a built-in scalar base-type spelling. `const Point* cp = &p` **fails to parse today** ("Expected expression") — confirmed empirically. The only place a const struct-typed value is ever constructed is the hand-rolled `this` parameter inside `const fn` (`src/parser/declarations.rs` `parse_method`). This lexer/parser gap must be fixed as part of this proposal, or pointee-protection is inexpressible for the most natural use case (`const Node* n` guarding a struct's fields).

`resolve_field`'s const propagation **does** compose through chains already — confirmed: a `const fn` correctly rejects `this.next.value = 99` today. That part needs no new work.

### Proposed change

Extend `DerefAssign` to consult the pointee's `is_const` and reject the write; fix the lexer/parser so `const` works before named/struct base types.

### Reviewer findings — required fixes, found by tracing the checker (not in the original ask)

1. **Soundness hole: `*this.field = x` bypasses the protection.** `synth_expression`'s `Dereference` arm (`src/typechecker/checker/expressions.rs:87-103`) returns the stale **parse-time-stamped** `default_type` instead of recomputing from the just-synthesized `inner_type`. The parser's own field-access stamp (`parse_dot_postfix`, `src/parser/expressions.rs:1200-1216`, "best-effort field-type stamp") does not apply the base's `is_const` — only the checker's `resolve_field` does. Net effect: `this.next.value = x` is correctly rejected in a `const fn` (verified), but `*this.next = someNode` would read the wrong (non-const) stamp and be silently allowed — a bypass route around the exact thing this proposal adds. **Fix required as a co-requisite:** the `Dereference` synth arm must recompute `expr.expr_type` from `inner_type` (pointer_depth − 1, propagating `is_const`) instead of trusting the parse-time stamp. Audit other lvalue paths reachable through `Dereference` (subscript desugars to `*(base + i)`) for the same issue.
2. **Depth ≥ 2 amplifies the single-const-position ambiguity.** Confirmed empirically: `const i32 **pp` stamps `is_const: true` on the whole depth-2 type, and each `*` in a chain copies the flag forward unchanged. A naive `DerefAssign` check would reject **both** `*pp = q` (reassigning the inner pointer) **and** `**pp = 5` (writing the int) — not just the innermost value, unlike C's `const int **`. Needs an explicit decision (see below).

### Historical precedent — must be addressed, not silently ignored

`TODO.md`'s Done table records that this project **already tried meaningful const enforcement once and reverted it**, after it caused a 396-macro `$define` leak (documented as "also made `const` free again, which killed" that leak). This doesn't kill the proposal outright — write-through-`DerefAssign` is a different mechanism than whatever drove that episode — but the revised proposal needs to name what was different last time and why this attempt won't reproduce the same pain, especially since [Proposal C's rule 4](#proposal-c) lands on the same const-coercion surface.

### Open questions

**Q1.** Aspect currently has no way to express C's `T* const` (mutable pointee, const pointer) as distinct from `const T*` (const pointee, mutable pointer binding) — a single `const T*` under this proposal would block *both* rebinding `p = other` (already true today) and writing `*p = x` (new). Is collapsing these into one strictly-stronger `const` acceptable for v1, or does full C-style dual-const-position parity need to be in scope now to avoid a breaking re-design later?
**Decision:** dual-const-position parity needed.

**Q2.** For depth ≥ 2 (`const i32 **pp`): should const "infect every level once written" (blocks both `*pp = q` and `**pp = 5`) as acceptable v1 semantics, or should this proposal scope itself to depth-1 pointers only for now and defer multi-level const to a follow-up?
**Decision:** propagate const downward

**Q3.** What was different about the prior const-enforcement attempt that led to the 396-macro-leak revert, and why won't `DerefAssign`-checking reproduce that pain? (This answer should also cover Proposal C's rule 4, which touches the same mechanism.)
**Decision:** I just coded it sloppily, honestly.

### Required for implementation regardless of the above

- `cast_valid` already permits unconditional pointer-to-pointer casts (`src/typechecker/types.rs:141-151`) regardless of const — the `as` escape hatch this proposal needs already exists; no codegen/cast changes required.
- Doc rewrites (not annotations): `doc/handbook.md` §15's documented pitfall that `*s = 90` is "allowed and unchecked" becomes false and must be rewritten with a new example; same for `doc/compiler/09-syntax-reference.md`'s "`const` is a binding qualifier" passage.
- New tests: the parser fix (`const NamedType`) as its own runtime test; a `DerefAssign`-rejects-const failure fixture; a chain case (`*this.next = x` in a `const fn`) exercising the Dereference-staleness fix; a depth-2 case exercising Q2's resolution; a positive test confirming non-const chains are unaffected.

---

## Proposal C — tighten pointer-to-pointer coercion, narrow the `u0*` exception

### Current state (verified against `types_coercible`, `src/typechecker/types.rs:14-75`)

Any two pointers of matching depth coerce implicitly regardless of pointee type (`i32* -> u8*` needs no cast) — `:58-61`. Separately, `u0*` at depth exactly 1 bridges **any** depth in **both directions** — `Point*`, `Point**`, `Point***`, … all coerce to/from `u0*` with no cast — `:42-51`.

### Proposed rules

1. Remove the general "matching depth, any pointee type" implicit coercion. `T* -> U*` (T ≠ U) requires an explicit `as` cast.
2. Narrow the `u0*` exception to depth-1-only, one direction: depth-1 `T*` coerces implicitly to `u0*` (type erasure, matches C's `T* -> void*`). `T**` and deeper no longer get this treatment.
3. The reverse (`u0* -> T*`, any depth) is **not** implicit — requires a cast. Removes today's `Point* p = some_u0_ptr` idiom.
4. Const and pointee-type get the same free-to-add / cast-to-remove treatment: `T* -> const T*` stays implicit, `const T* -> T*` requires a cast.

### Reviewer pushback on rule 3 — needs your call

Rule 3 isn't just cast-churn — it **contradicts the stdlib's own documented design**. `lib/std/mem/generic.ap`'s doc header states outright that allocation results should land in any pointer variable untyped ("everything traffics in `u0*`"). 79 occurrences of `u0*`/`u0 *` and 34 `malloc`/`zalloc`/`realloc` call sites exist across `lib/std`, plus 4 demos. This also breaks the handbook's own canonical example (`u0 *raw = malloc(...); i32 *xs = raw;`).

**Reviewer's recommendation:** keep rules 1–2 (they close the real footguns: untyped same-depth coercion, unbounded-depth `u0*` bridging) but reconsider rule 3, since `void* -> T*` implicit is the one direction C itself already allows, and `u0*` already self-documents "no pointee claimed" at the type level — unlike two concrete-but-wrong pointer types.

### Interaction not addressed by the original ask

The same coercion gate (`types_coercible`) is reused by `binary_op_types_valid` (`src/typechecker/checker/expressions.rs:629-643`) for pointer *comparisons* (`==`, `!=`, `<`, etc.), not just assignment/binding. Tightening rule 1 therefore also newly requires a cast just to *compare* two differently-typed pointers (`Point* a; Node* b; a == b`). This may be desired, but needs an explicit call — comparing two typed-but-mismatched pointer values isn't necessarily the same hazard as binding one as the other.

### Open questions

**Q1.** Keep rule 3 as originally proposed (accept the `lib/std` migration and the stdlib-design contradiction), or adopt the reviewer's narrower recommendation (`u0* -> T*` stays implicit; only rules 1, 2, and 4 land)?
**Decision:** Go by Reviewer recommendation.

**Q2.** Should pointer *comparisons* get the same tightening as assignment/coercion, or should comparison keep a more permissive rule of its own?
**Decision:** Comparisons do not need this tightening.

### Required for implementation regardless of the above

- Doc rewrites (not footnotes): `doc/handbook.md` §4 "Pointers" and "The opaque pointer `u0*`", and `doc/compiler/09-syntax-reference.md`'s array-decay and `u0*` sections, all currently document the *opposite* of the new rules as canonical.
- Test plan: positive/negative regression per adopted rule, a failure fixture per new diagnostic, and a full corpus run (in addition to the `lib/std`/`demos` migration sweep) to confirm nothing else in `tests/programs/**` relied on a removed coercion.
- Heads-up, no action needed now: `doc/plans/Areas-And-Refinements.md` (design-accepted, not implemented) would eventually rewrite the same `malloc`-idiom call sites this proposal's rule 3 forces churn on, for unrelated reasons — worth a one-line cross-reference so a future implementer isn't surprised by a second rewrite of the same sites later.

---

## Proposal D — warn on implicit integer widening that changes signedness

### Current state (verified)

Implicit integer coercion is gated on width alone. Three cases exist, all silent today: (a) wider, same sign (`i32 -> i64`) — uncontroversial; (b) wider, different sign (`i32 -> u64`) — silent; (c) *same* width, different sign (`i32 -> u32`, bit-reinterpretation, not actually "widening") — also silent, and documented in `doc/handbook.md` §4 as the intended idiom for sign-agnostic bit manipulation (see `demos/types.ap`'s packed-RGBA example).

**There is currently no warning mechanism in the compiler at all** — confirmed (`grep -rn "eprintln\|Warning\|warn(" src/` finds exactly one unrelated CLI warning in `main.rs:240` for malformed `ASPC_*_FLAGS`). `generate_tests!()` only knows `Expected::ExitCode`/`Expected::ErrorFragments` — no warning-fixture convention exists yet.

### Proposed scope: warn on case (b) only

Reviewer agrees with scoping to case (b), not case (c): the docs explicitly document case (c) as intended, and the user's own "widened" phrasing is technically precise (width strictly increases in (b), not (c)). Warning on a documented idiom in a brand-new diagnostic class's first release would burn trust in the mechanism immediately. If (c) is wanted later, it should be a separate, opt-in check — a natural candidate once `doc/plans/Three-Hook-Metasystem.md`'s Rules hook lands — not folded into this warning's default scope.

### Required plumbing — this is the actually novel part

1. **Accumulator.** `TypeChecker::check_program` returns `Result<(), Vec<TypeCheckError>>` (`src/typechecker/checker.rs:107`) and discards everything on the `Ok` path. Needs a `warnings: Vec<Diagnostic>` field on `TypeChecker`, populated at the `types_coercible` call sites (`src/typechecker/checker/expressions.rs:463,476,601`), read by the caller after a successful `check_program`.
2. **Dual wiring is mandatory.** `src/main.rs`'s `build_program` and `tests/integration_tests.rs`'s `parse_and_typecheck` (`:40-55`) each construct their own `TypeChecker` independently — the test harness never goes through `main.rs`. `doc/plans/Three-Hook-Metasystem.md` §14.4 already documents this exact trap for the Rules hook ("driver-only wiring would leave the entire corpus blind"). Same lesson applies here verbatim.
3. **Corpus convention.** Extend the existing runtime-test annotation with an optional `# expected_warning: "frag"` line (asserted against captured stderr), rather than a parallel `warnings/` fixture directory — a warning doesn't block compilation, so it fits the dual `-O0`/`-O2` runtime-test shape better than the fatal `failures/` shape.
4. **Format/stream.** Stderr, not stdout (stdout carries `--print` IR and the program's own `print`/`println`), mirroring the existing `file:line:col: warning: <msg>` shape used by `TypeChecker::format_error` and the `aspc: warning:` precedent in `main.rs:240`.
5. **Exit code / `-Werror`.** State explicitly: a warning does not fail the build or change `aspc`'s exit code in v1; suppression/`-Werror` is deferred future work, not silently absent.
6. Docs: `doc/handbook.md` §4 and §15, plus the matching passage in `doc/compiler/09-syntax-reference.md`, currently assert "no warning mechanism exists" — needs rewriting.

### Open questions

**Q1.** Confirm scope stays case (b) only for v1 (reviewer's recommendation)?
**Decision:** No, scope also addresses c.

**Q2.** Does a future `-Werror`/suppression mechanism need to be scoped now (e.g. reserve a flag name, design the on/off surface), or fully deferred with just a note in the docs?
**Decision:** just a note for now

---

## Cross-proposal interactions

- **B's Dereference-staleness fix is a prerequisite for C's rule 4 being sound**, not just for B's own `DerefAssign` check: once `const T* -> T*` requires a cast (C rule 4), the same stale-parse-time-stamp gap in the `Dereference` synth arm could make a cast look unnecessary at a field-access-then-deref chain. **Sequence B ahead of C if both proceed.**
- **B (deref writes) and C's rule 4 (const-removal needs a cast) both land on the same const-coercion mechanism blamed for the historical 396-macro-leak revert** (see Proposal B's Q3). The merged proposal needs one shared answer for why this attempt won't recur, not two independent ones.
- **A is independent** of B/C/D — no shared code paths. Both A and B/C touch `doc/compiler/09-syntax-reference.md`'s "Notable constraints" chapter — sequence doc edits to avoid clobbering each other if implemented concurrently.
- `doc/plans/Areas-And-Refinements.md` doesn't conflict with any of the four, but its eventual allocator rewrite touches the same call sites as C's rule 3 — see the note under Proposal C.

---

## Suggested implementation order (pending decisions above)

1. **A** — independent, smallest surface, no coercion-mechanism risk.
2. **B** — including the Dereference-staleness fix, which C needs.
3. **C** — rule 4 depends on B's fix landing first.
4. **D** — independent of the others; lowest urgency, but establishes diagnostic infra other future work will want.

## Next steps

1. Fill in the `Decision:` blanks above.
2. Send this doc back to the `language-designer` subagent for a final confirmation pass — in particular Proposal B's Q3 (the 396-macro-leak history) and Proposal C's Q1 (rule 3) are the two answers most likely to change the shape of the implementation.
3. Once confirmed, implement in the order above; each proposal gets its own runtime/failure test fixtures per the "Required for implementation" notes.
