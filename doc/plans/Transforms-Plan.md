# Transforms (Hook #2) — First-Slice Plan

**Status:** Slice 1 landed 2026-07-22 (the inert round engine — see §5). Slice 2
(the `transform` / `allow coercion` surface) is still interface-first design and
gated on the owner decisions in §6 plus a `language-designer` review. This
plans hook #2 of the three-hook metasystem (`doc/plans/Three-Hook-Metasystem.md`
§6, §13, §14.1, §15 Phase 1 + Phase 4), building on the rules work already
landed (`doc/compiler/11-rules.md`, `Meta-Module-JIT-Interface.md` §10). Like
the `std/meta` design, this frames the decisions the owner reserves — above all
the **AST write surface** — rather than settling them.

---

## 1. Decision: transforms before expansions

The plan orders expansions (Phase 3) before transforms (Phase 4), but for a
*first working slice* transforms are the easier target. Both hooks are blocked
on the same unbuilt, unsettled prerequisite — the **AST construction / write
surface** (`quote` / `Ast.*`, deferred as the owner's call). The tiebreaker is
how *much* of it each needs to prove itself, plus how much new pipeline each
introduces:

| | Transforms (coercion) | Expansions (derive_eq) |
|---|---|---|
| Write-surface footprint | wrap one `Expr` in a call | build a whole function |
| New pipeline stage | none — fires inside the existing checker | a pre-parse **staging** stage + parser→JIT |
| Inert de-risking milestone | **yes** — round engine with no handlers is a no-op | no — staging/capture/write light up together |
| Reuses an existing AST node | **yes** — the checker-resolved `MethodCall` (Phase 0) | no |

The load-bearing point: the `String -> u8*` transform's entire output is
`$(site).c_str()` — a method call wrapping the demand-site expression. We
already have `ExprKind::MethodCall { base, name, args }` (added in Phase 0 so
metaprogram-generated calls can defer dispatch to the checker), and the checker
already lowers it. So the transform's write surface is **"return a `MethodCall`
node,"** and everything downstream is machinery we shipped. Expansions have no
such shortcut — they construct items.

We therefore take transforms out of plan order, explicitly. Expansions remain
next after this.

---

## 2. What ships (the first proof)

The `String -> u8*` coercion from Appendix A, end to end:

```aspect
# a String landing where a u8* is wanted is auto-rewritten to `.c_str()`
transform String -> u8* {
    fn handle(Expr site) -> Expr {
        return quote { $(site).c_str() }     # or, pre-quote: Ast.method(site, "c_str")
    }
}

allow coercion String -> u8*   # governance: the transform fires only where opted in
```

Given `append_cstring(name)` where `name: String` and the parameter wants
`u8*`, elaboration rewrites the argument to `name.c_str()`, re-checks, and the
program compiles — no hand-written cast. A `String -> u8*` in a module that did
**not** `allow` it stays a normal type error.

**Deliberately deferred to a later transform slice:** decoration transforms
(`@debug`), the full write surface / `quote`, and multi-round synthesis.

---

## 3. Prerequisites & status

1. **Round-based elaboration (Phase 1 — unbuilt).** The engine that lets a
   handler fire at a checker demand site and re-check to a fixpoint. This is the
   bulk of the work and the risk; §5 slices it so it lands *inert* first.
2. **A minimal AST write surface (unbuilt, partly unsettled).** Transforms need
   exactly one primitive for this slice: *given an `Expr` handle, produce a new
   `Expr` that is a method call on it.* Because the checker already resolves
   `ExprKind::MethodCall`, the Rust side can build that node directly; the
   `quote { $(site).c_str() }` sugar is optional and can come later. **This is
   the smallest possible bite of the surface the owner reserved** (§6).
3. **The judge / handle ABI (built).** Transforms are JIT'd Aspect, fired during
   elaboration, `Expr -> Expr`. This reuses the rules judge wholesale: the
   per-invocation `MetaCtx` arena, the `meta_*` externs, `add_global_mapping`,
   and the scalar-ABI trampoline pattern. New: the read surface must expose the
   demand-site `Expr` (and its `Type`), and the return path must accept a
   constructed `Expr` handle.

---

## 4. Architecture

### 4.1 Obligations (§6, §14.1)
An obligation is keyed `(kind, subject)`. Two trigger families share one
worklist: **repair** (seeded lazily when a type judgment gets stuck, e.g. a
failed coercion) and **decoration** (seeded eagerly from attribute sites). This
slice implements **repair only** — specifically the coercion key
`(coerce, From -> To)`. Decoration (`@debug`) is a later slice.

### 4.2 Round-based engine (§14.1) — replaces the worklist solver
The existing single-pass checker stays; a driver wraps it:

```
loop {
    fresh TypeChecker
    check whole program                     # handlers fire mid-pass at demand sites
    if rewrites_this_round == 0 { break }
    if round > bound { error naming the oscillating (kind, subject) }
}
# only the final round's errors are reported
```

At a demand site (`assert_coercible`, and later `UndefinedFunction` /
`UnknownField` in `src/typechecker/checker/expressions.rs`) that fails, the
checker consults the handler registry for the obligation key. If a handler is
registered, it invokes it, **replaces the `&mut Expression` node in place** with
the returned rewrite, and bumps `rewrites_this_round`; otherwise it stamps a
**poison / `Unresolved` sentinel** (to suppress cascade errors) and records the
obligation for the final diagnostic round.

**Load-bearing invariant:** re-checking an already-checked `Program` is a
fixpoint (idempotent). Guarded by a regression test: check, clone, re-check the
clone with a fresh checker, `assert_eq!`. The only existing structural rewrite —
the one-shot `MethodCall` lowering — must **not** bump `rewrites_this_round`
(it's core lowering, not a handler rewrite), or metaprogram calls would force a
spurious extra round.

### 4.3 Handler discipline (§6, non-negotiable)
- **At most one registered handler per key** — a second is a compile error.
- Handlers are **deterministic in output**; they may carry intra-compilation
  state, provided firing order is total and source-determined (lexicographic:
  round, then source position).
- Bounded rounds (compiler flag, sane default); non-quiescence is an error
  naming the oscillating obligation.

### 4.4 Governance gate (§13) — coercion is governed, not global
A user `From -> To` coercion is an implicit conversion (the C++/Scala footgun),
made worse by the flat module namespace. Two guardrails, both already expressible
with the rules machinery we shipped:
1. A coercion fires **only after built-in coercion fails** — the demand site is
   the built-in-coercion-failure point, so this is automatic.
2. It fires **only where a rule opts the module in**: `allow coercion String -> u8*`.
   This is a new governance declaration (a sibling of `rule`), scoped per module,
   read post-parse.

### 4.5 The handler ABI (reuses the judge)
`fn handle(Expr site) -> Expr`, JIT'd. The judge installs a `MetaCtx` whose arena
now also holds **constructed** nodes (owned, from the write surface) alongside
the borrowed read nodes. The handler receives an `Expr` handle for the site,
calls the (tiny) write primitive to build the `MethodCall` rewrite, and returns
its handle; the checker materializes the real `Expression` from the arena and
splices it in place. The scalar-ABI trampoline pattern carries over unchanged
(`Expr` is a `{u64}` handle, same byval/sret story).

---

## 5. Slices (incremental de-risking)

- **Slice 1 — round engine, no handlers (inert). ✅ Landed 2026-07-22.** The
  rounds driver (`src/typechecker/elaborate.rs`), an empty handler registry +
  `try_repair` consultation at the coercion demand site, the `Unresolved` poison
  sentinel, the `typecheck_is_idempotent_on_recheck` guard, and the `--max-rounds`
  flag. **Behaviourally a no-op:** the whole corpus stays green at -O0/-O2 and the
  idempotence guard passes. This isolated and proved the scary checker
  restructure *before any handler exists*. *Refinement made during implementation:*
  only the coercion demand site is consulted now — `UndefinedFunction` /
  `UnknownField` need `check_call` / `resolve_field` refactored to carry the `&mut`
  node, which is Slice 2 work for their repair handlers (matches §4.2's
  "`assert_coercible`, and later `UndefinedFunction` / `UnknownField`"); wiring
  non-splicing lookups there now would be dead plumbing that breaks in Slice 2. The
  obligation-recording + final-round undischarged-obligation diagnostic are
  likewise Slice 2 (they only bite once a handler can defer a site).
- **Slice 2 — the coercion transform.** The handler registry (one-per-key), the
  `transform From -> To { fn handle(...) }` surface, the `allow coercion` gate,
  the minimal write primitive (build a `MethodCall`), and the judge wiring to
  fire `Expr -> Expr` handlers at the coercion demand site. Ship `String -> u8*`.
- **Later slices (out of scope here):** decoration (`@debug`, eager attr
  obligations, consumed attributes), the full write surface + `quote` hygiene,
  handler-synthesized *items* (registered through `ModuleSymbols::add_function`).

---

## 6. Decisions the owner should settle

1. **Transform declaration surface.** Appendix A uses a block —
   `transform From -> To { fn handle(Expr) -> Expr }`. Your rule/expansion/
   transform-`fn` trio suggests `transform fn handle(Expr site) -> Expr` with the
   `From -> To` key bound separately. Pick the surface (and whether the checker
   fn is named `handle` or free).
2. **The write primitive.** Ship the bare "build a `MethodCall`" builder now and
   add `quote { $(site).c_str() }` as sugar later (recommended — smallest bite),
   or design a slice of `quote` up front. This is the reserved write-surface call,
   scoped as narrowly as any hook ever will.
3. **The governance surface.** `allow coercion String -> u8*` as a new top-level
   declaration (sibling of `rule`), or fold it into the rule grammar. Its module
   scope and how it's keyed to the transform.
4. **Bounded-rounds default** and whether the round bound is a `-D`/flag or fixed.

---

## 7. Risks & open questions

- **Checker idempotence** is the load-bearing assumption; Slice 1 exists to prove
  it in isolation. The known structural rewrite (`MethodCall` lowering) is
  already one-shot and idempotent — the risk is any *other* hidden in-place
  mutation the re-check isn't stable under.
- **Poison sentinel design** — `VOID` placeholders today spawn secondary errors;
  the `Unresolved` type must suppress them within a round and in the final round.
- **Determinism of firing order** across rounds (a node generated in round 2 at an
  early position fires after an original round-1 site at a later position — still
  total, still deterministic; document it).
- **Rule-engine entry point** — like rules, transforms must run from both
  `build_program` and the test harness's `parse_and_typecheck`, or the corpus is
  blind to them.
- **Whole-program re-check cost** is O(rounds × program) — fine under the
  whole-program-only constraint.

---

## 8. Testing & docs

- Slice 1: the idempotence guard test (`tests/integration_tests.rs`); the entire
  existing corpus stays green (proves inertness).
- Slice 2: a runtime fixture where a `String` auto-coerces to `u8*` under
  `allow coercion` and the program runs; failure fixtures for — a `String -> u8*`
  used *without* `allow` (stays a type error), two handlers claiming one key,
  non-quiescence hitting the round bound, and a coercion handler returning an
  ill-typed rewrite.
- Docs: extend `doc/compiler/11-rules.md` (or a new `12-transforms.md`) with the
  round engine + handler ABI; `doc/handbook.md` a `transform` section;
  `doc/compiler/09-syntax-reference.md` the `transform` + `allow coercion`
  grammar; `Meta-Module-JIT-Interface.md` the write-surface slice; mark
  `Three-Hook-Metasystem.md` §15 Phase 1 + Phase 4 progress.

---

## 9. Why this is low-risk given what's built

Transforms are mostly *assembly* of parts we already have: the judge, the
`MetaCtx` arena, the `meta_*` externs + trampoline (rules), the `MethodCall` node
+ its checker lowering (Phase 0), and the governance vocabulary (`rule` / a new
`allow`). The genuinely new engine — round-based elaboration — is landed **inert
first** and proven a no-op before a single handler runs. The write surface, the
part you reserved, is needed here at its **smallest possible scope**: one node
kind, already lowerable. That combination is why transforms, not expansions, are
the right next build.
