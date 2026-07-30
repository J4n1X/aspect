# The `quote` write surface — Transforms Slice 2b (design)

**Status (updated 2026-07-28):** **Slices A, B, C shipped** — `quote { <expr> }`
+ `$(...)` parse to the real Aspect parser in quote-mode, desugar to `Ast.*`
builder calls before typecheck, and drive the coercion proof
(`tests/programs/transform_coerce.ap`). §1–§4 below are being **revised in
place** for the next increment — Slices D (hygiene) and E (statement/block
quotes), now unified into one change (see §1's "Grammar unification") rather
than staged separately, because "void quotes lower to `Stmt`, value quotes
lower to `Expr`" only has one clean shape if every quote's body is the same
kind of thing. **Design-reviewed 2026-07-28: approved with changes**
(`language-designer`) — the review empirically disproved this revision's
original hygiene motivation (built and ran the repro; sibling value-blocks
with the same local name already work fine today, no hygiene needed) and
found the *real* bug (capture: a template binder's own declaration runs
before its initializer is checked, so it can shadow a same-named spliced
reference to an outer variable). That finding cascades: §3's mechanism
simplifies substantially (no runtime `meta_gensym`, no wrapping value-block —
a fixed compile-time rename is sufficient once the actual bug is capture, not
cross-firing collision), and §2's `Ast.block` target was wrong outright
(`ValueBlock` is unconditionally value-producing — verified against the
checker — so void quotes build `StatementKind::Block` instead). Six required
changes, folded into §2–§6 below. (The B/C review, folded into the previous
revision of this doc, is preserved in git history —
`git log -p -- doc/plans/Quote-Plan.md` — rather than kept alongside this
rewrite, since most of what it applied to is being superseded here.) Builds
on `doc/plans/Transforms-Plan.md`,
`doc/compiler/12-transforms.md`, `doc/plans/Meta-Module-JIT-Interface.md` §8–10,
and `doc/plans/Three-Hook-Metasystem.md` §3/§12/§14.

---

## 0. The load-bearing insight

`quote { … }` produces AST, and everything it produces already has a home: the
deferred `ExprKind::MethodCall` node (Three-Hook §14.2) was built precisely so
metaprogram-generated calls with no parse-time receiver type can defer dispatch
to the checker.

**A `quote` template is parsed by the real Aspect parser, but in a mode where
every construct that normally resolves at parse time from a receiver type
instead emits its *deferred* node.** `$(site).c_str()` cannot resolve `.c_str()`
at parse time (the template receiver `$(site)` has no type), so quote-mode
parsing emits `MethodCall { base: Splice(site), name: "c_str", args: [] }` — the
§14.2 node. The template is lowered to `meta_ast_*` builder calls that, at
runtime inside the JIT'd handler, reconstruct a real `MethodCall` in the user
program's arena, which the *outer* checker resolves at the demand site. That
symmetry — template `MethodCall` → builder → runtime `MethodCall` → outer-checker
resolution — is what makes `quote` small. Method/field *names* are never lexical
identifiers, so hygiene never touches them.

---

## 1. Surface & parse contract

**Unchanged from Slice B (shipped, not reopened by this revision):**

- **Recognition.** `quote` is a soft keyword: `Identifier("quote")` immediately
  before `{`, recognized at the identifier-then-`{` seam in `parse_primary`
  (`src/parser/expressions.rs`), guarded on
  `self.module.struct_id("quote").is_none()` so a real `type quote { … }` keeps
  its struct-literal form (matching the `rule`/`transform`/`meta` soft-keyword
  precedent).
- **Legality.** `quote { }` parses anywhere an expression is legal; it is gated
  **semantically**, not syntactically, to meta-fn bodies
  (`FunctionProto.meta_kind.is_some()`) by a dedicated `QuoteRefs` scan in
  `check_meta_gate` (`src/meta/mod.rs`) — a `Quote` node carries no symbol
  reference, so the gate's ordinary name-based scanning can't see it. `quote`
  outside a meta fn is a `meta-scope` error, not a parse error.
- **`$(expr)` splice holes.** `TokenKind::Dollar` followed by `(`: parses
  `$( <expr> )` into `ExprKind::Splice(Box<Expression>)`. The inner expression
  is ordinary handler-side Aspect (evaluated in the handler, must produce the
  `Expr` handle type), parsed with quote-mode *suspended* — it is not template
  content. Outside a quote, `$` in expression position stays the current error.
- **Quote-mode.** A `quote_depth: usize` counter on `Parser` (a counter, not a
  bool, so nested quotes are representable later). While `quote_depth > 0`,
  `parse_dot_postfix` defers method/field dispatch unconditionally — emits
  `ExprKind::MethodCall`/`FieldAccess` instead of resolving from a receiver
  type the template doesn't have — because it cannot resolve dispatch without
  one.

**New in this revision — grammar unification.** `quote { … }`'s body is no
longer a single bare expression; it is **always a statement sequence**, parsed
by looping the real statement parser — literally `parse_block_statement()`
called with the cursor already on `{` (the exact function an ordinary
`{ stmt* }` block statement already uses, including its error-recovery
`sync!`), then unwrapping the `StatementKind::Block(stmts)` it returns into
`stmts` directly. **This drops the shipped bare-expression form**:
`quote { $(site).c_str() }` becomes `quote { return $(site).c_str() }` (§5
covers the mechanical rewrite of already-shipped test files). The AST node
changes to match — `ExprKind::Quote { template: Box<Expression> }` becomes
`ExprKind::Quote { body: Vec<Statement> }`, holding the exact shape a
`ValueBlock` holds; no `QuoteBody` sum type, no "is this an expr-quote or a
stmt-quote" branch anywhere downstream. **Why now, not staged as before:** a
void quote must lower to a `Stmt`, a value quote to an `Expr` (§2) — those are
different `Ast.*` calls, and the *only* clean way to know which without a
second grammar is to look at the body's own shape, which requires the body to
already be a statement sequence in the common case too.

No new parsing work is needed for the individual statement shapes
(`VarDecl`/`Return`/expression-statement) — `parse_statement` already routes to
each one, and each one's own embedded expressions already thread through
`quote_depth` correctly (that machinery is Slice B's, unchanged). The one
**new** parser change: inside quote-mode, a bare identifier (not a splice, not
a method/field name) can name a template-local binder (a `VarDecl` earlier in
the *same* template) or a free identifier the constructed AST relies on
resolving wherever it's spliced (unhygienic by design, §3) — either way,
`parse_primary` cannot and should not try to resolve it against the handler's
own scope (`self.symbol_table`), so a bare identifier while `quote_depth > 0`
skips `variable_reference` entirely and becomes `ExprKind::Variable(name)`
directly, `LangType::UNRESOLVED`, deferred to `lower` exactly like
method/field dispatch already is.

**Value vs void, from the body alone.** A quote's body is **value-producing**
iff its *last* statement is `Return(Some(expr))`; anything else — ending in a
`VarDecl`/expression-statement, an empty body, or a `Return(None)` — is
**void**. v1 templates are flat (no `if`/`while`/`for`/nested `Block` inside a
quote — see §2's unsupported-shape list), so "last statement" is unambiguous;
a `Return` anywhere *but* last is a `ParserError::UnsupportedQuoteShape` at
lower time (dead code after a mid-body return would otherwise silently do
nothing, which is worse than an error). A value quote lowers to an `Expr`
(`Ast.value_block`); a void quote lowers to a `Stmt` (`Ast.block`) — see §2.

**New AST nodes.** `ExprKind::Quote { body: Vec<Statement> }` and
`ExprKind::Splice(Box<Expression>)` (near `ValueBlock`, `ast.rs`). Both stay
**out** of the frozen `meta_expr_kind` ABI enum in `meta.ap` — neither survives
into the user program; both live only in the judge module and are desugared
away before typecheck.

---

## 2. Desugaring & the construction API

**Approach — an AST→AST pass over meta-fn bodies** (mirrors the *philosophy* of
`inject_rule_trampolines`, but at the AST level, not synthetic source; shipped
as `src/meta/quote.rs::desugar_quotes`). Run once, after parse and before
`elaborate_program`. Wins carry over unchanged: the **normal checker
type-checks the desugared builder calls** (a `$(x)` where `x` isn't `Expr` is
an ordinary arg-type error at the `Ast.*` call, positioned at the splice — no
bespoke quote type-checker); codegen is untouched; positions survive as AST.

**`lower_body(body: Vec<Statement>)` — the new top-level entry point**,
replacing the old `lower(template: Expression)`. Every `ExprKind::Quote`'s
desugared replacement is still **one expression**, same as `desugar_expr`
already produces today (no change to how a `Quote` node gets replaced in its
parent) — but simpler than the original draft of this section proposed: no
wrapping value-block, no runtime state. (§3 explains why, following the
review's finding that the runtime-uniqueness machinery the first draft built
was solving a problem that doesn't exist.) `lower_body`:

1. Computes the body's **rename map**: for each `VarDecl` binder, `orig` →
   `orig + "$hyg"` — a plain, fixed, **compile-time** string substitution
   (`$` cannot appear in a real Aspect identifier, so no real name or
   independently-hygiened name can ever collide with one; §3).
2. Builds each template statement via the appropriate `Ast.*` call (below),
   substituting the rename map wherever a binder's original name would
   otherwise appear — its own declaration *and* every later reference to it —
   as an ordinary string-literal argument, exactly like a non-binder name
   already is (bound method/field names are never lexical, so they're
   untouched, same as §0).
3. Assembles the built statements with `StmtList.new()` + `.push(stmt)` per
   statement (mirrors the already-planned `ExprList.new`/`.push`, also
   still-unbuilt — this revision ships both).
4. Yields `Ast.value_block(list)` (body is value-producing) or `Ast.block(list)`
   (void) — whichever §1's value/void rule determined. **Not** the same
   underlying node — see "`Ast.block`'s actual target" below, a correction
   from review.

**`lower_stmt(s: &Statement) -> Result<Expression, ParserError>`** — lowers one
template statement into an expression that, when evaluated in the handler,
produces a `Stmt` handle:

- `Return(Some(e))` → `Ast.return_stmt(lower(e))`. Only legal as the body's
  last statement (§1); the value-quote case.
- `VarDecl { var_type, name, initializer: Some(e) }` → `Ast.vardecl(kind_tag,
  size_bits, pointer_depth, "<renamed>", lower(e))` — the renamed literal from
  step 1 above, see "vardecl's type" below. An **uninitialized** `VarDecl`
  (`initializer: None`) is unsupported this slice (the constructed `Stmt`
  needs a concrete initializer to build against; nothing today needs an
  uninitialized quoted local).
- `Expression(e)` → `Ast.expr_stmt(lower(e))` — a side-effecting statement, no
  bound value (e.g. a spliced `Expr` used only for its effect).
- Everything else (`If`/`While`/`For`/`Block`/`VarAssign`/`DerefAssign`/
  `FieldAssign`/`Break`/`Continue`, and a `Return` not in last position) is a
  `ParserError::UnsupportedQuoteShape` (already shipped in Slice C) — v1
  templates are **flat**: no branching, no mutation of a name bound outside the
  template. Failure fixture (already shipped, reused):
  `tests/programs/failures/meta_quote_unsupported_shape.ap`.
- **New failure fixture, from review item 6**: a body whose last statement is
  a bare expression-statement — exactly the pre-migration bare-quote shape,
  minus `return` — is a legal *void* quote under §1's rule, not an error; used
  where an `Expr` is wanted, it's an ordinary `Stmt`-vs-`Expr` type mismatch at
  the demand site, not a parse error. Add a fixture asserting this is a clean
  diagnostic (confirmed non-panicking) rather than silently accepted or
  confusingly worded — this is the single most likely mistake right after a
  breaking change to this exact syntax.

**`lower(e: Expression) -> Result<Expression, ParserError>` — the existing
expression-level lowering, extended by two cases.** `Splice(inner)` → `inner`
and no-arg `MethodCall` → `Ast.method(...)` are unchanged (shipped) **except**
one fix, from review's "minor" list, folded in here because it touches the
same arm: `Splice(inner)` must recursively desugar `inner` (i.e. run the same
find-and-lower-any-nested-`Quote` pass `desugar_expr` runs over ordinary code)
before returning it verbatim, not hand it back untouched. Today, quote-mode is
suspended (not raised) while parsing a splice's interior, so `$(quote { ... })`
already **parses** — a nested quote inside a splice — but ships un-lowered
into the final code, where it hits the checker's/codegen's `unreachable!()`
for a stray `Quote` (a compiler panic, not a diagnostic). Nested quotes are
still not a *supported* shape for v1 (§6), but "unsupported" must mean a clean
`UnsupportedQuoteShape` error, never a panic — this fix is a prerequisite for
that guarantee to actually hold, since after this revision `$(quote {...})` is
reachable through ordinary parsing, not just a hypothetical. New: `Variable(name)`
— reached only via quote-mode's new bare-identifier deferral (§1) — becomes
`Ast.var("<renamed>")` if `name` matches a binder in this body's rename map,
else `Ast.var("name")` (a literal string; **unhygienic by design**, §3).
Everything else `lower` doesn't implement (`Binary`/`Comparison`, a
`MethodCall` with arguments, `FieldAccess`, `FunctionCall`, `Literal`, …)
stays `UnsupportedQuoteShape`, same as shipped — this revision does not grow
the *expression*-level table, only adds the *statement*-level one above it.

**Vardecl's type (v1: scalar or single pointer-to-scalar only).** A template
`VarDecl`'s type is a literal token (`i32`, `u8*`, …), resolved by the
*ordinary* `parse_type()` at parse time — quote-mode adds nothing here, so by
the time `lower_stmt` runs it already has a concrete `LangType`. Encode it as
`(i32 kind_tag, i32 size_bits, i32 pointer_depth)` — `kind_tag` reusing the
frozen `TypeKind` ABI positions (`Meta-Module-JIT-Interface.md` §7:
`SInt`/`UInt`/`Bool` only for v1) — and **reject** at lowering time (not
runtime) anything else: `SFloat`, `Void`, `Struct`, `FnPtr`, `pointer_depth >
1`, arrays. Mirrors, and is slightly broader than (allows one level of
pointer, needed for `u8*`), the existing `is_meta_scalar` restriction on `meta`
globals (`src/parser/meta.rs`) — same v1-honesty shape, different feature.
**Deferred:** a `Type` splice (`$(subject.type())`) would remove this
restriction generally; not attempted this slice (§6).

**`Ast.block`'s actual target (review item 3 — the original draft was wrong).**
`ExprKind::ValueBlock` is **unconditionally value-producing** — verified
directly against the checker (`ValueBlockMissingReturn`/`ValueBlockVoidReturn`,
`checker/statements.rs`); there is no void variant of it, so a void quote
cannot lower to *any* shape of `ValueBlock`. Since `Ast.block` must return a
`Stmt` handle (not an `Expr`), its actual target is `StatementKind::Block(stmts)`
— the same node an ordinary `{ }` block **statement** already produces
(`parse_block_statement`, reused directly for quote's own grammar in §1). This
has different `return`-binding semantics than a value-block (binds to the
*enclosing function*, not "the innermost value block") — moot for v1's
restriction that a void quote's body contains no `Return` at all (only
`VarDecl`/expression-statements can appear once the body doesn't end in
`Return(Some(_))`, §1), but stated explicitly here since getting the node kind
wrong is exactly the kind of mistake this slice's own proof plan (a `rule fn`
reading the construction back through `.kind()`/`.children()`, §5 step 4)
would **not** catch — both `ValueBlock` and `Block` expose the same read-side
shape (a list of child statements) through the query API, so only an actual
splice-and-run (or a direct check of the constructed `StatementKind` variant)
would surface the error, and nothing consumes a `Stmt` splice yet (§4) to
force that check. Note this explicitly in the step-4 test itself.

**Demand-site position — a new `MetaCtx` field, not existing machinery.** §4
describes builders stamping the firing's demand-site `pos` onto constructed
nodes as though this were settled; it is not. Today's one shipped builder
(`meta_ast_method`) gets a correct position only *incidentally*, by inheriting
`base_node.pos` — which happens to equal the demand site because `base` **is**
the demand-site node for a coercion handler. New builders with no natural node
to inherit a position from — `Ast.var(name)` (a string-only argument), and
`Ast.value_block`/`.block` called on an empty list — need an actual stashed
position. Add `MetaCtx.demand_pos: Position`, populated in `fire_transform`
(`src/meta/jit.rs`) from `site.pos` before the handler runs. Per-builder rule:
inherit from an operand when one exists and is itself already-positioned
(`Ast.method`, `.return_stmt`, `.expr_stmt`, `.vardecl` — all take at least one
`H` operand); read `MetaCtx.demand_pos` when none does (`Ast.var`, empty
`.value_block`/`.block`).

**Construction API** (`H` = `u64` handle; the arena's `HandleData::ExprNode`
holds owned `Expression`s, already shipped — statements need the analogous
`HandleData::StmtNode(Statement)`; every builder's position rule is above,
not repeated per row):

| `Ast.*` / list wrapper | builtin | builds | status |
|---|---|---|---|
| `Ast.method(base, name)` | `meta_ast_method(H, u8*) -> H` | `MethodCall{..,args:[]}` | ✅ shipped |
| `Ast.var(name)` | `meta_ast_var(u8*) -> H` | `Variable` | **this slice** |
| `Ast.return_stmt(e)` | `meta_ast_return(H) -> H` | `Return(Some(e))` | **this slice** |
| `Ast.expr_stmt(e)` | `meta_ast_expr_stmt(H) -> H` | `Expression(e)` | **this slice** |
| `Ast.vardecl(kind, bits, ptr, name, init)` | `meta_ast_vardecl(i32,i32,i32,u8*,H) -> H` | `VarDecl` | **this slice**, scalar/`T*`-only |
| `Ast.value_block(list)` | `meta_ast_value_block(H) -> H` | `ExprKind::ValueBlock`, value-producing | **this slice** |
| `Ast.block(list)` | `meta_ast_block(H) -> H` | `StatementKind::Block` (not `ValueBlock` — see above), void | **this slice**, corrected |
| `StmtList.new()` / `.push(stmt) -> StmtList` | `meta_stmtlist_new() -> H` / `meta_stmtlist_push(H,H) -> H` | mutable, chaining builder list | **this slice** |
| `ExprList.new()` / `.push(expr) -> ExprList` | `meta_exprlist_new/push` | mutable, chaining builder list | **this slice** (planned since A, unbuilt until now) |
| `Ast.method_args(base, name, args)` | `meta_ast_method_args(H, u8*, H) -> H` | `MethodCall` w/ args | later |
| `Ast.call(name, args)` | `meta_ast_call(u8*, H) -> H` | `FunctionCall` | later |
| `Ast.field(base, name)` | `meta_ast_field(H, u8*) -> H` | `FieldAccess` | later |
| `Ast.int_lit / .str_lit / .bool_lit` | `meta_ast_*_lit(...) -> H` | `Literal` | later |
| `Ast.binary(l, op, r)` | `meta_ast_binary(H, i32, H) -> H` | `Binary`/`Comparison` | later |
| `Ast.alloc / .struct_lit / .cast / .sizeof` | — | `Alloc`/… | later |
| `Type` splices, non-scalar `vardecl` | — | — | later |

`StmtList`'s existing (shipped, read-only) `.count()`/`.at()` accessors stay;
`.new()`/`.push()` are new methods on the *same* Aspect type, not a new type —
mirroring how `ExprList` was always meant to serve both roles (§2's original
table already listed `.new`/`.push` as planned, just never built). Both
`.push()`s are **chaining** (return the same list, mutated), matching the
shipped `Ast.method`-style fluent surface, not a separately-observable
mutation — pinned explicitly per review's minor-item note that the prior
draft left this unspecified.

Open point: `Literal::String` is an index into the parser's string table
(`ast.rs:7`); a runtime-built node has no slot — needs a MetaCtx string pool the
outer codegen can resolve, or an owned-bytes literal variant. Settle before
`meta_ast_str_lit` ships.

---

## 3. Hygiene (§12)

**Corrected by review — this section's original motivation was empirically
wrong**, and the fix is a substantial simplification, not just a different
justification for the same mechanism. Kept as a worked example below rather
than deleted, because the wrong turn is instructive about what hygiene in
this system is actually *for*.

**The bug hygiene actually prevents: capture, not collision.** `VarDecl`
checking (`checker/statements.rs`) calls `define_var` **before**
`check_initializer` — so a template's own binder is visible, unbound, at the
point its *own initializer* is checked. A splice referencing an outer
variable that happens to share the binder's name reads the wrong thing:

```aspect
fn main(u32 argc, u8 **argv) -> i32 {
    i32 __v = 100
    i32 x = { i32 __v = __v + 1  return __v }   # inner __v shadows itself
    return x                                     # x is garbage, not 101
}
```

verified directly (built `aspc`, ran it): this returns garbage, not `101`,
because the inner `__v`'s own (uninitialized) slot is what `__v + 1` reads,
not the outer one. This is the shape a `quote { i32 __v = $(something) return
__v }` template produces once `$(something)` is (or references) a variable
named `__v` in the splicing scope — exactly the classic hygiene problem
(a macro-introduced binder capturing a free reference in spliced-in code),
and exactly what renaming the binder — *only* the binder, never the splice's
own content — fixes: `i32 __v_renamed = __v + 1  return __v_renamed` reads the
*outer* `__v` correctly, because the name that could shadow it no longer does.

**What hygiene does *not* need to solve (review's finding, verified): the
same handler firing twice in one scope.** The original draft of this section
motivated hygiene with `strlen(s) + strlen(s)` producing two sibling
`{ u8* __v = ... return __v }` value-blocks in one function, worried they'd
collide as a duplicate declaration. Built and ran exactly that shape — it
compiles and runs fine, unmodified, today. Two reasons, both structural: every
`ValueBlock` gets its own `enter_scope()`/`exit_scope()` (`checker/statements.rs`),
so sibling constructed blocks are already disjoint scopes, not one flat
namespace; and there is **no duplicate-variable check at all** outside the
*parser's own* transient symbol table (`symbol/table.rs`), which never runs on
programmatically-constructed or spliced AST regardless. So a same-scope
"collision" between two firings' locals was never actually reachable — hygiene
doesn't need to make each firing's rename *distinct from other firings'*,
only distinct from anything **real Aspect source could ever spell**.

**Mechanism, following directly from that correction — no runtime component
at all.** `lower_body` (§2) computes the body's binder set in one flat pass
(`VarDecl` names — v1 has no loops/nested scopes to walk into) and builds a
rename map: `orig` → `orig + "$hyg"`, a **fixed, compile-time** string
substitution — no counter, no per-firing uniqueness, because none is needed
(above). `$` cannot appear in a real Aspect identifier (it's the splice
sigil), so `"<orig>$hyg"` can never collide with anything a user, or another
independently-hygiened template, could write. Every reference to the binder
within the *same* template — its own declaration and every later `Variable`
use (§2's `lower`) — gets the renamed literal instead of the original;
splice content is never touched (it's opaque, already-checked `Expr`). This
removes the entire `meta_gensym` builtin, the `MetaCtx` counter, and the
wrapping value-block the original draft needed to hold its runtime-computed
name — `lower_body`'s output is exactly the construction-call chain §2
already describes, nothing wrapping it.

**Regression test — rebuilt around the actual bug.** A coercion handler
using `quote { i32 __v = $(site) + 1 return __v }`-shaped construction where
`site`'s expansion, once spliced, can reference a same-named `__v` in the
demand site's own scope (mirroring the worked example above, adapted to a
real `transform`) — passing only with the rename in place; reverting it
should reproduce the garbage-read directly, not merely fail a positive
assertion. This is the test §5 step 3 must build, not the two-firings shape
the original draft proposed.

**Same-named binder declared twice in one template — an accepted, pre-existing
gap, not something this slice introduces or fixes.** Following directly from
"no duplicate-variable check outside the parser's own table" above: a
template that itself writes `i32 x = 1  i32 x = 2` renames both to `x$hyg`,
and the second silently overwrites the first in the checker's scope stack
(`ScopeStack::insert`) rather than erroring — same as any other
programmatically-constructed AST with a repeated name, unrelated to hygiene
specifically. Flagged as an open gap (§6), not solved here.

Limits (v1 honesty, otherwise unchanged): only lexical binders visible in the
template are renamed; free identifiers are unhygienic; splice-introduced
binders are opaque; no cross-quote hygiene; no loops/nested scopes inside a
template to walk (v1 templates are flat, §1). `meta_gensym`/per-firing
uniqueness may become necessary later — not for hygiene, but if decoration
(hook #2b) ever splices a `Stmt` at a demand site *without* the natural
scope-isolation a `ValueBlock`/`Block` gives it; flagged as a forward-looking
open question (§6), not built speculatively now.

---

## 4. Splice typing & re-check

- The incoming `site` handle and constructed nodes are both
  `HandleData::ExprNode(Expression)` (owned AST), not the position-only rules
  variant — so builders splice real structure. Splice type errors surface as
  ordinary arg-type mismatches at the desugared `meta_ast_*` call.
- **Demand-site `pos`.** Every builder stamps a position onto the constructed
  node, so a bad rewrite re-checks with diagnostics pointing at **user
  source** (spliced sub-nodes keep their own positions) — aligns with §14.1
  ("positions are not node identities") and §11's source-mapping question.
  **Not yet a stashed `MetaCtx` field for every builder** — review item 4
  found this was true only incidentally for the one shipped builder
  (`Ast.method` inherits `base_node.pos`); §2 now specifies the actual
  `MetaCtx.demand_pos` field this slice adds and which builders need it.
- **Round engine.** The handler returns an `Expr` handle; the checker splices the
  `ExprNode`'s `Expression` at the coercion demand site (`expressions.rs:829`),
  bumps `rewrites`; next round `resolve_method_call` (`:493`) lowers the
  `MethodCall` one-shot (must **not** bump the counter — core lowering). After
  rewrite `.c_str()` is `u8*`, coercion succeeds, and the resolved call re-checks
  as a fixpoint — protected by `typecheck_is_idempotent_on_recheck`.
- **Void quotes have no round-engine consumer yet.** A `Stmt`-producing quote
  (`Ast.block`) would splice at a *statement*-level demand site — that's
  decoration (hook #2b, `@debug`), which doesn't exist. This slice's void path
  is proven independently, via a `rule fn` constructing one and inspecting it
  through the read-side query API (`.kind()`/`.children()`), not via an
  end-to-end artifact splice (§5's proof strategy).

---

## 5. Staging (each sub-slice independently landable)

- **A — construction builders (Rust only, no quote). ✅ Shipped.**
  `HandleData::ExprNode`, `meta_ast_method` + the `Ast.method` wrapper drive the
  coercion proof.
- **B — `quote` parse (bare-expression form). ✅ Shipped.**
  `ExprKind::Quote{template: Expression}`/`Splice`, the soft-keyword branch,
  quote-depth counter, `$(…)`, method/field deferral, the meta-fn gate.
- **C — desugar + typing (bare-expression form). ✅ Shipped.** The AST→AST
  lowering; `A + B + C` is what currently drives
  `tests/programs/transform_coerce.ap`.
- **D+E (this revision) — grammar unification, statement quotes, void quotes,
  hygiene, as one change.** Landable in the sub-steps below, each buildable and
  independently testable, but sequenced (E's grammar work has to land before D
  has anything to rename):
  1. **Grammar + AST unification (§1).** `Quote{template: Expression}` →
     `Quote{body: Vec<Statement>}`; `parse_quote` delegates to
     `parse_block_statement`; quote-mode bare-identifier deferral. Mechanically
     updates the four already-shipped `.ap` files using `quote { <expr> }` —
     verified by grep, exactly these and no others — to `quote { return
     <expr> }`: `tests/programs/transform_coerce.ap` and the three
     `tests/programs/failures/meta_quote_*.ap` fixtures, plus `handbook.md`'s
     example. **Regression gate:** every existing quote test must still pass
     after this step, mechanical rewrite only, before anything new is added.
  2. **Value-quote statement lowering (§2, no hygiene yet).** `lower_body`/
     `lower_stmt` for `Return`/`Expression`-statement only (no `VarDecl` yet,
     so no binders, so no hygiene needed yet), plus `MetaCtx.demand_pos` and
     the `Splice` recursive-desugar fix (both §2, needed regardless of
     `VarDecl`). Testable by re-expressing `transform_coerce.ap`'s handler as
     `quote { return $(site).c_str() }` — same shape as step 1's rewrite,
     confirms nothing regressed structurally.
  3. **`VarDecl` + hygiene together (§2, §3).** They land together because a
     `VarDecl` inside a quote is the only v1 binder — implementing lowering for
     it without the rename would ship the capture footgun (§3) on day one.
     `Ast.vardecl`/`.value_block` builders, `StmtList.new`/`.push`, the
     compile-time `orig` → `orig + "$hyg"` rename map — **no `meta_gensym`,
     no new runtime state** (§3's correction). **Proof:** a coercion handler
     whose constructed `quote { i32 __v = $(site) + 1 return __v }`-shaped
     rewrite lands where the demand site's own scope has a same-named `__v`
     in play, mirroring §3's worked repro — passes only with the rename;
     reverting it must reproduce the actual garbage-read (§3), not merely
     fail a positive assertion, the way `typecheck_is_idempotent_on_recheck`
     guards a different invariant with a real regression, not a smoke test.
  4. **Void quotes (`Ast.block` → `StatementKind::Block`, no round-engine
     consumer).** Proven via a `rule fn` that constructs one and inspects it
     through the query API (§4) — not an end-to-end artifact splice, since
     nothing consumes a `Stmt` rewrite yet. **Must assert the actual
     `StmtKind`/node shape**, not just that construction succeeds — §2's
     `Ast.block` correction notes the read-side query API can't distinguish
     a correctly-built `Block` from a wrongly-built `ValueBlock` by
     `.kind()`/`.children()` alone, so the test needs to check what `rule
     fn`-visible signal *does* distinguish them (e.g. `StmtKind::Block` vs.
     the parent expression's own kind, whichever the implementation exposes)
     rather than only checking children count/shape.

---

## 6. Risks / open questions / debt

- **Breaking the already-shipped surface, deliberately.** `quote { <expr> }`
  (no `return`) stops parsing; every shipped consumer moves to
  `quote { return <expr> }`. Acceptable — single-maintainer repo, this landed
  one session ago, nothing external depends on it — but it's a real surface
  change, not purely additive like A/B/C were, and should be called out as
  such at review rather than buried in a diff.
- **`HandleData::StmtNode(Statement)` — a new arena variant, mirroring
  `HandleData::ExprNode(Expression)`.** Needed the moment `Ast.vardecl`/
  `.return_stmt`/`.expr_stmt` exist (they return `Stmt` handles that later
  builders — `StmtList.push`, `Ast.value_block`/`.block` — consume). Same
  shape as the existing variant, same arena, no new lifetime story.
- **`src/meta/walk.rs` must change in step 1, not discovered as a compile
  error later.** Its `walk_expr` has `ExprKind::Quote { template: inner } |
  ExprKind::Splice(inner) => walk_expr(inner, v)` — dead the moment `Quote`'s
  field becomes `body: Vec<Statement>`. Correct replacement, mirroring the
  existing `ValueBlock` arm: `ExprKind::Quote { body } => body.iter().for_each(|s|
  walk_stmt(s, v))`, split out of the combined arm (`Splice` keeps the old
  shape). This walker is shared by **both** `check_meta_gate` and
  `QueryIndex` (`src/meta/query.rs`) — review item 5, folded into step 1
  since it's a mechanical consequence of the same field rename, not separate
  work.
- **Same-named binder declared twice in one template** — an accepted,
  pre-existing gap (§3), not solved this slice: no duplicate-variable check
  runs on constructed AST at all, hygiene or not.
- **`meta_gensym`/per-firing-unique renaming — explicitly not built this
  slice** (§3's correction). Would become necessary only if a future hook
  splices a `Stmt` at a demand site without a `ValueBlock`/`Block`'s natural
  scope isolation around it — flagged as a forward-looking open question for
  whoever builds decoration, not spec'd further here since decoration's own
  splice mechanism doesn't exist yet to design against.
- **Fence-model deviation** (§0): document host-parsed interior vs. raw-token
  expansion so the governance story stays coherent — carried over from B/C,
  still unresolved, still low-stakes (expansions don't exist yet).
- **`Literal::String` representation** (§2 open point, carried over from
  A) — settle before `meta_ast_str_lit` ships; unrelated to this revision.
- **`Type` splices** (`@debug`'s `$(subject.type())`) — would remove the
  scalar/`T*`-only restriction on `Ast.vardecl` (§2); not attempted this
  slice. Same for non-scalar `vardecl` generally (struct/array locals).
- **Branching inside a template** (`if`/`while`/`for`, nested `Block`) —
  v1 templates stay flat; unsupported-shape errors, not silently wrong.
- **Nested quotes** (`quote { ... $(quote {...}) ... }`) — parse successfully
  (quote-mode is suspended, not raised, inside a splice) but are not a
  *supported* shape; §2's `Splice`-recursion fix guarantees "unsupported"
  means a clean error there, not the panic it would have been pre-fix, but no
  attempt is made to give nested quotes real semantics this slice.
- **`MethodCall` privacy carve-out** (§14.2, carried over): constructed
  `MethodCall`s bypass the `public type` cross-module gate — an accepted,
  inherited v1 hole, unrelated to this revision.
- **Owner decisions:** the string-literal representation; the `BinaryOp` tag
  enum surface (still needed once `Ast.binary` ships); whether `meta_gensym`
  ever gets built, and if so on what trigger (above).
- **Test/doc debt:** the four-step landing sequence in §5 (each step is its
  own regression point, not just the final one, including the new "forgot
  `return`" fixture from §2); a `mcall`-style Rust unit test for
  `lower_body`'s value/void determination and the rename map (independent of
  the JIT — desugaring is a pure AST transform, testable without running
  anything); the capture regression test (§5 step 3) — reverting the rename
  must reproduce the actual wrong-read, not just fail a positive assertion;
  the hygiene proof program added to `typecheck_is_idempotent_on_recheck`'s
  corpus (minor, but cheap and this is genuinely new state-producing
  machinery); docs — `doc/compiler/12-transforms.md` ("The `quote` sugar"
  section), `handbook.md` (rewrite the `quote` example to the value-block
  form, document void quotes even without a consumer yet),
  `09-syntax-reference.md` (the grammar changed, not just grew), `03-ast.md`
  (`Quote`'s field renamed `template` → `body`, type changed), the two skill
  files (same rule as before — this is squarely inside "AST-rewriting surface
  in the language").

### Critical files
`src/parser/expressions.rs` (`parse_quote`, `parse_dot_postfix`, `parse_primary`'s
identifier arm), `src/parser/statements.rs` (`parse_block_statement`, reused
directly), `src/parser/ast.rs` (`ExprKind::Quote`), `src/meta/quote.rs`
(`lower_body`/`lower_stmt`/`lower`, all shipped-so-far logic lives here),
`src/meta/walk.rs` (`walk_expr`'s `Quote` arm — breaks on the field rename,
shared by `check_meta_gate` and `QueryIndex`), `src/meta/jit.rs`
(`HandleData::ExprNode`/new `StmtNode`, builders + `extern_bindings`, the new
`MetaCtx.demand_pos` field), `lib/std/meta/meta.ap`
(`Ast.*`/`StmtList`/`ExprList` + `meta_*` externs),
`src/typechecker/checker/statements.rs` (`VarDecl`'s `define_var`-before-
`check_initializer` ordering — the actual source of the capture bug §3's
hygiene fixes, not touched by this slice but load-bearing context for why the
rename is needed), `tests/programs/transform_coerce.ap` +
`transform_meta_counter.ap` (the mechanical rewrite + a home for the new
capture-based hygiene regression test respectively — the latter needs a
demand-site scenario, not just an extra firing, per §3/§5's correction).
