# A theoretical transform attribute: `@journal`

**Status: decoration now fires (landed 2026-07-31); this exact `@journal` is one
slice away.** The transform hook's *decoration* mode is real: an attribute
handler `transform fn (Stmt) -> Stmt` fires eagerly on every tagged statement and
rewrites it. A working, tested example — `@log`, which threads a binding's value
through a zero-arg method — ships as
[`tests/programs/transform_decorate.ap`](../../tests/programs/transform_decorate.ap),
and the `money_dialect.ap` demo can now use it. The `@journal` below goes one
step further: it passes a *runtime id* to an arg-bearing recorder
(`ledger_record(id, amount)`), which needs a write-surface piece `quote` doesn't
build yet (a call with arguments). So this file stays illustrative for that last
step — see "What's left" at the end. The point still stands: a transform
*attribute* injects runtime code nothing else in the language can.

## The gap it fills

The working demo already has an `@audited` attribute — but that is a **rule**
anchor (hook #3). A rule can only *judge*: `@audited` emits a compile-time note
per money-touching function and fails or passes the build. It cannot change what
the program *does* at runtime.

A real ledger needs the opposite: an **audit journal** — a runtime record of
every money movement, written automatically so no one can forget to log a
transaction. That is a cross-cutting concern you do **not** want to hand-write at
every assignment site (it's exactly the code people forget, or get subtly wrong).

That is a job for a transform *attribute* in **decoration** mode: seeded eagerly
from every attribute site, it rewrites the tagged statement to thread the value
through a recording call — using the value's resolved type to pick the recorder.

## In use

```aspect
@journal Money fee    = Money.of(0, 25)     # every money-bearing binding is journaled
@journal Money refund = Money.of(1, 00)
```

You tag the binding; the journal entry writes itself.

## The handler (shipped syntax, once decoration fires)

```aspect
# Compile-time counter: gives each journaled site a stable, source-ordered id.
# `meta` globals already work today (see money_dialect.ap's cousin,
# transform_meta_counter.ap) — they carry state across firings.
meta u64 entry_id = 0

# Decoration handler: (Stmt) -> Stmt. Fires on each `@journal` site, rewrites the
# binding to record the money value, then yields it unchanged.
transform fn journal(Stmt node) -> Stmt {
    entry_id = entry_id + 1
    Expr amount = node.value_expr()          # the right-hand side (a Money)

    # Wrap the initializer in a value-block that records, then returns the value.
    # Value-blocks ARE shipped — `{ ...; return v }` in expression position is a
    # core feature (doc/09 Value blocks), which is the whole reason a decoration
    # can wrap a binding without disturbing its type.
    return node.with_value_expr(quote {
        {
            Money __v = $(amount)
            ledger_record($(Ast.int_lit(entry_id)), __v.raw())
            return __v
        }
    })
}
transform @journal journal                   # `public transform @journal ...` for whole-program reach
```

`ledger_record` and `Money.raw()` are ordinary, already-defined symbols — the
handler only *calls* them, it does not synthesize anything the user names. (That
distinction matters — see "Why this one is buildable" below.)

## What it lowers to

```aspect
@journal Money fee = Money.of(0, 25)
```

becomes, before typecheck, exactly:

```aspect
Money fee = {
    Money __v = Money.of(0, 25)
    ledger_record(1, __v.raw())      # 1 = this site's stable id
    return __v
}
```

`fee` is still a `Money`, still `$0.25`; the recording is now impossible to omit.
The next `@journal` site gets id `2`, and so on — deterministic because the
metasystem fires obligations in source order.

## Why *only* a transform can do this

The same `@`-attribute could be read by any hook, but only decoration produces
this result:

- A **rule** (hook #3) can *diagnose* that a money binding isn't journaled, but
  it cannot *inject* `ledger_record`. Rules never modify the program. So
  automatic journaling is flatly impossible with rules — the tool you have today.
- A **coercion transform** (the shipped mode) only fires on a *stuck type
  conversion* and only *replaces one expression with another*. It cannot wrap a
  statement with a side-effecting record-and-return.
- An **expansion** (hook #1, pre-parse) runs before types exist. A money-only
  `@journal` could almost be an expansion — but the *general* one, `@journal`
  over any type, must pick `ledger_record` vs `dbg_i32` vs a string recorder from
  the value's **resolved type**, which an expansion cannot see. Type-directed
  code generation is the defining reason the transform hook exists.

Decoration is the only mode that is **eager** (fires on the attribute, not on an
error), **type-aware** (mid-elaboration, so `amount.type()` is known), and
**code-generating** (emits runtime behavior). `@journal` needs all three.

## Why this one is *buildable* (unlike `@dataclass`)

The `@dataclass` idea (auto-generate `get_x`/`set_x`) hit a hard wall: user code
writes `p.get_x()`, and Aspect resolves method dispatch at **parse time**, before
the methods are synthesized — so the call fails to parse. `@journal` sidesteps
that entirely: it injects calls to symbols that **already exist** (`ledger_record`,
`Money.raw()`); it never asks user-written source to name a symbol that doesn't
exist yet. So decoration is not just the *useful* transform-attribute mode — it's
also the *tractable* one.

## What's left

The decoration **engine** shipped, so most of the earlier list is now real:

1. ~~An eager per-round pre-pass that seeds each `@attr` site, fires the handler,
   splices, and consumes the attribute.~~ **Done** (`fire_decorations`).
2. ~~A `(Stmt) -> Stmt` handler shape.~~ **Done** (`is_valid_decoration`).
3. ~~The statement read/rewrite surface `Stmt.value_expr()` / `.with_value_expr()`.~~
   **Done.**

What `@journal` *specifically* still needs — because it passes a runtime id to a
recorder — is the **arg-bearing write surface**: a free-function-or-method call
with arguments (`ledger_record(id, amount)`), plus `Ast.int_lit`. `quote` builds
only zero-arg method calls today, so a decoration currently threads its value
through a zero-arg method (`$(amount).logged()` — the shipped `@log`), not an
arbitrary recorder. Add the arg-bearing call builder and `@journal` runs verbatim.

None of this needs the parser→`MethodCall` migration. The engine, the value-block
wrapping, the `meta` counter, `quote`, and the persistent JIT are all shipped —
so "transform attributes are useless" is already false: `@log` is a real runtime
rewrite driven by an attribute, and `@journal` is the same shape with one more
builder.
