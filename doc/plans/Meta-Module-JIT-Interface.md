# `std/meta` — Module Interface & JIT-Side Contract (design)

**Status:** Interface-first design. This document + the accompanying Aspect
interface (`lib/std/meta/meta.ap`) define *what the compiler must expose* and
*what a metaprogram sees*. **No compiler code is written yet** — the `extern`
builtins named here are unimplemented; the interface forwards to signatures the
JIT side will fulfil later. Companion to `doc/plans/Three-Hook-Metasystem.md`
(this realises its §14/§15 "std/meta handle ABI" item).

**Scope of this pass:** the **read** surfaces of the whole system — the typed-AST
read/query/judgment surface that **rules** and **transforms** inspect (§6.1–6.9),
*and* the **raw-token** surface that **expansions** consume pre-parse (§6.10).
Enough to *inspect* a program (typed) or a captured token tree (untyped) and
*emit diagnostics*. The **write** surfaces are **deferred**: AST **construction**
(`Ast.*` builders) and **`quote { … }` / `$(…)`** — the output of expansions and
transforms — are not settled (§8), and in-place **rewrite** (transforms) is out
of scope. The kind-tags are real **enums** (§7) — the `enum` language feature was
added first so they could be type-safe from the start.

---

## 1. Decisions locked in (design session, 2026-07-21)

These were settled before this document and are treated as fixed constraints:

1. **Implicit load, no import.** `std/meta` is *not* `$import`-ed. Its types +
   `extern` signatures are injected into the compilation of a metaprogramming
   directive; they are in scope there and nowhere else. A normal function that
   names `Expr` simply gets "undefined type" — the natural gate. A nicer,
   self-hosted gate (a rule that rejects meta symbols escaping into normal code)
   lands once Phase 2a exists; we build no bespoke error machinery for it now.
2. **Handles are opaque, encapsulated.** Every special struct is `type T { u64 handle }`
   with a **private** `handle`; the raw index is never readable out of a node.
   Cross-type wrapping goes through a `public fn from_handle(u64) -> T` static
   (unavoidably public — no module-internal visibility tier exists — but it
   exposes *wrapping*, not *reading*).
3. **Handle lifetime = one directive invocation.** Handles are valid only while
   the owning metaprogramming directive runs. After it returns the arena is torn
   down. **Storing a handle across invocations is undefined behaviour**,
   documented and unenforced. The Rust side must still *validate* every incoming
   handle (out-of-range / stale → a clean compiler error, never a crash).
4. **Aspect methods forward to Rust externs.** The ergonomic surface is ordinary
   Aspect (real `type` structs + methods, resolved by the normal parser/checker
   — zero intrinsic-type special-casing). Each method is a thin forward to an
   `extern fn meta_*` implemented in Rust.

---

## 2. The seam

```
metaprogram (Aspect)                compiler (Rust)
────────────────────                ───────────────
expr.kind()                         MetaCtx (thread-local, per invocation)
  └─ meta_expr_kind(this.handle) ──▶  arena: u64 ─▶ &Expression / &Fn / …
                                      program:  &Program under query
       i32  ◀───────────────────────  source:  &[PathBuf] / string table
```

- One **`MetaCtx`** thread-local is installed around each directive invocation.
  It owns, for that invocation only: the **arena** (handle → borrowed compiler
  object), the `&Program` being queried, and the source/string registries needed
  to hand back `u8*` strings and positions.
- The `extern fn meta_*` builtins are registered on the judge/hook
  `ExecutionEngine` via `ExecutionEngine::add_global_mapping` before the
  metaprogram is invoked, and read `MetaCtx` from the thread-local.

---

## 3. ABI rules (the Rust↔JIT boundary)

| Aspect type at the boundary | Rust type | Notes |
|---|---|---|
| `u64` (a handle) | `u64` | arena index; **`0` = null/none**. Never pass a `{u64}` struct across the seam. |
| `u8*` (a string) | `*const u8` (NUL-terminated) | **borrowed**, valid for the invocation only; the metaprogram must not retain it (it can't — handles/strings die together). |
| `bool` | `bool`/`i1` | plain scalar. |
| `i32` / `u64` (a scalar result) | same | e.g. kind tags, counts, line numbers. |

- **Null-handle convention:** a builtin returning "no such node" returns `0`.
  Aspect wrappers may expose that as a predicate (`is_null()`) or a sentinel;
  callers must check before navigating.
- **Kind tags are a stable contract.** `meta_expr_kind`/`meta_stmt_kind`/
  `meta_type_base` return small `i32` tags that mirror the compiler enums
  (`ExprKind`, `StatementKind`, `TypeBase`). The tag values are frozen API — the
  Aspect side exposes them as named constants (§7); the Rust side must map its
  internal enum to these fixed integers, not to `as i32` of the enum.

---

## 4. Handle model / arena

- A handle is a `u64` index into the invocation's arena. The arena maps each
  handed-out index to a borrowed reference into the live `Program`/symbol tables
  (AST nodes are not copied — handles are cheap views).
- The arena is **populated lazily**: a query builtin that returns a child node
  interns that reference and returns a fresh handle. Identity is *not* promised
  — asking for the same child twice may return two handles to the same node
  (fine, since handles are opaque and non-comparable in the public API).
- **Validation:** every builtin bounds-checks its handle argument against the
  current arena and rejects a stale/foreign index with a compiler error routed
  through the normal diagnostic path — never a panic across the FFI boundary.
- **Teardown:** the arena is dropped when the invocation returns; all handles
  become invalid simultaneously.

---

## 5. Gating (implicit load) — how `std/meta` becomes available

Not `$import`. When the driver compiles a hook-providing module **standalone**
(§14.3 of the metasystem doc), it injects the `std/meta` interface into that
compile so the directive bodies — and any helper functions they call in the same
module — see the special structs and `extern` signatures. In the ordinary
whole-program stream the metaprogram declarations are inert and `std/meta` is
absent, so naming `Expr` there is an "undefined type" error (the natural gate).
The precise injection mechanism (a synthetic prelude vs. a preloaded module) is
an implementation detail deferred with the staging driver; **this pass only
fixes the interface, not the loader.**

---

## 6. The builtin catalog (what the Rust side must implement)

Grouped by special struct. Each row is one `extern fn`; the Aspect wrapper that
forwards to it lives in `lib/std/meta/meta.ap`. `H` denotes a `u64` handle.
This is the **read/query/judgment** surface; construction is §8.

### 6.1 `Program` — the query root

| `extern fn` | → | Semantics |
|---|---|---|
| `meta_program_functions(H prog)` | `H` (`FnList`) | all functions, definition order |
| `meta_program_globals(H prog)` | `H` (`GlobalList`) | all global variables |
| `meta_program_structs(H prog)` | `H` (`TypeList`) | all type-structs |
| `meta_program_call_sites_of(H prog, u8* name)` | `H` (`ExprList`) | every direct call to `name` (mangled `Type$method` names literal) |
| `meta_program_instantiations_of(H prog, u8* name)` | `H` (`ExprList`) | construction sites (struct-literal / `alloc`) of the named struct type — matches the Phase 2a `QueryIndex` "construction only" model (§10); the interface (`meta.ap`) takes the type **name**, not a `Type` handle |

### 6.2 Lists — one hand-monomorphized type per element (no generics)

Every `*List` (`FnList`, `GlobalList`, `TypeList`, `StmtList`, `ExprList`,
`AttrList`) exposes the same two builtins, named per list:

| `extern fn` | → | Semantics |
|---|---|---|
| `meta_<list>_count(H list)` | `u64` | element count |
| `meta_<list>_at(H list, u64 i)` | `H` | i-th element handle (`0` if out of range) |

### 6.3 `Fn`

| `extern fn` | → | Semantics |
|---|---|---|
| `meta_fn_name(H fn)` | `u8*` | source name (mangled for methods) |
| `meta_fn_is_public(H fn)` | `bool` | module visibility |
| `meta_fn_is_export(H fn)` | `bool` | external linkage |
| `meta_fn_is_extern(H fn)` | `bool` | body defined elsewhere |
| `meta_fn_param_count(H fn)` | `u64` | arity |
| `meta_fn_param_type(H fn, u64 i)` | `H` (`Type`) | i-th parameter type |
| `meta_fn_return_type(H fn)` | `H` (`Type`) | return type |
| `meta_fn_body(H fn)` | `H` (`StmtList`) | statements (`0` for extern/asm/naked) |
| `meta_fn_attrs(H fn)` | `H` (`AttrList`) | leading attributes |
| `meta_fn_pos(H fn)` | `H` (`Pos`) | declaration position |

### 6.4 `Stmt`

| `extern fn` | → | Semantics |
|---|---|---|
| `meta_stmt_kind(H stmt)` | `i32` | tag (§7 `STMT_*`) |
| `meta_stmt_pos(H stmt)` | `H` (`Pos`) | position |
| `meta_stmt_attrs(H stmt)` | `H` (`AttrList`) | statement attributes |
| `meta_stmt_as_expr(H stmt)` | `H` (`Expr`) | the expression of an expression-statement (`0` otherwise) |
| `meta_stmt_children(H stmt)` | `H` (`StmtList`) | nested statements (block / if / loop bodies), flattened one level |

### 6.5 `Expr`

| `extern fn` | → | Semantics |
|---|---|---|
| `meta_expr_kind(H e)` | `i32` | tag (§7 `EXPR_*`) |
| `meta_expr_type(H e)` | `H` (`Type`) | resolved type (post-typecheck) |
| `meta_expr_pos(H e)` | `H` (`Pos`) | position |
| `meta_expr_callee_name(H e)` | `u8*` | for a `FunctionCall`: the (mangled) target name (`""` otherwise) |
| `meta_expr_args(H e)` | `H` (`ExprList`) | call arguments (`0` if not a call) |
| `meta_expr_child_count(H e)` | `u64` | number of sub-expressions (operands/callee/base) |
| `meta_expr_child(H e, u64 i)` | `H` (`Expr`) | i-th sub-expression |

### 6.6 `Type`

| `extern fn` | → | Semantics |
|---|---|---|
| `meta_type_base(H ty)` | `i32` | tag (§7 `TYPE_*`) |
| `meta_type_pointer_depth(H ty)` | `u64` | indirection levels |
| `meta_type_is_const(H ty)` | `bool` | immutability |
| `meta_type_bits(H ty)` | `u64` | width for scalars (0 for `u0`) |
| `meta_type_struct_name(H ty)` | `u8*` | declared name for a struct type (`""` otherwise) |

### 6.7 `Attr`

| `extern fn` | → | Semantics |
|---|---|---|
| `meta_attr_name(H a)` | `u8*` | attribute name (`@foo` → `"foo"`) |
| `meta_attr_arg_count(H a)` | `u64` | number of arguments |
| `meta_attr_arg_at(H a, u64 i)` | `H` (`Expr`) | i-th argument expression |

### 6.8 `Pos`

| `extern fn` | → | Semantics |
|---|---|---|
| `meta_pos_line(H p)` | `u64` | 1-based line |
| `meta_pos_column(H p)` | `u64` | 1-based column |
| `meta_pos_file(H p)` | `u8*` | source file path |

### 6.9 `Judgments` — the out-channel (the only "write" a rule may do)

| `extern fn` | → | Semantics |
|---|---|---|
| `meta_judgments_new()` | `H` | fresh accumulator (one per invocation) |
| `meta_judgment_error(H js, H pos, u8* msg)` | `u0` | record an **error** (fails the build) |
| `meta_judgment_warn(H js, H pos, u8* msg)` | `u0` | record a **report/warning** (stderr, build continues) |
| `meta_judgments_count(H js)` | `u64` | how many recorded (for the driver) |

The driver reads the accumulator after the rule returns and renders each as
`file:line:col: rule <name>: <msg>` (mirroring `TypeChecker::format_error`), per
§15 Phase 2a. `Judgment { severity, pos, rule, message }` is the compiler-side
record; the `rule` name is attached by the driver, not the metaprogram.

### 6.10 Raw tokens — the **expansion** input surface

Distinct from the typed-AST surface above. An **expansion** (hook #1) runs
*pre-parse* and sees only the `TokenTree` captured inside its braces — no types,
no names, no resolved AST. This is the layer it reads.

| `extern fn` | → | Semantics |
|---|---|---|
| `meta_token_kind(H tok)` | `i32` | tag (§7 `TOKEN_*`) |
| `meta_token_text(H tok)` | `u8*` | the lexeme verbatim |
| `meta_token_pos(H tok)` | `H` (`Pos`) | position |
| `meta_tokentree_count(H tt)` | `u64` | token count |
| `meta_tokentree_at(H tt, u64 i)` | `H` (`Token`) | i-th token |
| `meta_tokentree_segments(H tt)` | `H` (`SegmentList`) | split an interior string literal into text runs + `{ident}` holes (the `interp` idiom) |
| `meta_segment_is_text(H seg)` | `bool` | text run vs. `{ident}` hole |
| `meta_segment_text(H seg)` | `u8*` | the run's text (for a text segment) |
| `meta_segment_hole_name(H seg)` | `u8*` | the identifier (for a hole segment) |
| `meta_segmentlist_count / _at` | — | the usual list pair |

`segments()` is a convenience over the raw token stream, oriented at string
interpolation; whether it belongs in **core `std/meta`** or in **`std/fmt`**
(closer to `interp`) is an open question (§9). The **output** of an expansion —
building AST via `quote`/`Ast.*` — is the deferred construction surface (§8), so
this pass exposes an expansion's *read* side but not (yet) its *write* side.

---

## 7. Kind enums (frozen contract)

The kind accessors return real Aspect **enums** (added 2026-07-21), so
`expr.kind() == StmtKind.Return` is a compile error, not a silent nonsense — the
whole reason enums went in first. The `extern fn meta_*_kind` builtins still hand
back the raw `i32` position; the Aspect wrapper casts it to the enum
(`meta_expr_kind(h) as ExprKind`). **Variant order is the ABI** — the Rust side
maps its internal enum onto these positions, so the JIT implementation must emit
these integers, and the contract may only *append*, never reorder:

- **`ExprKind`** (mirrors `ExprKind`): `Literal, Variable, Binary, Comparison,
  Reference, Dereference, UnaryNot, BitwiseNot, FunctionCall, Cast, Alloc,
  ListInit, FieldAccess, StructLiteral, FunctionRef, IndirectCall, SizeOf, Null,
  ValueBlock, MethodCall`.
- **`StmtKind`** (mirrors `StatementKind`): `VarDecl, VarAssign, DerefAssign,
  FieldAssign, Return, If, While, For, Block, Expression, Break, Continue`.
- **`TypeKind`** (mirrors `TypeBase`): `SInt, UInt, SFloat, Void, Bool, Struct, FnPtr`.
- **`TokenKind`** (coarse grouping of the lexer's `TokenKind`): `Ident, Integer,
  Float, String, Bool, Keyword, LangType, Punct, Newline, Eof`. Operators and
  punctuation collapse to `Punct`; read `token.text()` for the exact lexeme.

Adding a compiler AST variant is a breaking change to this contract and must
append (never reorder).

---

## 8. Deferred — explicitly out of scope for this pass

- **Construction API** (`Ast.*` builders: `meta_expr_binary`, `meta_stmt_return`,
  …). The write side. Rules never construct; only expansions/transforms do.
- **`quote { … }` / `$(…)`.** Special parser + desugar treatment; the
  implementation approach is unsettled (owner's call), so no builders are
  specified until it is.
- **Mutation / in-place rewrite** (transforms). Handle **garbage collection** or
  lifetime *enforcement* (v1 leaves dangling-handle use as UB). Per-handler
  **watchdog** and **libc import allow-list** (§ metasystem "v1 honesty").

---

## 9. Open questions to settle before implementation

1. **Null vs. predicate ergonomics.** Do wrappers expose `0`-handles as an
   `is_null()` predicate, an `Option`-like pair, or a documented sentinel? (Lean:
   a plain `is_null()` on each node type — no generics for `Option`.)
2. **String lifetime in practice.** Strings are borrowed for the invocation; is
   any builtin tempted to hand back a string that outlives the arena slot it came
   from? (Contract: all `u8*` are valid until the invocation returns, no longer.)
3. **`Ast` vs. `Expr`/`Stmt`.** The doc's examples use a bare `Ast` in
   construction contexts. On the *read* side we expose concrete `Expr`/`Stmt`;
   whether a unifying `Ast` handle is worth it here (vs. only for construction)
   is deferred with §8.
4. **Enums — resolved (2026-07-21).** Minimal C-style `enum` was added to the
   language first, and the kind-tags are now real enums (§7). Open sub-question
   only: whether `enum` should later gain explicit `= N` variant values (it ships
   auto-numbered `0,1,2,…`); the meta tags don't need it (variant order already
   matches the ABI).
5. **`segments()` home.** Core `std/meta` (as written) or `std/fmt` (closer to
   `interp`)? It is a string-interpolation convenience, not general token
   reading.
6. **Language-designer review.** This is a language-surface change; per repo rule
   it goes through the `language-designer` gate **before any compiler code is
   written** — i.e. once this interface + doc are approved by the owner.

---

## 10. Phase 2b — resolved execution model (2026-07-22)

Owner design session + `language-designer` review (Approved with changes)
resolved the open items for the first JIT'd-rules slice. This section is
authoritative where it differs from §§1–9 above.

### 10.1 Surface: the `rule fn` descriptor
Rule checkers are ordinary Aspect functions marked with a **hook-specific,
glanceable** descriptor: `rule fn <name>(...) -> ... { ... }`. The trio
`rule fn` / `expansion fn` / `transform fn` (latter two later) each name their
hook at a glance. `rule` is a **soft keyword**: `rule fn` is unambiguous because
`fn` is a keyword (never a global of a type named `rule`); it is distinct from a
`rule <anchor> <checker>` declaration and from `rule x = …`. A `rule fn` rejects
`public`/`export`/`extern`/`asm`/`naked`/attributes. AST: `FunctionProto.meta_kind:
Option<MetaKind>` (`MetaKind::Rule` today). *(Landed as sub-slice A.)*

A metaprogramming function has three properties, all keyed on `meta_kind.is_some()`:
`std/meta` is in scope in its body; it may **not** be called from ordinary code;
it is codegen'd into the JIT-only **judge module**, never the artifact.

### 10.2 Two LLVM modules (owner refinement — supersedes the §14.4 linkage knob)
Meta functions and the injected `std/meta` wrappers + `extern fn meta_*`
declarations are codegen'd into their **own** LLVM module — built for the **host**
target, JIT-only, **never emitted**. The artifact module contains only ordinary
code. Consequences: no linkage knob and no `globaldce` surgery (the judge module
is ours; we simply don't strip its entry points); the `public` `meta.ap`
forwarders can't leak into and break native links (they aren't in the artifact);
and the "judge must be host-target" requirement is satisfied by construction (a
program built with `--target i386-…` still JITs its rules for the host). Codegen
partitions `program.functions` by `meta_kind` / defining module (`std/meta`):
meta set → judge module, the rest → artifact.

### 10.3 The gate (supersedes the §1.1/§5 "undefined type" language)
Under conditional injection the special types are *present but tagged*, not
absent, so the gate is: a reference to a `std/meta` symbol (a type/function whose
defining module is `std/meta`) or a call to a meta function is legal **only inside
a meta-function body** (and inside `std/meta` itself). Elsewhere it is a
`"'Expr' is a meta-only type, usable only inside a rule fn"` error — a real
diagnostic, not "undefined type". A helper that touches meta types must itself be
a `rule fn` (meta fns may call meta fns); there is no other route to meta scope.

### 10.4 Conditional injection
`std/meta` is injected (its decls made available in the whole-program symbol
table) **only when the program declares a meta function** — ordinary programs keep
a clean namespace. Injection reuses the import machinery (a synthetic
`std/meta` load via the `-I` roots). A user `type Program` / `type Fn` / … then
collides with a reserved meta type → a clear `"'Fn' collides with a reserved
std/meta type"` error, not a raw `DuplicateType`. Meta types are interned at the
`prescan_type_names` stage so pass-2 `rule fn` bodies resolve them.

### 10.5 Calling convention (BLOCKING fix from review)
`rule fn f(Program, Type) -> Judgments` does **not** lower to `fn(u64,u64)->u64`:
single-field `{u64}` structs pass `byval` with an `sret` return (`is_struct_value`,
`src/codegen/functions.rs`). So the judge emits, per rule checker, a tiny
**`u64`-ABI trampoline** — `<name>__entry(u64 prog, u64 anchor) -> u64` that builds
the `{u64}` structs, calls the real meta fn via the true sret+byval ABI, and reads
the returned handle field. `get_function_address` targets the trampoline, whose ABI
genuinely is `fn(u64,u64)->u64`. Type-anchored checkers are exactly `(Program,
Type) -> Judgments`; the second param's type is the anchor-kind encoding.
Attribute-anchored checkers (`(Program, AttrList)`) are deferred — `rule @a <rulefn>`
errors "attribute-anchored rule functions are not yet supported" for now.

### 10.6 MetaCtx + arena
One thread-local `MetaCtx` per rule invocation owns: a `u64`-handle arena over the
live `&Program` and its `QueryIndex`, plus the source/string registries. `0` = null;
every builtin bounds-checks its handle and, on a stale/foreign one, sets an error
flag and returns `0`/null (**never** unwinds across the FFI boundary) — the judge
checks the flag after the meta fn returns and raises a normal diagnostic. The arena
is torn down when the invocation returns; storing a handle across invocations is UB,
documented and unenforced (§3). `QueryIndex` must retain `&Expression` (not just
`Position`) so `instantiations_of` can mint real `Expr` handles.

### 10.7 Resolution order
`rule <anchor> <name>` → builtin registry first (Phase 2a `singleton`/`audit`),
else an in-scope `rule fn` named `<name>` with a matching signature, else an error.
Specific diagnostics: an *ordinary* function used as a checker → "`<name>` is an
ordinary function; a rule checker must be a `rule fn`"; a genuinely unknown name →
did-you-mean over builtins **and** rule-fn names; a builtin-shadowed rule fn →
documented (builtin wins).

### 10.8 First-slice extern surface
Only what a JIT'd `singleton` needs: `meta_program_instantiations_of`,
`meta_exprlist_count`, `meta_exprlist_at`, `meta_expr_pos`, `meta_pos_line/column/file`,
`meta_type_struct_name`, `meta_judgments_new`, `meta_judgment_error`,
`meta_judgment_warn`. The remaining ~40 read-surface externs land incrementally.

### 10.9 Build order
- **A — `rule fn` descriptor** (parser/AST). *Landed 2026-07-22.*
- **B — injection + gate**: conditional `std/meta` injection, the meta-only
  reference/call gate, collision diagnostics.
- **C — JIT vertical**: `MetaCtx`/arena + the §10.8 externs, the two-module split +
  trampoline, `run_rules` dispatch to `rule fn` checkers. Proof: an end-to-end
  JIT'd `singleton` written in Aspect.
