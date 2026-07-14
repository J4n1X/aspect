# Areas and Refinements — tjlb's Identity

Status: **design accepted, not yet implemented**. Locked decisions: B + A
(see [§ Locked decisions](#locked-decisions)). Implementation is sequenced
as **Areas first, then Refinements**.

This document captures the design commitments that move tjlb out of the
"C with classes" orbit and into its own niche. It is the canonical
reference for both features and the migration that follows them.

---

## The pitch

> *tjlb: a small systems language where memory has **scopes** and values
> have **shapes**.*

Two distinct ideas, both committed simultaneously as the language's
identity. Neither is novel on its own; the combination is what
distinguishes tjlb from the saturated modern-C-replacement family
(Zig / Odin / Hare / Jai / unsafe-Rust).

- **Areas** — memory has named scopes. Every allocation belongs to an
  *area*. Areas are lexical: when the scope ends, the area is reclaimed
  in one shot. Lifetimes are tracked by dataflow, *not* by an
  arena-name in the pointer's type. Catches dangling-pointer bugs
  without a borrow checker.
- **Refinements** — values have predicates. `i32 {>0}`, `u8* {nonnull}`,
  `i32 {0..=100}`. Interval-only — no SMT solver, no general predicates.
  Boundaries (assignment, parameter pass, return) verify statically when
  the source range fits the target; otherwise a runtime check is inserted
  that aborts on violation.

Naming: we used to call the memory feature "arenas" but settled on
**area** because *arena* connotes bump-allocator semantics. Tjlb's
runtime gets to choose the implementation — bump allocator is the
likely default, but the language commitment is to *scoping*, not
allocation strategy.

---

## Locked decisions

These were settled in the design session and are **not open for
relitigation** during implementation. Push back here, not in PRs.

1. **Area lifetime tracking: dataflow, lexical.** No arena-name in
   pointer types. The type checker tracks "does this pointer's storage
   outlive its declaring scope?" by dataflow, surfacing escapes as
   compile errors. Reference: Cyclone's "regions in types" was option
   C; we chose B.

2. **Refinements: interval predicates only.** Constraints are concrete
   ranges and bounds (`>0`, `>=lo`, `<=hi`, `nonnull`). Compositions
   propagate by interval arithmetic. No general boolean predicates, no
   inter-parameter relations. SMT/Z3-based refinements are explicitly
   out of scope for tjlb.

3. **Default area: `heap`.** A process-wide area backed by malloc/free
   exists implicitly. Stdlib types accept an `area` parameter; passing
   `heap` reproduces today's manual-malloc behavior verbatim. This lets
   existing code migrate one allocation at a time.

4. **Sequenced rollout: Areas first, Refinements after.** Areas reshape
   memory; refinements layer on top. Doing both at once would mean
   rewriting refinement code against the new memory model. Each phase
   is a coherent, shippable chunk.

5. **`return}` is a complete statement.** Already shipped — was a
   Tier-1 papercut. Calling out here because it removes a class of
   friction that area/refinement code would have hit constantly.

---

## Areas — design

### Syntax

```
area name { ... }                # block scope — area lives for the block
fn foo(area a, ...) -> ...       # function parameter — area lives across the call
malloc_in(a, n)                  # allocate `n` bytes in area `a`
new(a) Type { ... }              # construct a heap-allocated value in `a`
```

`area` is a new keyword. `area name { ... }` introduces a lexical block.
Inside, `name` refers to the area; allocations made via `malloc_in(name,
...)` (or `new(name) ...`) live until the block's close.

A function may take an `area` parameter — the caller decides which area
the callee's allocations land in. This lets, e.g., a parser allocate
its AST into the caller's area: `fn parse(area a, u8* src) -> Ast`.

### Type system: dataflow lifetimes

No `i32*@scratch`-style annotation. The type checker maintains, for
each pointer-typed value, a *region*: the lexical scope of the area
it was allocated in (or "any" for `heap` / "static" for `.rodata`
pointers). Regions are checked at:

- **Return statements** — a returned pointer must come from an area at
  least as wide as the function's caller. Returning a pointer
  allocated in a block-scoped area inside the function is an error.
- **Field assignment** — assigning a pointer into a longer-lived
  struct's field is an error if the source's region is shorter.
- **Across area boundaries** — using a pointer from a now-defunct
  area is an error.

Inference is purely lexical. Two pointers allocated in the same area
have the same region; arithmetic on a pointer preserves its region;
`&local_var` is region "current function's stack" (treated as a
short-lived area).

Failure mode: when the type checker can't *prove* a pointer outlives
its use, it emits an error referencing the originating allocation.
Cycles in the region graph are not yet allowed.

### Runtime

A minimal runtime, linked statically into every binary. Each area is
a small object (≈32 bytes) holding:

- A chunk list head.
- The current bump pointer + chunk-end pointer.
- A page size / chunk size.

`area name { ... }` lowers to:

1. Allocate one chunk (default 4 KB), initialise the area struct.
2. Run the block body.
3. Walk the chunk list, free each.

Allocation within an area is a bump on the fast path; on chunk
exhaustion, mmap (Linux) / VirtualAlloc (Windows) for a new chunk.
No per-allocation free — that's the whole point.

`realloc` is "alloc new + memcpy + abandon the old slot." Wasteful for
growing buffers, but the cost is predictable and the failure mode is
silent. A future fast-path for "grow last allocation" can be added if
profiling demands it.

`heap` is a special area: its `malloc_in` calls malloc, and there's no
chunk-list bookkeeping. Allocations in `heap` persist until explicit
`free`. This is the escape hatch for non-stack-shaped lifetimes
(servers, caches, long-lived data structures).

### Stdlib migration

Every heap-owning type in `demos/std/` becomes area-parameterised:

```tjlb
# Today:
String s = String.from_cstring("hello")
# ...
s.destroy()

# Under areas:
area scratch {
    String s = String.from_cstring(scratch, "hello")
    # ... no destroy() — scratch's close reclaims it
}
```

`from_cstring` gains an `area` first parameter. `destroy()` goes away
for area-allocated values (kept only for `heap` explicitly).
`VecI32`, `Vec*`, future container types follow the same pattern.

Failure mode at compile time: forgetting the `area` argument is a
type error, since `from_cstring` now requires it.

### Open questions (to be answered before implementation)

1. **Construction syntax.** `new(a) Type { ... }` or `Type.new(a,
   ...)` or `Type.in(a)`? Leaning toward `Type.new(a, ...)` —
   consistent with the existing static-factory convention
   (`String.from_cstring(...)`), no new keyword.
2. **Returning area-allocated values.** Returning `String` from a
   factory: by-value, sret as today — the *contents* (the buffer)
   belong to the caller's area passed in as a parameter. No
   special-case.
3. **Globals.** Pointers in global initialisers must come from `heap`
   or `.rodata`. Block-scoped areas can't appear at file scope.
4. **Nested areas.** `area outer { area inner { ... } }` — does
   `inner` reclaim before `outer`? Yes (LIFO). Cross-area pointers
   (inner → outer) outlive their declaring area? No — that's an
   error.

---

## Refinements — design

### Syntax

```
i32 {>0}              # positive
i32 {>=0}             # non-negative
i32 {1..=100}         # closed integer range
u8* {nonnull}         # non-null pointer
i32 {!=0}             # divisor — useful for `divide`
```

The refinement appears between the base type and any pointer/array
modifiers: `i32 {>0}* x` is "pointer to a positive i32", not
"non-null pointer to i32". For `nonnull` on the pointer itself, use
`i32* {nonnull}`.

Multiple refinements on one type compose by intersection:
`i32 {>0, <=100}` (or `i32 {1..=100}` — sugar for the same).

### Type system: interval propagation

Each refined value carries a known interval. The bidirectional checker
already handles literal narrowing; refinements extend this to track
ranges through arithmetic:

```
i32 {>0} a, i32 {>0} b
a + b               # → i32 {>1} (or {>=2})
a - b               # → i32 {>=...} — unknown lower bound
a * b               # → i32 {>0}
```

Integer overflow remains UB on `nsw`-marked signed arithmetic; the
refinement layer doesn't change that. Refinements only propagate
through operations where the result range is statically inferable.

At boundaries (assignment, parameter pass, return):

- If the source range is a *subset* of the target range, no check
  needed — pure type-level pass.
- Otherwise, the codegen inserts a runtime check at the boundary
  that aborts on violation (`abort()` from libc, same as the bounds
  check in `VecI32.at`).

### Composition with the existing bidirectional checker

Refinements are *contracts*, not new types in their own right.
`i32 {>0}` and `i32` have the same LLVM lowering, the same `sizeof`,
the same calling convention. The refinement is purely a check at
boundaries. So:

- `LangType` does not change shape. Refinements live in a side table
  keyed by the value's *expression node*, or as a new optional field
  on `LangType` (we'll prefer the side-table approach so `LangType`
  stays `Copy`).
- `types_coercible` gains a refinement-checking layer that runs AFTER
  the existing base-type compatibility check.
- The check-mode arms of the bidirectional checker push *refinements*
  down into operand checking — same shape as how target widths
  already propagate.

### Stdlib usage

```tjlb
fn divide(i32 a, i32 {!=0} b) -> i32 { return a / b }

fn VecI32.at(this, u64 {< this.length} i) -> i32 {
    return this.data[i]
    # No runtime bounds check inside `at` — the refinement is the bound.
}
```

The current `if i >= this.length { abort() }` becomes a compile-time
contract. Callers pass `i` whose interval is statically known to fit;
when it isn't (loop induction variables, untyped indices), the codegen
inserts the runtime check — same machine code, but the contract is now
on the boundary, not the function body.

### Open questions (to be answered before implementation)

1. **Where does the refinement live?** Side table keyed by expression
   id, or extra field on `LangType`? Side table keeps `LangType` Copy
   (preferred); side-table indirection at every check site is a small
   cost.
2. **Refinements on function-pointer types.** Pre-conditions on the
   pointee's signature? Skip for now — function pointers don't carry
   refinements in v1.
3. **Refinements on struct fields.** `type Point { i32 {>=0} x; i32
   {>=0} y }` — when does the contract check fire? At struct literal
   construction and at field assignment. Field reads use the
   declared range.
4. **Pretty-printing.** `i32 {>0}` should print verbatim in
   diagnostics, not as `i32`. Display impl needs the refinement
   table.

---

## Implementation sequencing

### Areas (phase 1)

1. **Runtime stub.** A tiny C-like header `area.h` + `area.c`
   (bump allocator, chunk list, free-all). Linked statically. Two
   functions: `area_alloc(area*, u64)` and `area_destroy(area*)`.
2. **Keyword + parser.** `area` keyword. `area name { ... }` block
   statement. `area name` function parameter. `new(area) Type {
   ... }` constructor (or chosen alternative).
3. **Type system: regions.** Each pointer-typed expression gets a
   region tag (lexical-scope id). The checker walks expressions
   propagating regions. Cross-region escapes (returns, field
   assignment, etc.) emit errors.
4. **Codegen.** Areas lower to alloca'd `Area` structs;
   allocations call `area_alloc`; block close calls
   `area_destroy`. `heap` calls malloc directly. `realloc`
   policy as documented.
5. **Stdlib rewrite.** `String`, `VecI32`, `mem/alloc` all take an
   area parameter. `destroy()` removed from area-bound types.
   Demos updated. The two non-testable demos
   (`bench`, `ask_name`) get the rewrite too.
6. **Tests.** Region-escape errors, area-allocated struct returns,
   nested areas, `heap` interop.

Rough size: 2-3 weeks of focused work, ~1500 lines.

### Refinements (phase 2)

1. **Parser.** `{>0}`, `{>=0}`, `{lo..=hi}`, `{nonnull}` after a
   type. Side-table assigned per-expression at parse time.
2. **Interval-propagation pass.** A new mini-checker that runs
   alongside the bidirectional checker, tracking each value's
   known interval through arithmetic and comparisons.
3. **Boundary checks.** When a source value's interval doesn't
   fit the target's refinement, codegen inserts a runtime abort.
4. **Stdlib rewrite.** Bounds-checked accessors become
   contract-typed: `at(u64 {< len} i)`, `divide(i32 a, i32 {!=0} b)`,
   etc.
5. **Tests.** Static-fit cases (no runtime check), runtime-check
   cases (verify abort fires), refinement composition through
   arithmetic.

Rough size: 1-2 weeks, ~700 lines.

---

## What this commits us to

After both phases land, tjlb has:

- Scoped memory with compile-time leak/dangling protection — no
  borrow checker, no GC, no manual `destroy()`.
- Range-constrained values — bounds checks become contracts at
  boundaries, not bodies.
- The full existing language: type-structs, methods, fn-pointers,
  sum types (whenever we add them), `$import` modules, sizeof, the
  stdlib.

The single-sentence pitch holds: **a small systems language where
memory has scopes and values have shapes.** Nobody else commits to
both at once.

---

## What this does *not* do

To stay focused, the following are explicitly out of scope:

- **Borrow checker.** Aliasing rules are unchecked. If you mutate
  through two pointers to the same location, that's your problem.
- **General predicate refinements.** No SMT. No
  `i32 {x > y}`-style inter-parameter relations.
- **Region inference across function boundaries.** Functions take
  areas explicitly; areas aren't inferred from call-graph
  analysis.
- **Effect tracking.** No `pure fn` vs `io fn` distinction.
- **Generics.** Containers stay manually monomorphised
  (`VecI32`, `VecU8`, …). Generics — comptime or otherwise —
  are a separate identity question we deliberately deferred.

If a future identity bump wants any of these, that's a new design
doc, not an extension of this one.

---

## References

- Cyclone's region system: Grossman, Hicks, et al. — *Region-Based
  Memory Management in Cyclone* (PLDI 2002). The
  "regions-in-types" version of what we're calling option C.
- Liquid Haskell / SPARK Ada: refinement-types reference
  implementations. We're committing to a strict subset (intervals
  only).
- Zig's allocator pattern: closest mainstream cousin to areas. Tjlb
  goes further by making lifetimes part of type-checking, not just
  runtime convention.
