# Transforms (`src/typechecker/elaborate.rs`, `src/meta/jit.rs`)

Transforms are the second hook of the three-hook metasystem — **typed-AST
rewriting**. Two modes ship: **repair** (a coercion handler rewrites a stuck
`from -> to` demand site) and **decoration** (an attribute handler rewrites a
statement it is tagged on — see [Decoration](#decoration-transform-attr)). The
full design is
[`doc/plans/Three-Hook-Metasystem.md`](../plans/Three-Hook-Metasystem.md) and
[`doc/plans/Transforms-Plan.md`](../plans/Transforms-Plan.md); the `std/meta`
handle ABI is [`doc/plans/Meta-Module-JIT-Interface.md`](../plans/Meta-Module-JIT-Interface.md).

A transform lets a library opt one of its types into an implicit conversion it
controls, without loosening coercion for the whole language: when built-in
coercion `from -> to` fails at a demand site, a bound handler rewrites the site
and the program is re-checked.

## Where it runs

Transforms fire during **round-based elaboration**
(`typechecker::elaborate::elaborate_program`), which wraps type checking in both
entry points (`build_program` in `src/main.rs`, `parse_and_typecheck` in
`tests/integration_tests.rs`). A program declaring no coercion transforms takes
the fast path: a single checking pass. Otherwise the engine is JIT-built once
(before any round) and each round's checker consults it.

## The declaration

Two pieces, both parsed in `src/parser/program.rs` (`transform` is a soft
keyword, told apart by the token after it — `fn` → handler, else → binding):

- **Handler** — `transform fn to_cstr(Expr site) -> Expr { … }`, a meta function
  (`MetaKind::Transform`) with signature `(Expr) -> Expr`. It takes neither
  `public` nor `export`.
- **Binding** — `transform <from> -> <to> <handler>`, parsed into
  `Program::transforms: Vec<TransformDecl>` with `TransformKey::Coerce { from,
  to }`. The from-type's `parse_type` greedily consumes a fn-pointer type's own
  `->`, so the arrow that survives is always the key separator. A binding takes
  optional `public`: module-scoped by default, whole-program with `public`
  (mirrors `public type` / `public rule`). An `@attr` key
  (`TransformKey::Attribute`) also parses but does not fire yet.

## The round engine (`elaborate.rs`)

`elaborate_program` re-checks the whole program with a **fresh** `TypeChecker`
each round until a round rewrites nothing (the fixpoint) or `--max-rounds`
(default 16) is exceeded. Quiescence is detected by `TypeChecker::rewrites()`:
each transform splice bumps a counter, and a round with zero is final. Only the
final round's diagnostics are reported — an earlier round's errors may be
cleared by a later rewrite, so they are discarded.

At a stuck coercion (the `_` arm of `check_expression`), the checker forms an
`Obligation::Coerce { from, to }` and calls `try_repair`. If a handler claims
it, the rewrite is spliced in place, `self.rewrites` is bumped, and the node is
re-checked next round — where the checker's one-shot `MethodCall` lowering
resolves `site.c_str()` into a plain `String_c_str(site)` call. Once the
rewritten call's type coerces cleanly, the round rewrites nothing and elaboration
settles. A poisoned demand site is stamped `TypeBase::Unresolved`, which
`types_coercible` treats as coercible-to-anything so one stuck site does not
cascade secondary errors across the round.

### The module gate

`HandlerRegistry::lookup(from, to, module)` matches a handler by type pair
(same `base`/`size`/`pointer_depth`/`array_size`, ignoring `is_const` so a
`const T` site still fires a `T -> …` transform) **and** reach: a `public`
handler fires anywhere, a private one only for sites in its own module
(`file_modules[site.pos.file_id]`, threaded into the checker). This is the
transform twin of module-scoped rules.

## The write surface (`Ast.*`)

A handler's write primitives live in the `Ast` namespace in
`lib/std/meta/meta.ap`, all forwarding to `meta_ast_*` builtins
(`src/meta/jit.rs`) that read/build owned `HandleData::ExprNode`/`StmtNode`
entries (real, owned `Expression`/`Statement`s, unlike the position-only read
handles rules use): `Ast.method(site, "c_str")` builds `site.c_str()`;
`Ast.var(name)`/`.return_stmt(e)`/`.expr_stmt(e)`/`.vardecl(...)` build the
individual pieces of a statement sequence; `Ast.value_block(list)`/`.block(list)`
assemble a `StmtList` (`.new()`/`.push()`, a mutable chaining builder — same
pattern as the still-unbuilt `ExprList.new`/`.push`) into the two shapes below.
`Ast.*` is the bare form the `quote` sugar desugars to.

## The `quote` sugar (`src/meta/quote.rs`)

`quote { ... }`'s body is **always a statement sequence** — the same grammar
a `ValueBlock` uses (literally `parse_block_statement`, reused directly) —
value-producing iff the last statement is `return <expr>`, otherwise void.
`quote { return $(site).c_str() }` is sugar for a `StmtList`-built
`Ast.value_block` wrapping `Ast.method(site, "c_str")`. The load-bearing
insight, unchanged since the first slice: a quote template is parsed by the
*real* Aspect parser, in a mode where every construct that normally resolves
at parse time from a receiver type instead emits the deferred
`ExprKind::MethodCall`/`FieldAccess` nodes above, since the template has no
receiver type to resolve against. Full design in
[`doc/plans/Quote-Plan.md`](../plans/Quote-Plan.md); the pieces:

- **Parse** (`src/parser/expressions.rs`). `quote` is a soft keyword at the
  same seam as the struct-literal check in `parse_primary` (guarded on
  `struct_id("quote").is_none()`, so a real `type quote { ... }` keeps its
  literal form). `parse_quote` delegates straight to `parse_block_statement`
  under a `quote_depth` counter on `Parser`, which makes `$(expr)` parse as
  `ExprKind::Splice` (its interior parsed with quote-mode suspended — it's
  ordinary handler Aspect, not template content), makes `parse_dot_postfix`
  emit `MethodCall`/`FieldAccess` unconditionally instead of resolving
  dispatch, and makes a bare identifier (a template-local binder or a free
  reference) become `ExprKind::Variable` without a symbol-table lookup.
- **Gate** (`check_meta_gate`, `src/meta/mod.rs`). A `Quote` node carries no symbol
  reference, so it's invisible to the gate's other reference-scanning
  (a name to look up); a dedicated `QuoteRefs` scan flags one surviving in a
  function whose `meta_kind` is `None`. Because `quote` needs no
  `transform`/`rule` declaration anywhere in the file to parse, both call
  sites (`build_program`, the integration-test harness) always run the gate,
  rather than skipping it when `program.transforms` is empty.
- **Desugar** (`src/meta/quote.rs::desugar_quotes`). An AST rewrite over every
  `meta_kind.is_some()` function body, run once, after parsing and before
  `elaborate_program`. `lower_body` computes a hygiene rename map (below),
  then builds each statement via `lower_stmt` (`Return`/`VarDecl`/
  `Expression`-statement) and each expression via `lower` (`Splice`, a
  zero-arg `MethodCall`, a bare `Variable`), assembling the result with
  `StmtList.new().push(...)` and wrapping with `Ast.value_block`/`.block`.
  An unsupported shape (branching, a `MethodCall` with arguments, …) is a
  positioned `ParserError::UnsupportedQuoteShape`, not a panic.

Since desugaring runs before typecheck, the **normal checker type-checks the
desugared builder calls for free** — a non-`Expr` splice is an ordinary
arg-type mismatch at the `Ast.method` call, positioned at the splice, and a
void quote used where an `Expr` is wanted is an ordinary `Stmt`-vs-`Expr`
mismatch, not a bespoke diagnostic. Codegen never sees a `Quote`/`Splice`
(both are `unreachable!()` in the checker's and codegen's exhaustive
`ExprKind` matches, alongside the pre-existing `MethodCall` arm).

**Hygiene.** A `VarDecl` is the only binder a (flat, v1) template can
introduce. The bug it fixes is *capture*, not cross-firing collision: the
checker's `VarDecl` handling calls `define_var` before `check_initializer`, so
a template's own binder is visible, unbound, while its initializer is
checked — a splice referencing an outer variable that shares the binder's
name would read the binder's own (uninitialized) slot instead. The fix is a
fixed, compile-time rename (`orig` → `orig + "$hyg"`, computed once per
`lower_body` call) — not a runtime gensym: `$` cannot appear in a real Aspect
identifier, so the renamed form can never collide with anything real source
could write, and no cross-firing uniqueness is needed, since sibling
constructed `ValueBlock`s already get their own checker scope
(`tests/programs/transform_hygiene.ap` is the regression test, built around
this exact capture scenario).

**Void quotes.** `Ast.block`'s target is `StatementKind::Block`, not another
`ValueBlock` — that variant is unconditionally value-producing (no void
form), so a void quote needs the same node kind an ordinary `{ }` block
*statement* produces instead. Nothing splices a `Stmt` yet (that's
decoration, hook #2b, not built), so void quotes are proven independently: a
`rule fn` can construct one (`quote { ... }` is legal in any `meta_kind.is_some()`
function, not just `transform fn`) and inspect it through the read side —
`Stmt.kind()`/`.pos()` (`meta_stmt_kind`/`meta_stmt_pos`), the first two of
that Tier-1 query surface actually wired up, added specifically so this test
could assert the real `StmtKind` rather than just that construction didn't
crash (`tests/programs/transform_quote_void.ap`).

## The persistent engine (`with_transform_engine`)

The handler runs as JIT'd code, so its trampoline address is valid only while
the `ExecutionEngine` lives. `with_transform_engine` resolves the lifetime knot
by **inversion of control**: it builds the engine (a meta-only clone of the
program — meta functions + `std/meta`, user code dropped — typechecked
standalone so the handler is JIT-ready before round 1, then codegen'd and JIT'd
host-side with the `meta_*` builtins bound), resolves each handler's
`__rt_<name>` trampoline address into a `HandlerAddr`, and then runs the entire
round loop *inside* its own stack frame as a closure. `fire_transform` installs a
minimal `MetaCtx`, seeds the site as the sole handle, calls the scalar
trampoline `fn(u64) -> u64`, and reads back the returned `ExprNode`.

The trampoline `transform fn __rt_<name>(u64) -> u64` is injected by the
preprocessor (`inject_transform_trampolines`) into **its handler's own module**
(via a `$module` header) — a root synthetic file cannot call a handler private
to another module, which is what makes cross-module `public transform` work.

## Meta globals

A `meta <int-or-bool> <name>` global (`GlobalVar::is_meta`, parsed in
`src/parser/meta.rs`) is compile-time-only mutable state a handler can carry
across firings — e.g. a counter of how often a transform fired. The
implementation leans entirely on the persistent engine and existing codegen:

- **Storage is the judge module itself.** `with_transform_engine` retains
  `is_meta` globals in the meta clone (instead of clearing all globals), so the
  meta global is an ordinary defined global in the judge module. Because that
  module lives across the whole round loop, its value naturally survives every
  firing — no compiler-side cell, no `add_global_mapping`.
- **The artifact drops it for free.** In the artifact the meta global is a
  normal internal global referenced only by meta functions (`transform fn`,
  trampolines) — all of which `globaldce` strips — so it becomes unreachable and
  is stripped too. No codegen special-casing; `optimize` runs `globaldce` even
  at `-O0`.
- **Access is transform-scoped.** The meta-scope gate (`check_meta_gate`)
  rejects a `meta`-global reference — a `Variable` read or a `VarAssign` write —
  from any function that is not a `transform fn`, and from any ordinary global
  initializer. In v1 the live value exists only in the transform engine, so a
  `rule fn` could not read a meaningful value; rule/expansion (cross-hook) access
  waits on a unified persistent meta engine.
- **Determinism.** Firing order is unspecified, so meta globals are for
  order-insensitive aggregates. The *count* is deterministic for a convergent
  handler (each site fires once; a re-firing handler is a `RoundLimitExceeded`
  error, so a bad value never escapes). Elaboration precedes codegen and is
  `-O`-independent, so `-O0` and `-O2` compute the identical value.

Recorded design decision (not yet exercised): when cross-hook access lands,
**rules are read-only** on meta globals — a rule may read an aggregate to enforce
a cap but not mutate compile-time state (preserving "rules only diagnose").

## Decoration (`transform @attr`)

The second transform mode. Instead of repairing a stuck coercion, a decoration
handler rewrites a **statement carrying an attribute** it claims:
`transform @log log_it` binds the handler `transform fn log_it(Stmt) -> Stmt` to
the `@log` attribute, and every statement tagged `@log` is rewritten in place. A
binding takes optional `public` for whole-program reach, like a coercion binding.

### Where it runs — the eager pre-pass

Coercion fires *inside* the checker at a stuck demand site; decoration is seeded
**eagerly**. `run_rounds` runs `fire_decorations` before each round's checker: it
walks every function body (recursing into nested block/if/loop bodies), and for
each statement carrying a claimed attribute fires the handler, splices the
result, and **consumes** that attribute. Consumption mutates the persistent
`Program` — the checker is fresh each round but never reads `Statement.attrs`, so
a re-check cannot re-fire it. A round is quiescent when neither the pre-pass nor
the checker rewrote anything (`deco_rewrites == 0 && checker.rewrites() == 0`).

The pre-pass runs *before* the round's checker, so a round-1 handler sees an
untyped AST. The shipped value-threading form reads only structure
(`value_expr`), so this is sound; a future **type-directed** decoration (choosing
a rewrite from the subject's resolved type, like the plan's `@debug` picking
`__dbg_i32` vs `__dbg_str`) would need the pass reordered after a typing pass — a
boundary this slice deliberately stops short of.

### The handler + write surface

A decoration handler is `transform fn (Stmt) -> Stmt`, told from a coercion
handler by its signature and checked by `is_valid_decoration`. Its read/rebuild
primitives (`src/meta/jit.rs`, exposed on `Stmt` in `lib/std/meta/meta.ap`):

- `Stmt.value_expr()` — the statement's value expression: a `VarDecl`
  initializer, an assignment right-hand side, a `return` operand, or an
  expression statement. The **null handle** for a valueless statement.
- `Stmt.with_value_expr(Expr)` — the same statement (kind, position) with that
  value replaced.

The staple shape threads the value through a **zero-arg method** — the one call
form `quote` builds today:

```aspect
transform fn log_it(Stmt node) -> Stmt {
    Expr amount = node.value_expr()
    return node.with_value_expr(quote { return $(amount).logged() })
}
transform @log log_it
```

rewrites `Amount x = e` into `Amount x = { return e.logged() }`, where
`Amount.logged()` runs a side effect and returns the value so the binding keeps
its type. Because the handler returns a fresh node with no attributes of its own,
the **engine** owns attribute lifetime: after each firing it transplants the
surviving attributes onto the spliced statement.

### Stacked attributes, order, determinism

One attribute fires per statement per round, **innermost first** — the rightmost
in the source-order `attrs` vector, since `@a @b x` applies `b` first (§13,
outside-in). So `@a @b x` settles over two rounds (`b`, then `a`), each carrying
the survivor forward. Firing order across *different* statements is unspecified
(a handler's `meta` global is for order-insensitive aggregates, as with
coercion); a run is deterministic.

### Consume-hides-from-rules

A decoration that consumes `@x` removes it from the AST before `QueryIndex` is
built (post-typecheck), so a `rule @x` anchored on the same attribute would not
see decorated sites. Give a decoration and a rule that must both observe a site
distinct attributes.

### Failure

A value-threading handler applied to a statement with **no** value expression
(`value_expr()` returns null, so nothing usable is built) is a positioned
`DecorationFailed` at the decorated statement — a clean diagnostic, not a
mid-elaboration crash.

## Validation (before elaboration)

`validate_transforms` rejects, with a positioned `InvalidTransformKey`. For a
coercion key:

- a **dead** key (`types_coercible(from, to)` already holds — the handler could
  never fire);
- a key that only **removes `const`** (const removal must stay an explicit
  `as`, never an implicit transform);
- a **handler that is not `(Expr) -> Expr`**.

For an attribute key: a **handler that is not `(Stmt) -> Stmt`**
(`is_valid_decoration`). And for either kind: **two handlers claiming one key**
with overlapping reach (one key, one handler — ambiguity, not first-wins) and an
**undefined** handler.

A stuck coercion with *no* binding is left alone (an ordinary `TypeMismatch`); an
attribute with no claiming handler stays inert (an unclaimed attribute is not an
error).

## Gate ordering

When a program declares transforms, the meta-scope gate (`check_meta_gate`) runs
**before** elaboration in both entry points, so a misuse of the meta surface in
ordinary code is a clean `meta-scope` error rather than a mid-elaboration engine
failure.

## Artifact stays clean automatically

Meta functions (`transform fn`, the trampolines, `std/meta`) are compile-time
only. They carry internal linkage and `optimize` runs `globaldce`, which strips
them from the artifact — so a released binary never contains the handler or
references the unresolved `meta_*` externs. `optimize` runs even at `-O0` (it
only deletes unreachable symbols), so this holds at every level.

## Testing

- Runtime proof: `tests/programs/transform_coerce.ap` (a `Str` flows into a
  `u8*` extern → rewritten to `.c_str()` → 5), plus
  `transform_public_cross_module.ap` for whole-program reach.
- Failure fixtures (`tests/programs/failures/transform_*.ap`): no binding
  (stays `TypeMismatch`), dead key, const removal, non-convergence
  (`RoundLimitExceeded`), ill-typed rewrite, duplicate key, invalid handler,
  and a private transform not reaching another module.
- `typecheck_is_idempotent_on_recheck` includes the settled coercion program: a
  bare re-check (no handlers) must leave the already-rewritten `.c_str()`
  untouched.
- Meta globals: `tests/programs/transform_meta_counter.ap` (a handler counts its
  firings and picks a different rewrite each time; the result is order-robust),
  plus failures for a `meta`-global reference in ordinary code, in a global
  initializer, in a `rule fn`, and a non-integer/bool `meta` type.
- `quote`: the coercion proof above is itself a runtime test, expressed as
  `quote { return $(site).c_str() }`; `transform_hygiene.ap` is the capture
  regression (§ above); `transform_quote_void.ap` proves void quotes via a
  `rule fn`. Failures (`meta_quote_*.ap`): `quote` in ordinary code, a
  non-`Expr` splice, an unsupported template shape (a binary operator), and
  the "forgot `return`" footgun (a legal void quote surfacing an ordinary
  `Stmt`-vs-`Expr` mismatch, not a bespoke diagnostic).
- Decoration: `tests/programs/transform_decorate.ap` (`@log` threads two
  bindings through `logged` → 125), `transform_decorate_stacked.ap` (stacked
  `@log @trace`, proving survivor transplant → 20), and
  `transform_decorate_public_cross_module.ap` (public reach). Failures
  (`failures/transform_decorate_*.ap`): an `@attr` handler that is `(Expr) ->
  Expr` not `(Stmt) -> Stmt`, a duplicate `@attr` handler, and a value-threading
  handler on a valueless statement (`DecorationFailed`). The idempotence guard
  includes the settled decoration program.

## Explicitly deferred

- **Function decoration** (`transform @attr(fn) -> fn`) — statement decoration
  fires; the function-body form is a distinct key.
- **Type-directed decoration** — a handler choosing its rewrite from the
  subject's resolved type. The eager pre-pass runs *before* the round's checker
  (untyped AST), so this needs the pass reordered after a typing pass.
- `Type` splices (`$(expr.type())`, needed for a non-literal `Ast.vardecl`
  type), branching inside a template (`if`/`while`/`for`, nested `Block`),
  and the richer `Ast.*` construction surface (`.method_args`/`.call`/
  `.field`/`.binary`/`.int_lit`/…) that later template shapes need
  ([`doc/plans/Quote-Plan.md`](../plans/Quote-Plan.md) §6) — so a decoration
  today threads through a **zero-arg method**, not an arbitrary recorder call.
- A per-firing-unique rename (the original `meta_gensym` design) — not needed
  for hygiene (a fixed rename suffices, above), but may become necessary if a
  future hook splices a `Stmt` without a `ValueBlock`/`Block`'s scope
  isolation around it.
- A handler `Program` context (a mid-elaboration program is full of `Unresolved`
  poison; a round-start snapshot is future work).
