# The `quote` write surface — Transforms Slice 2b (design)

**Status:** Design only — no code. This is the deferred write surface for hook #2
(transforms). The bare `Ast.method(site, "c_str")` builder ships first with the
coercion proof (Transforms-Plan §6.2, Slice 2a "Stage C"); `quote` is its
ergonomic front and lands after. Builds on `doc/plans/Transforms-Plan.md`,
`doc/plans/Meta-Module-JIT-Interface.md` §8–10, and `doc/plans/Three-Hook-
Metasystem.md` §3/§12/§14.

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

- **Recognition.** `quote` is a soft keyword: `Identifier("quote")` immediately
  before `{`. A new primary-expression branch at the `OpenBrace` seam
  (`src/parser/expressions.rs:808`, before the generic identifier path) calls
  `parse_quote()`. Syntactically this fits the closed-grammar fence (§3), but is
  a **deliberate deviation**: an expansion fence banks its interior as raw tokens
  (foreign DSL); a `quote` interior is **host Aspect**, parsed by the real parser
  so it gets real syntax diagnostics with real positions. Document the distinction.
- **Legality.** `quote { }` parses anywhere an expression is legal; it is gated
  **semantically** to meta-fn bodies (`FunctionProto.meta_kind.is_some()`) by the
  existing meta-only gate (a `std/meta` symbol outside a meta fn is already an
  error). A `quote` in ordinary code is the same class of error.
- **Interior capture.** `parse_quote()` parses the interior with the real
  expression parser under a **quote-depth counter** on `Parser` (a counter, not a
  bool, so nested quotes are representable later). While `quote_depth > 0`:
  1. **`$(expr)` splice holes** — `TokenKind::Dollar` (already lexed) followed by
     `(`: parse `$( <expr> )` into `ExprKind::Splice(Box<Expression>)`. The inner
     expression is ordinary Aspect (runs in the handler, must evaluate to an
     `Expr` handle) and is *not* part of the template. Outside quote mode,
     `Dollar` in expression position stays the current error.
  2. **Method/field postfix defers** — `parse_dot_postfix` / `build_method_call`
     resolve dispatch from the receiver type today; in quote mode they cannot and
     must not, so `base.name(args)` emits `ExprKind::MethodCall` and `base.field`
     emits `ExprKind::FieldAccess` unconditionally (the checker, not the parser,
     decides method-vs-field later).
- **New AST nodes.** `ExprKind::Quote { template: Box<Expression> }` and
  `ExprKind::Splice(Box<Expression>)` (near `ValueBlock`, `ast.rs:133`). They must
  **stay out** of the frozen `meta_expr_kind` ABI enum in `meta.ap` — a
  `Quote`/`Splice` never survives into the user program; it lives only in the
  judge module and is desugared away.
- **v1 scope: expression quotes only** (`quote { <expr> }`) — all the
  `String -> u8*` proof needs. Statement/block quotes are a later sub-slice.

---

## 2. Desugaring & the construction API

**Approach — an AST→AST pass over meta-fn bodies** (mirrors the *philosophy* of
`inject_rule_trampolines`, but at the AST level, not synthetic source). It lowers
each `ExprKind::Quote` in a `meta_kind.is_some()` function into ordinary Aspect
AST: `FunctionCall`s to the `meta_ast_*` builtins, plus gensym `VarDecl`s.
Run once, after parse and before `elaborate_program`. Wins: the **normal checker
type-checks the desugared builder calls** (a `$(x)` where `x` isn't `Expr` is an
ordinary arg-type error at the `meta_ast_*` call, positioned at the splice — no
bespoke quote type-checker); codegen is untouched; positions survive as AST.

**Lowering rules** (`lower(t)`): `Splice(e)` → `e` verbatim; `MethodCall` →
`meta_ast_method[_args]`; `Variable(n)` → `meta_ast_var` (gensym local if a
template binder, else the literal name, unhygienic); `FunctionCall` →
`meta_ast_call`; `FieldAccess` → `meta_ast_field`; `Literal` → `meta_ast_*_lit`;
`Binary`/`Comparison` → `meta_ast_binary`; an arg list → `meta_exprlist_new()` +
`meta_exprlist_push` per arg.

**Construction API** (`H` = `u64` handle; new arena variant
`HandleData::ExprNode(Expression)` holds owned constructed nodes, distinct from
the rules' position-only `HandleData::Expr(Position)`; each builder stamps the
firing's demand-site `pos`):

| `Ast.*` wrapper | builtin | builds | slice |
|---|---|---|---|
| `Ast.method(base, name)` | `meta_ast_method(H, u8*) -> H` | `MethodCall{..,args:[]}` | A (baseline) |
| `Ast.method_args(base, name, args)` | `meta_ast_method_args(H, u8*, H) -> H` | `MethodCall` | A/E |
| `Ast.var(name)` | `meta_ast_var(u8*) -> H` | `Variable` | A |
| `Ast.call(name, args)` | `meta_ast_call(u8*, H) -> H` | `FunctionCall` | A |
| `Ast.field(base, name)` | `meta_ast_field(H, u8*) -> H` | `FieldAccess` | A |
| `Ast.int_lit / .str_lit / .bool_lit` | `meta_ast_*_lit(...) -> H` | `Literal` | A |
| `Ast.binary(l, op, r)` | `meta_ast_binary(H, i32, H) -> H` | `Binary`/`Comparison` | A |
| `ExprList.new / .push` | `meta_exprlist_new/push` | mutable builder list | A |
| `Ast.alloc / .struct_lit / .cast / .sizeof / .value_block` | later | `Alloc`/… | E |

**Minimum to retire the bare `Ast.method` call in the `String -> u8*` proof:**
just `Splice` + `MethodCall`(no args) — i.e. `meta_ast_method` (already the Stage
C baseline) plus the quote parse + lower. No other builder, **no hygiene**.

Open point: `Literal::String` is an index into the parser's string table
(`ast.rs:7`); a runtime-built node has no slot — needs a MetaCtx string pool the
outer codegen can resolve, or an owned-bytes literal variant. Settle before
`meta_ast_str_lit` ships.

---

## 3. Hygiene / gensym (§12)

Identifiers **bound inside** the quote (`VarDecl` names, loop vars — e.g. interp's
`__t`) are renamed so a spliced user variable of the same name isn't captured;
**free** identifiers resolve **unhygienically** (documented v1 honesty).

Scheme: at lowering, compute the template's binder set + per-binder in-template
use set (bound method/field *names* excluded — the §14.2 payoff). Per binder,
emit `u8* __g<k> = meta_gensym("<orig>")` once; pass `__g<k>` to the binder's
declaration and every use, so renaming is consistent and **unique per firing**
(`meta_gensym` mints `"<orig>$q<N>"` from a per-compilation `MetaCtx` counter).
Splice contents are never renamed (opaque, already-checked `Expr` handles).

Limits (v1 honesty): only lexical binders visible in the template are renamed;
free identifiers are unhygienic; splice-introduced binders are opaque; no
cross-quote hygiene. **The `String -> u8*` proof needs none of this** — hygiene is
exercised only once statement/block quotes land, so it is the *last* sub-slice.

---

## 4. Splice typing & re-check

- The incoming `site` handle and constructed nodes are both
  `HandleData::ExprNode(Expression)` (owned AST), not the position-only rules
  variant — so builders splice real structure. Splice type errors surface as
  ordinary arg-type mismatches at the desugared `meta_ast_*` call.
- **Demand-site `pos`.** On a firing, the checker stashes the demand-site
  `Position` in `MetaCtx`; every builder stamps it onto the constructed node, so a
  bad rewrite re-checks with diagnostics pointing at **user source** (spliced
  sub-nodes keep their own positions). Aligns with §14.1 ("positions are not node
  identities") and §11's source-mapping question.
- **Round engine.** The handler returns an `Expr` handle; the checker splices the
  `ExprNode`'s `Expression` at the coercion demand site (`expressions.rs:829`),
  bumps `rewrites`; next round `resolve_method_call` (`:493`) lowers the
  `MethodCall` one-shot (must **not** bump the counter — core lowering). After
  rewrite `.c_str()` is `u8*`, coercion succeeds, and the resolved call re-checks
  as a fixpoint — protected by `typecheck_is_idempotent_on_recheck`.

---

## 5. Staging (each sub-slice independently landable)

- **A — construction builders (Rust only, no quote).** `HandleData::ExprNode`,
  the `meta_ast_*` table, the mutable `ExprList` builder, `Ast.*` wrappers. Usable
  immediately as the bare-builder form; **this is Slice 2a's write primitive**.
- **B — `quote` parse** (expression quotes): `ExprKind::Quote`/`Splice`, the
  soft-keyword branch, quote-depth counter, `$(…)`, method/field deferral, the
  meta-fn gate.
- **C — desugar + typing.** The AST→AST lowering; quotes/splices type as `Expr`.
  **A + B + C is minimum-viable** to write `quote { $(site).c_str() }` and delete
  the bare `Ast.method` call — no hygiene.
- **D — hygiene/gensym** + `meta_gensym`. Needed once binders appear.
- **E — statement/block quotes** (`quote { stmt* }`, `Ast.vardecl/.value_block`,
  `Type` splices). Needed by `interp`/`@debug`, not by the coercion proof.

---

## 6. Risks / open questions / debt

- **Preprocessor `$(` collision (blocking for multi-line quotes).** `$` is a
  directive only at line start (`preprocessor/mod.rs:418`); a line-leading
  `$(subject)` inside a multi-line quote is misread as a directive. Fix: treat `$`
  as a directive **only when the next token is an identifier** (directive names
  always are). Single-line `quote { $(site).c_str() }` (mid-line `$`) is
  unaffected, so the proof (A–C) needs no preprocessor change.
- **Fence-model deviation** (§3): document host-parsed interior vs. raw-token
  expansion so the governance story stays coherent.
- **`Literal::String` representation** (§2 open point) — settle before Slice A
  ships `meta_ast_str_lit`.
- **`Type` splices** (`@debug`'s `$(subject.type())`) — v1 is `Expr`-splice only;
  deferred with Slice E.
- **`MethodCall` privacy carve-out** (§14.2): constructed `MethodCall`s bypass the
  `public type` cross-module gate (no `file_id→module` map in the checker) — an
  accepted, inherited v1 hole.
- **Owner decisions:** the string-literal representation; `Ast.*` as statics vs.
  free functions; the `BinaryOp` tag enum surface; whether the desugar pass lives
  in the meta pipeline or the judge-build path; source-mapping policy.
- **Test/doc debt:** a runtime coercion proof re-expressed as
  `quote { $(site).c_str() }`; failure fixtures (quote outside a meta fn, non-`Expr`
  splice, ill-typed rewrite pointing at user source, a hygiene non-capture once D
  lands); `mcall`-style unit tests for quote-mode deferral + the desugar output;
  docs in `Meta-Module-JIT-Interface.md §8`, `doc/compiler/12-transforms.md`,
  `handbook.md`, `09-syntax-reference.md`, and the §15 Phase 0 checkbox.

### Critical files
`src/parser/expressions.rs` (quote-mode: `:808`, `:1150`, `:1242`),
`src/parser/ast.rs` (`Quote`/`Splice` near `:133`), `src/meta/jit.rs`
(`HandleData::ExprNode` `:34`, builders + `extern_bindings` `:376`, MetaCtx pos +
gensym counter), `lib/std/meta/meta.ap` (`Ast.*` + `meta_ast_*`),
`src/typechecker/checker/expressions.rs` (`try_repair` `:840`,
`resolve_method_call` `:493`), `src/preprocessor/mod.rs` (`:298`, `:418`).
