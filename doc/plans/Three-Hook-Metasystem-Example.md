# Worked Example: The Three Hooks on One Type

**Status:** Companion to [Three-Hook-Metasystem.md](Three-Hook-Metasystem.md).
Illustrative — the metasystem is unimplemented, so the hook-*definition* syntax
below is **proposed** (consolidated from design discussion). The hook-*using* code
and the "lowers to today" code are real Aspect against the current `String` type
([demos/std/string/String.ap](../../demos/std/string/String.ap)).

Metaprogramming bodies are ordinary Aspect — `if`/`elif`, **C-style** `for (;;)`,
method calls — over compiler-provided **special structs** (`TokenTree`, `Ast`,
`Expr`, `Stmt`, `Fn`, `Program`, `Judgments`, and concrete list types like
`SegmentList`). No new control-flow forms.

> **Metalanguage reality (verified).** Aspect today has no `for-in`, no generics, no
> closures, no `match` — only C-style loops and hand-monomorphized collections. So
> metaprograms iterate index-wise (`list.count()` / `list.at(i)`), and `quote` is
> **not optional sugar — it is the only thing that makes AST construction bearable.**
> The verbosity in the bodies below is real, and is itself an argument for adding
> `for-in` as a general language feature eventually (not a blocker).

### Language features this leans on (new / proposed)

- **`quote { ... }` / `$(...)`** *(mandatory)* — an AST builder: `quote` writes
  surface Aspect that evaluates to an `Ast` node; `$(x)` splices an `Ast` (or type)
  into it. Desugars to explicit `Ast.*` constructors. Must **gensym** the
  identifiers it introduces (`__t`, `__v`, …) so they can't capture spliced user
  code (hygiene).
- **Value-blocks** *(landed 2026-07-14 — see doc/09 §Value blocks)* — a `{ ... }`
  in *expression* position whose `return` yields the block's value (not the
  function's). All paths must yield; `return` binds to the nearest value-block.
  This is the primitive every wrapping transform stands on.
- **Statement-position attributes** — `@attr` legal before a statement, not just
  before items. Widens parent §3 (see caveats).

---

## The idea

All three hooks hang off the existing `String` type-struct, and they **interlock**:

1. **Expansion** `interp { ... }` — string interpolation. Pre-parse, syntactic,
   sees no types. Lowers `"Hi {name}!"` into `String` append calls.
2. **Transform (coercion)** `String -> u8*` — when a `String` lands where a C string
   is wanted, rewrite the site to `<expr>.c_str()`. Needs resolved types.
3. **Transform (decoration)** `@debug(stmt)` — tag a call or assignment; encase the
   tagged expression so it prints itself and its value on one line.
4. **Rule** `must_destroy` — post-typecheck judgment: every constructed `String`
   must be `destroy()`-ed or escape. Modifies nothing; emits diagnostics.

The seam between #1 and #2 is the payoff: the expansion, blind to types, emits
`append_cstring(name)` for every hole. When `name` is itself a `String`, that call
is a `String`-where-`u8*`-expected mismatch — **the exact obligation the coercion
transform exists to discharge.** Generation files the demand; the type-directed hook
answers it. Phase order (parent §2): `EXPANSIONS → elaboration → TRANSFORMS → RULES → codegen`.

---

## What the user writes

```aspect
$import std/string      # the String type
$import std/fmt         # the `interp` expansion + `@debug` transform
$import std/io          # println

fn greet(u8* who) -> i32 {
    String name = String.from_cstring(who)

    # interp: expansion (hook #1). `name` is a String; the hidden coercion
    # transform (hook #2a) fixes the String-into-u8* it produces internally.
    String line = interp { "Hello, {name}! Welcome." }

    @debug println(line.c_str())     # decoration transform (hook #2b)

    line.destroy()                   # hook #3 verifies these happen
    name.destroy()
    return 0
}
```

The programmer sees none of the machinery — that is the point.

---

## Hook #1 — Expansion `interp`

**Lives in** `std/fmt`, **imported, never defined in the using file** (parent §4:
expansions compile ahead of their call sites). It sees only the token tree inside
its braces — no types, no names, no other items.

```aspect
# std/fmt/interp.ap
$module std/fmt

expansion interp(raw-tokens) -> expr {
    # `body` is the captured TokenTree. `segments()` splits the interior string
    # literal into text runs and `{ident}` holes, in source order.
    Ast stmts = Ast.stmts()
    stmts = stmts.push(quote { String __t = String.empty() })
    SegmentList segs = body.segments()
    for (u64 i = 0; i < segs.count(); i += 1 as u64) {     # no for-in in Aspect
        Segment seg = segs.at(i)
        if seg.is_text() {
            stmts = stmts.push(quote { __t.append_cstring($(seg.text_lit())) })
        } else {
            stmts = stmts.push(quote { __t.append_cstring($(seg.hole_ident())) })
        }
    }
    stmts = stmts.push(quote { return __t })
    return stmts.into_value_block()      # `{ ...; return __t }` as an expression
}
```

**Before:**

```aspect
String line = interp { "Hello, {name}! Welcome." }
```

**After (the value-block it emits):**

```aspect
String line = {
    String __t = String.empty()
    __t.append_cstring("Hello, ")
    __t.append_cstring(name)            # name is a String, not a u8*  (!)
    __t.append_cstring("! Welcome.")
    return __t                          # yields to `line`
}
```

**Why an expansion, not a transform?** Interpolation is a *syntactic* shape —
everything needed is in the tokens (parent §4). It also emits a value-block, which
no post-parse hook could introduce as new syntax.

**What it cannot do:** it does not know `name` is a `String` (that fact does not
exist yet), so it cannot insert `.c_str()`. It emits the naive
`append_cstring(name)` and lets the mismatch fall through to elaboration — where
hook #2a is waiting.

---

## Hook #2a — Transform (coercion) `String -> u8*`

> **Danger, acknowledged.** This is a user-defined *implicit conversion* — the
> C++/Scala footgun: invisible at the call site, composes transitively, and (because
> modules share a flat namespace) an unrelated import could otherwise change how your
> code coerces. **Two guardrails** (parent §6/§8, amended): a coercion fires **only
> after built-in coercion has failed**, and **only where a rule opts the module in**
> (`allow coercion String -> u8*`). That converts a footgun into a deliberate dialect
> feature — and makes the `interp`↔coercion link below an explicit *bundled-module*
> contract, not global magic. If this still feels too sharp, coercion transforms are
> the one hook worth deferring past v1.

Checking `__t.append_cstring(name)` demands `u8*`, finds `String`. Rather than
erroring, the checker files an obligation keyed `(coerce, String -> u8*)` at that
site — but only if the current module opted in. One registered handler claims that key.

```aspect
# std/string/coerce.ap
$module std/string

transform String -> u8* {
    # `site` is the expression that produced a String where u8* was demanded.
    # c_str(this) -> u8* is const, so reading the buffer doesn't invalidate it.
    fn handle(Expr site) -> Expr {
        return quote { $(site).c_str() }
    }
}
```

**Before → after (re-checked):**

```aspect
__t.append_cstring(name)            # (coerce, String->u8*)
__t.append_cstring(name.c_str())    # discharged; types agree
```

**Why a transform, not an expansion?** The decision needs a fact that does not exist
until elaboration: the argument's resolved type and the expected parameter type.

**Determinism:** exactly one handler may claim `coerce(String -> u8*)`; a second is a
compile error. `handle` is a pure function of `site`, so the rewrite is the same
regardless of when it fires. It creates no new obligation → the fixpoint converges
in one round.

**Extensibility:** a hole holding a `u64` would file `(coerce, u64 -> u8*)` — a
*different* key, a *different* transform (int formatting), no change here. This is
why the expansion's holes are effectively polymorphic without the expansion knowing
a single type.

---

## Hook #2b — Transform (decoration) `@debug(stmt)`

`@debug` tags a **use site** — a call, an assignment — and encases the tagged
expression so it reports itself and its value inline. It is not a repair (the code
typechecks fine), so an attribute obligation **always fires and never diagnoses** at
quiescence; it is an unconditional rewrite request.

```aspect
# std/fmt/debug.ap
$module std/fmt

transform @debug(stmt) -> stmt {
    u64 next_id = 0                       # intra-compilation state: shared across
                                          # every @debug firing this compilation

    fn handle(Stmt node) -> Stmt {
        next_id = next_id + 1
        Expr subject = node.value_expr()          # initializer / rhs / the call
        Ast  label   = Ast.str_lit(node.source_text())

        # Void calls have no value to thread → a plain statement block.
        if subject.type().is_void() {
            return quote {
                {
                    $(subject)
                    __dbg_void($(label), $(Ast.int_lit(next_id)))
                }
            }.as_stmt()
        }

        # A value use: bind, print with the type-directed formatter, yield.
        # Picking __dbg_i32 vs __dbg_str vs ... needs the RESOLVED type — this is
        # exactly why it must be a transform, not an expansion.
        Ast printer = dbg_printer_for(subject.type())
        return node.with_value_expr(quote {
            {
                $(subject.type()) __v = $(subject)
                $(printer)($(label), $(Ast.int_lit(next_id)), __v)
                return __v
            }
        })
    }
}
```

**Value site — before/after:**

```aspect
@debug i32 sum = add(a, b)
```
```aspect
i32 sum = {
    i32 __v = add(a, b)
    __dbg_i32("sum = add(a, b)", 7, __v)     # 7 = this site's id
    return __v
}
```

**Void site — before/after:**

```aspect
@debug println(line.c_str())
```
```aspect
{
    println(line.c_str())
    __dbg_void("println(line.c_str())", 8)
}
```

**Handler state + firing order.** `next_id` persists across every firing this
compilation, giving each site a stable label — the kind of "complex check across the
whole program" that carrying state in the handler unlocks. The price: because
`next_id` depends on firing order, the metasystem fires obligations in a
**deterministic total order** (by source position), so the same source yields the
same ids every run. State is fine; order must be pinned (refines parent §6).

**Also a function decorator.** The same `@debug` can register `transform @debug(fn) -> fn`
to wrap a whole body (all its returns funnel through one value-block — the reason the
value-block feature matters). `(@debug, stmt)` and `(@debug, fn)` are *different
keys*, so both handlers coexist; whichever fits where the attribute was written
dispatches. Unclaimed placements fail the hygiene rule (parent §7).

---

## Hook #3 — Rule `must_destroy`

Runs **last**, over the fully typed program — including the expansion's output and
both transforms' rewrites (parent §7). Modifies nothing; only judges.

> **Honesty check (this is the scrutiny's sharpest finding).** A real leak checker
> is linear-ownership + escape + alias + path-sensitivity analysis — *not* a cheap
> query. The query API therefore has **two tiers** (parent §7, amended): Tier-1 are
> genuine dictionary lookups (`call_sites_of`, `has_attr`, `instantiations_of`);
> Tier-2 (`escapes`, `destroyed_on_all_paths`, `reachable_from`) require analyses the
> compiler must actually build. `must_destroy` is **Tier-2**, so it ships as a
> **warning**, not a compile error, and is deliberately shallow — see its documented
> blind spots below.

```aspect
# std/string/lints.ap
$module std/string

rule String must_destroy {
    # Tier-2, best-effort, INTRAPROCEDURAL only. Emits warnings.
    # (A stateless one-fn rule can use the parent's `rule <anchor> <fn>` shorthand.)
    fn check(Program prog) -> Judgments {
        Judgments out = Judgments.empty()
        FnList fns = prog.functions()
        for (u64 i = 0; i < fns.count(); i += 1 as u64) {          # no for-in
            Fn f = fns.at(i)
            BindingList bs = f.bindings_of_type(String)            # local Strings
            for (u64 j = 0; j < bs.count(); j += 1 as u64) {
                Binding b = bs.at(j)
                if b.destroyed_on_all_paths() { continue }         # Tier-2 query
                if b.escapes()                { continue }         # returned/stored/moved
                if b.has_attr("leaks")        { continue }         # explicit opt-out
                out.warn(b.site(),
                    "String bound here is never destroyed and does not obviously escape")
            }
        }
        return out
    }
}
```

**Documented blind spots (v1).** No interprocedural ownership transfer (destroying
via a helper you pass the String to reads as a leak → *false positive*, hence
`@leaks`); no alias tracking (destroy through a second pointer is missed → *false
negative*). It catches the common, honest case and says so. The `@leaks("...")`
escape hatch mirrors the parent's trust idiom (§7); the hygiene rule forces `@leaks`
to be a claimed attribute — the linter documents its own opt-out.

**Why a rule, not a transform?** It generates and rewrites nothing; it renders a
verdict, and needs the whole typed program *after* the other hooks have run.

---

## How they compose — one trace

```
source:   String line = interp { "Hello, {name}! Welcome." }
                              │  EXPANSION (pre-parse, no types)
                              ▼
AST:      { String __t = String.empty(); __t.append_cstring("Hello, ");
            __t.append_cstring(name);            ← String, but u8* wanted
            __t.append_cstring("! Welcome."); return __t }
                              │  ELABORATION files (coerce, String->u8*)
                              ▼
          __t.append_cstring(name)  ── TRANSFORM String->u8* ──►  name.c_str()
                              │  (re-checked; obligation discharged)
          @debug println(...)  ────── TRANSFORM @debug(stmt) ──►  block + __dbg_void
                              │  RULES run over the final typed program (last)
                              ▼
          must_destroy (warning-tier): `line`, `name` both reach `.destroy()` → clean
```

Generation is deliberately dumb and files a demand (#1); the type-directed hook
answers it (#2a); a second type-directed hook decorates on request (#2b); the
judgment hook polices the result (#3). Each sees exactly what its phase exposes.

---

## What it lowers to (valid today)

Strip the hooks and this compiles against the current compiler — the ergonomic delta
the hooks buy. (Value-blocks are real now, so even the expansion's *output* is
directly expressible by hand; the manual form below spells it out as plain
statements only for clarity:)

```aspect
fn greet(u8* who) -> i32 {
    String name = String.from_cstring(who)

    String line = String.empty()
    line.append_cstring("Hello, ")
    line.append_cstring(name.c_str())      # the coercion, written by hand
    line.append_cstring("! Welcome.")

    println(line.c_str())                  # @debug elided

    line.destroy()
    name.destroy()
    return 0
}
```

---

## Caveats & ties to open questions

- **Proposed syntax.** `expansion`/`transform` declaration forms are consolidated
  here; only `rule <anchor> <fn>` comes verbatim from the parent (§7). `quote`, the
  `Ast.*` builder, and the query API are sketches — the parent flags both the
  diagnostics API (§11) and the query-API surface as open.
- **Parent §3 (attributes).** `@debug` on a statement requires attributes below item
  level. Recommended: anchor at **statement** position; the transform encases the
  value-producing sub-expression (initializer / rhs / call) and keeps the binding.
  Expression-position attributes (`x = @debug f(a) + 1`) are a later extension.
- **Parent §6 (purity).** Refined from "stateless" to "deterministic": handlers may
  carry **intra-compilation** state, provided obligations fire in a total,
  source-determined order. (Cross-*compile* persistence was never intended.)
- **Parent §7 (query API), amended.** Two tiers — cheap dictionary lookups vs.
  flow-sensitive analyses the compiler must build. Tier-2 rules warn, they don't gate.
- **Coercion is governed, not global.** Fires only after built-in coercion fails and
  only where a rule opts in. The single hook worth deferring past v1 if in doubt.
- **Execution is the real work, not the syntax.** All three hooks run JIT'd Aspect
  inside the compiler (already possible: `CodeGenerator::generate` + `jit_execute`,
  inkwell/LLVM 19). The gap is the marshalling ABI (AST nodes as opaque handles into
  a compiler arena; `quote`/`Ast.*`/query as Rust `extern` builtins) and a **staged
  driver** that JITs hook-providing modules before their users. See the parent doc's
  **Execution Model** and **Implementation Plan** sections.
- **Value-block** is a core-language feature the metasystem leans on, not a
  metasystem feature — it deserves its own short design note before it lands.
- **Build order.** None reachable until prerequisites (module system, value-blocks,
  attributes, `quote`, the metaprogramming std) then the obligation solver, then the
  hooks. Concrete phased checklist lives in the parent doc's Implementation Plan.