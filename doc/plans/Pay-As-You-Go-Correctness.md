# Pay-As-You-Go Correctness — Aspect's Identity

Status: **design proposal, not yet reviewed.** Supersedes
`Areas-And-Refinements.md` (deleted 2026-07-31): the *areas* half is
dropped, the *refinements* half is carried forward here and revised.
Every language-level item below still owes a `language-designer` review
before implementation (CLAUDE.md § Language design review) — this doc is
what that gate reads.

---

## The pitch

> Aspect is a systems language where the invariants you'd normally keep
> in comments become part of the type — checked, exploited, and never
> re-demanded — and you pay only for the ones you use.

Every systems programmer already tracks invariants by hand: *this index
is in range, this pointer isn't null, this value is one of three shapes,
this can fail.* In C that knowledge lives in your head and in comments.
In Rust the compiler tracks it for you, but bills you **upfront and
mandatorily** — nothing compiles until the borrow checker is satisfied.

Aspect takes the niche nobody occupies cleanly: **pay-as-you-go
correctness.** Assert nothing and you get C — full control, no ceremony,
no checker in your face. Every fact you *volunteer* buys a proportional
slice of safety, speed, and terseness. It's a gradient, not a gate: the
compiler never blocks you for saying nothing, and it never makes you
repeat what you've already said. Help is opt-in and incremental, never a
tax — so you're never fighting the compiler to state what you already
know, you're being *paid back* for stating it.

That single sentence is also the answer to "why not just use Rust?": you
keep C's control, and the compiler does the bookkeeping only where you
ask for it.

## Positioning

|                     | Who tracks invariants | When you pay        | Guarantee            |
|---------------------|-----------------------|---------------------|----------------------|
| C                   | You (head + comments) | Never / at runtime  | None                 |
| Rust                | Compiler              | Upfront, mandatory  | Full memory safety   |
| Zig                 | You (conventions)     | Runtime (safe modes)| Runtime-checked      |
| **Aspect**          | **Compiler, on request** | **Per fact you volunteer** | **Proportional to what you declare** |

Nobody else sells the *gradient*. That's the identity.

---

## The features — each an instance of one idea

The throughline: **deposit knowledge, get payback.** *Tell the truth
once; the compiler holds you to it, exploits it, and never makes you
repeat it.* Every feature below is that sentence applied to a different
kind of fact.

### Sum types + `switch` — the substrate

**✅ Shipped 2026-08-04** (all five stages, `is` and the `&&`-chaining
amendment included — see `../solved/Sum-Types-And-Switch.md`). Build
order step 1 is complete; step 2 (flow narrowing) is next.

Deposit your data's *shape* (a value is one of these variants); payback
is exhaustiveness and destructuring with no manual tag bookkeeping. This
is the foundation everything else leans on — an AST node, a token, a
`Result`, an `Option` are all sum types.

`switch` (a keyword already reserved but unimplemented — no C
fall-through legacy to undo) is the matcher, following Zig's precedent:

- **No implicit fall-through.** Each case is a block.
- **`case` labels are patterns, not just constants.** A bare integer
  (`case 3:`) is the degenerate constant pattern; `case Add(l, r):` is a
  constructor pattern that binds. One construct; the constant form is a
  special case of the pattern form.
- **Exhaustiveness is the payback.** A `switch` must cover its
  scrutinee. `default` supplies coverage for whatever the arms don't.
  Four stances, from one rule ("a switch must be exhaustive; `default`
  covers the rest"):
  - integer / pointer → `default` **required** (the type can't be
    enumerated, so it's the only route to coverage). Enums are
    *nominally closed* — fully-listed is exhaustive, and a value forged
    past the variant set by an `as` cast traps at the switch in -O0 builds
    (UB when optimized)
    (revised 2026-08-03; see `../solved/Sum-Types-And-Switch.md`);
  - sum type, every variant listed → `default` is a dead arm, compiler
    says so;
  - sum type, variants missing, no `default` → **error**: "handle `X`,
    `Y`, or add `default`";
  - sum type, variants missing, *with* `default` → legal, but this
    switch has opted out of future-variant checking (fine, as long as
    the author knows it).

The third and fourth stances are the whole point: add a variant next
year and the compiler marches you to every switch that must now handle
it — unless a `default` silently swallowed it, which is why a `default`
on an already-complete sum-type switch is flagged.

Lowering is nothing new for codegen: load the discriminant (a small int) →
`switch` → per-arm GEP/bitcast the payload and bind. No generated
per-type comparison function; that is only needed for whole-value `==`
on aggregates, which pattern matching never uses.

`is` complements `switch` as the single-variant probe
(`if s is Circle(r) { … }`, added 2026-08-03): same patterns, no
exhaustiveness claim — the deliberate opt-out for "I care about one
variant here." Specced in `../solved/Sum-Types-And-Switch.md`.

### Flow narrowing — the glue

Deposit a runtime fact (`if p != null { … }`, or a `switch` arm);
payback is that the fact is *remembered* in that scope. You checked null
once; you never re-check. This is what makes refinements feel alive
rather than bureaucratic — a preceding `if i < len` is exactly what lets
the checker discharge a `{< len}` boundary with no runtime cost.

### Refinements — the flagship

Deposit a value's *shape*; payback is twofold and this is the part the
old doc undersold: the same annotation makes the program **safer AND
faster**.

Syntax (carried forward from the superseded doc):

```
i32 {>0}            i32 {>=0}            i32 {1..=100}
u8* {nonnull}       i32 {!=0}           u64 {< len}
```

**Interval-only, no SMT.** Constraints are concrete ranges and bounds;
compositions propagate by interval arithmetic through the bidirectional
checker that already narrows literals. No general boolean predicates.

**Bounds may reference other values only if those values are immutable
in scope.** This is the key revision. `{< len}` against a runtime
variable is a *relational* refinement (it relates `i` and `len`), not an
interval one — and if `len` were mutable the fact could be broken by
mutating *either* operand, which is the inter-variable-dependency road
to an SMT solver. Restricting the referenced bound to something that
can't change (`const`, a `let`-bound length, an unchanging field)
collapses it back to single-variable interval tracking: only `i` can
drift, so "does `i` still fit" stays decidable. It also covers the real
use case (indexing a slice whose length is fixed while you walk it).

**Boundaries never lie.** Refinements are checked at assignment,
parameter pass, return, and use. At each boundary the checker does
exactly one of three things:

- proves it holds (e.g. inside `for i in 0..len`, induction gives
  `i < len`) → **no runtime cost**;
- proves it can't → **compile error**;
- can't decide → **inserts a runtime check that aborts** on violation.

So "what if `i` drifts past `len`?" has a clean answer: the mutation
`i = i + 1` is itself a boundary. The value physically cannot become
`>= len` while still typed `{< len}` without passing a check — the
refinement constrains the mutation, it doesn't go stale.

**Refinements double as optimizer facts.** This is what makes them
*cool* and not merely safe — the same `{…}` the checker verifies, codegen
feeds to LLVM:

- `i32 {>0}` / `{1..=100}` → `!range` metadata on loads, `llvm.assume`
  at boundaries;
- `u8* {nonnull}` → `nonnull` attribute, so null checks fold away;
- `u64 {< len}` → the bounds check isn't inserted *and* the optimizer
  knows the index is in range (vectorization, no guard).

The more precisely you describe a value, the more the compiler knows —
often beating the equivalent C, which can only guess. Same keystroke,
safer and faster.

**Lowering:** `LangType` stays `Copy` — refinements live in a side table
keyed by expression node, checked *after* the existing base-type
compatibility test in `types_coercible`. (Open question 1 below.)

### `Result` / `Option` + `?` — the unhappy path

Deposit the *failure path* once (a function returns `Result<T, E>`);
payback is propagation without hand-written error branches. `?` threads
the error; the happy path stays the main line instead of drowning in
`if err != 0` checks. Both types are ordinary sum types — they fall out
of the substrate for free, which is another reason sum types come first.

### UFCS — the comfort texture

The one pure-ergonomics lever, and nearly free given existing machinery.
`a.foo(y)` and `foo(a, y)` become the same call: any free function reads
as a method, chains fluently, and library authors extend a type without
inheritance or wrappers.

Resolution rides the site where method-vs-field is already decided at
`.`:

- `a.foo()` resolves by the **type of `a`** → `TypeA$foo`. Two structs
  sharing a method name were never in conflict — they are distinct
  `A$foo` / `B$foo` symbols already; the receiver picks.
- One type having both a method `TypeA$foo` and a free `foo(TypeA, …)`:
  a method *is* the free function `TypeA$foo`, so the rule writes itself
  — look for `TypeA$foo` first, fall back to a plain free `foo` whose
  first parameter is `TypeA`.
- Aspect has **no free-function overloading**, so the fallback has
  exactly one candidate — it matches `TypeA` or it's a plain type error.
  No overload-resolution ambiguity to design, which makes UFCS cheaper
  here than in Nim/D.

---

## Build order

Sequenced so each phase stands on the last and ships something felt:

1. **Sum types + `switch`.** The substrate. Also the highest-leverage
   base-language feature independent of the vision, and the prerequisite
   for `Result`/`Option` and for refinement diagnostics.
2. **Flow narrowing.** Small, and it's the glue that makes step 3 pay
   off at zero runtime cost.
3. **Refinements.** The flagship. Interval + immutable-relational,
   boundary-checked, doubling as optimizer facts.
4. **`Result` / `Option` + `?`.** Ergonomics on top of the substrate.
5. **UFCS.** Comfort texture; independent of the rest, drop in whenever.

---

## Explicitly out of scope / deferred

Recorded so a future identity bump is a new decision, not a silent drift.

- **Areas / region memory — dropped.** The escape-checking half is
  Rust-lifetime-shaped: proving a pointer doesn't outlive its area is the
  same analysis a borrow checker does, and it drifts the same way (toward
  false positives or signature annotations that recreate lifetimes),
  while delivering a *weaker* guarantee (aliasing unchecked). Rust's cost
  for less than Rust's safety. If arena *ergonomics* are ever wanted, add
  them as an unchecked Zig-style allocator-value — never as a checked
  lifetime feature.
- **The metasystem — dropped as an identity.** Compile-time
  metaprogramming isn't unique (Zig/Nim/Rust/D all have it) and it's a
  library-author's tool, felt rarely — a poor basis for "cool to use."
  The machinery (JIT'd Aspect functions) exists if it's ever revived as a
  *plain* comptime feature, but it is not the identity.
- **General generics / comptime — deferred.** Not rejected; simply a
  separate identity question. Sum types cover the substrate need without
  them for now.
- **Variadic arguments — deferred.** Three features hide under the word:
  the type-safe heterogeneous one (`print(a, b, c)`) needs comptime, so
  it waits on that decision; the C-untyped `va_arg` one is the antithesis
  of the vision (least-shaped values) and unneeded (the stdlib
  deliberately avoids `printf` — `lib/std/io/generic.ap` — and is moving
  off libc); the homogeneous `T...`→slice one is minor sugar wanting a
  slice type first. Revive trigger: a comptime decision → build the
  type-safe version. An `extern`-only ABI passthrough for *calling* C
  variadics stays a narrow interop option if a real need appears.
- **SMT / general-predicate refinements — out.** Interval + immutable
  references only. No Z3, no `{x > y}` inter-parameter relations.
- **Borrow checker / aliasing rules — out.** Mutating through two
  pointers to one location is the programmer's problem.

---

## Open questions

1. **Refinement storage.** Side table keyed by expression id (keeps
   `LangType` `Copy`, preferred) vs. an extra field on `LangType`. The
   side table costs an indirection at each check site.
2. **Refinements on struct fields.** `type Point { i32 {>=0} x }` — the
   contract fires at literal construction and at field assignment; field
   reads use the declared range.
3. **Refinement pretty-printing.** `i32 {>0}` must print verbatim in
   diagnostics, not as `i32` — the Display impl needs the refinement
   table.
4. **`switch` on refined scrutinees.** Does matching `case 0:` narrow the
   scrutinee's interval inside the arm? (Flow narrowing suggests yes.)
5. **`?` desugaring.** Early-return on the error variant needs a defined
   interaction with the function's declared return `Result<_, E>` and
   with any `defer`/cleanup once that exists.

---

## References

- Zig `switch` and `comptime`: the exhaustive-switch and
  metaprogramming-as-normal-code precedents.
- Liquid Haskell / SPARK Ada: refinement-type references; Aspect commits
  to a strict subset (intervals + immutable references, no solver).
- Nim / D: UFCS precedents — Aspect's lack of overloading makes its
  version simpler than either.
