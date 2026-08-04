# Sum Types + `switch`

Status: **reviewed — approved with changes (language-designer,
2026-08-03, two passes: the core design and the same-day `is`
addition); all required changes from both are folded in below.
Implementation may begin at Stage A.** This is step 1
of the build order in `Pay-As-You-Go-Correctness.md` made concrete:
declaration syntax, construction, layout, the `switch` statement,
exhaustiveness, and staging. The keyword `sum` is a settled decision
(maintainer's call, 2026-08-03): a new keyword that says what the thing
is, rather than overloading `type` or C's `union`/`enum`.

---

## Goals / non-goals

**Goals (v1):**
- Tagged sum types as first-class value types: declare, construct, copy,
  pass/return by value, embed in structs/arrays, point at.
- `switch` as the *only* way to observe a sum's variant and payload —
  no tag field, no payload accessor, so a sum value can never be read
  as the wrong variant.
- Exhaustiveness checking as specified in the parent doc (four stances).
- `switch` also subsumes the classic C use: integer/bool/enum
  scrutinees with constant patterns.

**Non-goals (v1), recorded so they're deferrals not oversights:**
- `switch` as an expression (use a value block around it, or wait).
- Nested patterns (`case Add(Lit(x), r)`) — one level deep only.
- Pattern guards (`case Some(n) if n > 0`).
- Range patterns (`case 1..=9`) — wants shared range syntax with
  refinements (roadmap step 3); design them together later.
- Pointer and float scrutinees. Floats are rejected outright (equality
  matching on floats is a footgun; use `if`/`elif`). Pointers are
  deferred, not rejected.
- Generic sums (`Result<T, E>`) — no generics yet; hand-monomorphize
  like `VecI32` (`sum ResultI32 { Ok(i32 value), Err(i32 code) }`).
- `==` on sum values (structs don't have it either).
- Methods on sums. UFCS (roadmap step 5) will give `shape.area()` via
  free functions; no reason to build a second method system now.
- Negation binding / guard-clause style (`if !(s is Circle) { return }`
  then using `r` after) — Rust needed `let-else` for this; the
  right future home is flow narrowing (roadmap step 2), not a v1
  contortion here.

---

## Declaring a sum

```aspect
sum Shape {
    Circle(f64 radius)
    Rect(f64 w, f64 h)
    Dot
}
```

- Top-level only, like `type` and `enum`. Registered in the same
  prescan that pre-registers `type` names, so forward references and
  mutual recursion (through pointers) work with no declaration order
  sensitivity.
- One variant per line, newline-separated — the `type` field style, not
  `enum`'s comma style; variants with payloads are too heavy for a
  comma list.
- A variant is a bare identifier (no payload) or an identifier with a
  parenthesized field list in function-parameter style: type-first,
  **names required** (`Rect(f64 w, f64 h)`, never `Rect(f64, f64)`).
  Names are for diagnostics and doc value; matching is positional.
  Empty parens `Dot()` are a parse error — a payload-less variant is a
  bare name.
- At least one variant; variant names unique within the sum; field
  names unique within their variant.
- Payload fields admit any sized type: integers, floats, `bool`,
  pointers, structs, other sums, fixed-size arrays — same rules as
  struct fields. A sum can never contain itself by value, directly or
  through a struct/sum cycle (same check as `type`); indirection
  through a pointer is fine, which is what makes list/AST nodes work.
- `public sum` exports it to importers, like `public type`. **No
  per-variant visibility**: variants are the sum's interface —
  exhaustive matching from another module is impossible if some
  variants are hidden. A non-public sum is fully opaque outside its
  module, same as a plain `type`.

Variant names live in the sum's namespace (`Shape.Circle`), not the
global one — two sums may share variant names, and the flat-namespace
v1 caveat doesn't grow.

### Relation to `enum`

A payload-less sum overlaps with `enum`. They stay distinct: `enum` is
an `i32`-backed value set with explicit casts both ways (the C-interop
shape); a sum has no integer identity and no casts. Guidance goes in
the handbook: reach for `enum` when the integer matters, `sum` when the
shape does. Possibly `enum` becomes sugar for a payload-less sum some
day; out of scope here.

---

## Constructing a sum

```aspect
Shape s = Shape.Circle(2.5)
Shape d = Shape.Dot
```

- `SumName.Variant(args…)` is an expression of type `SumName`,
  by value. It reads as — and typechecks as — a call: arity must match
  the variant's field count, each argument checks against the field's
  type with the ordinary implicit-coercion rules (widening fine,
  signedness change warns, narrowing needs `as`).
- Payload-less variants construct without parens, mirroring
  `EnumName.Variant`. Parens on a payload-less variant, or a bare
  `Shape.Circle` without its arguments, are errors.
- **Constant in global initializers (deferral lifted 2026-08-04).**
  `Shape g = Shape.Dot` at file scope folds: the constant is an
  anonymous padded mirror of the storage layout. Still deferred:
  sum-typed *fields* inside constant struct literals and sum *elements*
  in constant array initializers (the anonymous type can't embed in a
  named constant) — both rejected with precise diagnostics.
- The parser already resolves `Name.` for enum access and static
  methods; sum construction joins that same resolution site.
- Sums are ordinary aggregates thereafter: assignable, copyable,
  passable and returnable by value (same byval machinery as structs),
  storable in struct fields, arrays, and behind pointers.
  `sizeof(Shape)` works and includes padding.

There is **no other operation** on a sum value: no tag read, no field
access, no cast. `switch` and `is` are the only observers. That's the
invariant that makes "no manual tag bookkeeping" true rather than
aspirational.

---

## The `switch` statement

```aspect
switch s {
    case Circle(r) {
        area = PI * r * r
    }
    case Rect(w, h) { area = w * h }
    case Dot { area = 0.0 }
}

switch code {
    case 0 { println("ok") }
    case 1, 2 { println("warn") }
    default { println("err") }
}
```

Syntax, in Aspect's existing idiom (no parens around the scrutinee, no
colons, mandatory braces — `switch`/`case` follow `if`/`while`, not C):

- `switch <expr> {` followed by arms, each `case <pattern-list> {…}` or
  `default {…}`. Arm braces are mandatory like every other body.
- No fall-through; each arm is a block; nothing like C's `break`-to-exit
  exists or is needed.
- **`break`/`continue` inside an arm bind to the enclosing loop**, as
  they do in value blocks — `switch` is not a loop and is not a `break`
  target. This inverts C muscle memory and gets a loud handbook call-out.
  A `break` in an arm with no enclosing loop is the usual error.
- `default`, if present, must be the **last** arm.
- Each arm opens a child scope; pattern bindings live there.
- The scrutinee is evaluated exactly once.
- **Termination analysis:** a `switch` counts as always-returning iff
  it is coverage-complete (fully-listed sum/enum, `bool` with both
  literals, or `default` present) **and** every arm's block
  always-returns. `Checker::stmt_always_returns` recognizes only
  `Return`/`Block`/two-armed `If` today and must learn this case — it
  is what makes the value-block idiom below actually legal, and
  codegen's fall-off handling relies on the checker's guarantee.

**Scrutinee types (v1):** any integer type, `bool`, `enum`, or a sum.
Floats are an error ("use if/elif"); pointers are an error for now. No
auto-deref: matching through a `Shape*` is `switch *p` — the bare
pointer form is reserved against a future pointer-matching meaning.

**Patterns:**

- Integer scrutinee: integer literals, including negative literals and
  `$define`-expanded constants (free — expansion precedes parsing).
  Each literal must fit the scrutinee's type (existing literal-narrowing
  rules). `const` globals as labels are deferred (needs const-eval);
  the dispatch-table precedent already draws this same line.
- `bool` scrutinee: `true` / `false`.
- Enum scrutinee: variant references.
- Sum scrutinee: `Variant(x, y, _)` binds payload fields positionally
  as fresh locals of the field types (arity must match exactly; `_`
  discards a position); bare `Variant` matches while ignoring any
  payload. Bindings are **copies** — ordinary mutable locals; writing
  one does not write the sum.
- **Why copies, not references** (challenged and upheld 2026-08-04):
  a reference binding is a typed view into storage whose tag may be
  rewritten while the view lives (`s = Shape.Rect(…)` inside a
  `Circle(r)` arm), which reads the wrong variant through a stale view —
  the exact bug class sums exist to kill, and without a borrow checker
  nothing can forbid the rewrite. The design's own canonical idiom,
  `while node is Cons(h, t) { node = *t }`, rebinds the scrutinee while
  bindings live and is only correct under copies. Costs are illusory
  for the common case (scalar/pointer payloads SROA away; big payloads
  follow the boxing idiom, where binding the pointer gives reference
  semantics explicitly). Recorded deferral: an explicit opt-in capture
  (`case Circle(&r)`) can be added compatibly later if in-place payload
  mutation proves needed — a copy default can be loosened by opt-in;
  a reference default could never be tightened back.
- Enum and sum variant patterns are written **unqualified** (`case
  Circle(r)`, `case Red`): the scrutinee's type names the namespace, so
  qualification adds nothing. The qualified form (`case Shape.Circle(r)`)
  is also accepted for greppability; both resolve identically. This is
  a deliberate, narrow exception to "access only via `Name.Variant`" —
  it exists only inside `case`, where the type is already pinned.
- A `case` may carry a comma-separated pattern list **only when no
  pattern binds** (constants, bare variants). A binding pattern stands
  alone in its arm — no binding unification across alternatives.
- Duplicate coverage — the same constant value (after evaluation:
  `0x10` duplicates `16`) or same variant appearing twice — is an error.

**Exhaustiveness** — one rule, "a switch must cover its scrutinee;
`default` covers the rest," yielding the parent doc's four stances:

1. Integer scrutinee → `default` **required** (never enumerable in
   practice, even for `u8`; one rule, no width special-cases).
2. `bool` → `true` + `false` is complete; otherwise `default` (or the
   missing literal) required.
3. Sum or enum, every variant listed → exhaustive. A `default` here is
   **dead** and draws a non-fatal warning — it would silently swallow
   future variants, which is the failure mode exhaustiveness exists to
   prevent.
4. Sum or enum, variants missing, no `default` → **error naming the
   missing variants**; with `default` → legal, checking waived
   knowingly.

Stance 3 deliberately revised the parent doc's original "open enum →
default required" line (the parent doc has since been reconciled to
match): Aspect enums are *nominally closed* — the only route to
an out-of-range value is a forged `as` cast. Requiring `default` on a
fully-listed enum switch would forfeit the add-a-variant payback for
enums entirely, which is the feature's whole point. Forged values are
handled below, so exhaustiveness never lies.

**Lowering** (nothing codegen doesn't already do): evaluate the
scrutinee to a temp (alloca for sums); for sums load the tag (sized to the variant count since 2026-08-04);
LLVM `switch` over tag/value to per-arm blocks; in a sum arm, GEP the
payload through the variant's view (opaque pointers — no bitcast
ceremony) and copy fields into binding allocas. The `switch`'s else
edge is the `default` block when present; on a fully-listed sum/enum
switch it is a **trap block calling `llvm.trap`** (lowers to `ud2`),
not `unreachable` and not libc `abort()`: a tag can be forged through
the `u0*` bridge or a stale pointer, one cold branch target buys a
debuggable halt instead of UB — and `llvm.trap` keeps the freestanding
targets (i386 kernel, no libc) free of a hidden libc dependency in
every fully-listed switch. Because the JIT test harness runs
in-process, an executed trap would kill `cargo test`: the trap edge is
asserted by a Rust unit test over emitted IR, never a runtime corpus
program. Bool lowers to a conditional branch.

---

## `is` — the single-variant probe

Added 2026-08-03 (maintainer request): `switch` is the exhaustive
matcher; `is` is the tool for "I care about one variant here" without
dragging out a full switch.

```aspect
if s is Circle(r) {
    area = PI * r * r
} elif s is Rect(w, _) {
    area = w * w
}

while node is Cons(head, tail) {
    total = total + head
    node = *tail
}
```

- `expr is Pattern`, where the pattern grammar is exactly `case`'s:
  bare `Variant` (matches, ignores payload), `Variant(x, _, y)`
  positional bindings with `_` discards, unqualified names resolved by
  the scrutinee's type (qualified accepted).
- **Two forms with different reach:**
  - *Binding-free* (`s is Circle`) — an ordinary `bool` expression,
    usable anywhere (`bool round = s is Circle`,
    `if s is Circle || s is Dot { }`, `!(s is Rect)`). Precedence at
    the comparison tier. The two-form classification is **syntactic**:
    only a bare `Variant` is the expression form — any parenthesized
    pattern, including all-discard (`Circle(_)`), is the
    condition-restricted form. (Deliberate: whether code under `||`
    compiles must not flip when a binding is renamed to `_`; bare
    `Variant` already is the ignore-payload expression form.)
  - *Binding* (`s is Circle(r)`) — **not an expression**; grammatically
    a condition production, legal only as a **top-level `&&`-conjunct**
    of an `if`/`elif`/`while` condition (amended 2026-08-04,
    maintainer request — was "entire condition"). In
    `if s is Circle(r) && r > 2.0 { … }`, bindings from conjunct *i*
    are in scope for conjuncts *i+1…* and the success block —
    short-circuit evaluation guarantees a later conjunct only runs
    after the match succeeded, so a binding is always initialized when
    read. Bindings die with the success block. Rust's if-let-chains
    rule, and decidable for the same reason: a binding `is` remains
    illegal under `!` and `||` (and in `for` headers), where
    "did it match" and "is the binding readable" would diverge —
    that, not chaining, was always the C#-definite-assignment trap.
    Bindings are payload **copies** — ordinary mutable locals scoped
    to the success block; the `while` form re-evaluates the whole
    condition chain and re-binds fresh copies each iteration.
    Chain mechanics, pinned by the third review pass (2026-08-04):
    - **"Top-level" means the leaves of the condition's root `&&`
      spine.** Any `||`, `!`, comparison, or parenthesization *above*
      the `is` demotes it to expression position and fires the
      not-an-expression diagnostic — so in
      `if s is Circle(r) && x || y` the root is `||` and the binding
      `is` is illegal, even though it sits textually beside `&&`.
    - **All chain bindings share one scope** (the success-block
      scope): `a is Circle(r) && b is Circle(r)` is the ordinary
      same-scope redeclaration error, not per-conjunct shadowing.
      Shadowing an *outer* variable falls out of normal scope rules.
    - **Bindings register at pattern-parse time, in textual order** —
      this is forced, not a choice: the parser errors on undefined
      variables and resolves field-vs-method at `.` from parse-time
      types, so `s is Wrapped(p) && p.x > 0` only parses if conjunct
      1's bindings (with payload types from the scrutinee) are in the
      symbol table before conjunct 2 parses. Consequently
      `parse_if_statement`/`parse_while_statement` need either a
      condition production that splits at the `&&` tier or in-parse
      registration with post-hoc position validation. A binding read
      in a conjunct *before* its binding conjunct is the ordinary
      undefined-variable error.
  - The binding form is deliberately excluded from **`for` headers** in
    v1 (bindings would have to scope across the increment clause; the
    `while` form covers the walk pattern) — the binding-free form works
    there automatically as an ordinary bool expression, and the
    not-an-expression diagnostic fires on the binding form.
  - The RHS of `is` is parsed by the **pattern parser, never
    `parse_expression`** — otherwise bare `Variant` followed by the
    body's `{` could collide with the struct-literal production
    (`ident '{'`) when a variant name coincides with a type-struct
    name.
- **Sums only (v1).** On enums it would duplicate `==` (the diagnostic
  says so); on integers/floats/pointers it's an error.
- **`is` is the deliberate exhaustiveness opt-out.** A `switch` claims
  coverage of its scrutinee; an `is` claims interest in one variant.
  Add a variant next year and `is` sites are *not* flagged — the same
  contract as `default`, and its whole point. The handbook states this
  trade explicitly so switch-vs-`is` is an informed choice.
- **Poor-man's `?`:** `if r is Err(code) { return code }` covers the
  Result early-return pattern until roadmap step 4 lands the real `?`.
- **Modules:** requires visibility of the sum, same as construction
  and `switch`.
- **Keyword:** `is` becomes reserved. Corpus/stdlib grep: prose-comment
  hits only, zero identifier uses — no renames.
- **Lowering:** tag load + compare against the variant's constant +
  cond-br; payload GEP'd and copied into binding allocas in the true
  block. Chains reuse the **existing `&&` short-circuit lowering** —
  each binding conjunct's payload copies are emitted on its true edge
  before the next conjunct's evaluation block, so the success block is
  reachable only through paths on which every binding was stored. No
  fused-condition machinery. No exhaustiveness machinery, no trap
  edge — strictly a subset of Stage D's codegen.

## Interactions checklist

- **Modules**: `public sum` construction and matching both require
  visibility of the sum; importers see all variants or none. Same for
  enums: switching on a value whose type is a foreign-*private* enum is
  an error (matching requires type visibility) — private-enum values do
  flow through importers as opaque handles today, so the case is
  reachable.
- **Coercion and casts**: sums are nominal, identity-only in
  `types_coercible` (like enums); `as` to or from a sum type is invalid
  in **both** directions (`cast_valid` learns this). A sum value is
  only ever its own type.
- **Preprocessor**: nothing new; `$define` constants work as case
  labels by construction.
- **Value blocks**: a `switch` inside a value block is a statement like
  any other; `return` in its arms binds to the value block per existing
  rules. (This is the v1 idiom for "switch as expression".)
- **`const`**: a `const Shape` (or reads through `const Shape*`) can be
  switched on or probed with `is` — matching only reads. Bindings are
  copies, so no const-ness escapes into them.
- **`extern` / ABI**: sum layout (tag at offset 0 — since 2026-08-04 sized
  to the variant count — payload after,
  max-size/max-align) is documented as **unspecified/internal** — sums
  are not a C-interop type. Nothing blocks taking a pointer to one, as
  with structs; it's the programmer's problem. Sum-typed parameters and
  returns on `extern fn` follow exactly the by-value-struct rule
  (currently "awaits per-target ABI work") — whatever structs do at
  that boundary, sums do.
- **Keywords**: `sum`, `case`, `default`, `is` become reserved. Repo
  fallout for `is`: none (comments only). Repo fallout for `sum`
  today (full grep, Stage A renames all of it): local `sum`
  accumulators in `break_continue.ap`, `value_block.ap`,
  `control_flow.ap`, `asm_arith.ap`, `narrow_width.ap`; method
  `Pair.sum` in `methods.ap` and `rvalue_materialization.ap`; free
  `fn sum` in `struct_byvalue.ap`; public method `sum` in
  `tests/modules/shapes.ap` with its call site in
  `module_public_export.ap`; and `demos/vm.ap` + `demos/vec_demo.ap`
  (demos aren't CI, but they'd stop compiling — renamed too).
  `case`/`default` occur only in comments. Handbook keyword list,
  `09-syntax-reference.md`, and the handbook skill all update. This is
  the accepted cost of the accurate name; `sum` is a common accumulator
  identifier and will occasionally bite users — the diagnostic for
  "keyword in identifier position" should be friendly.

## Diagnostics (new)

Parse: empty sum; duplicate variant; unnamed payload field; empty
payload parens; `case`/`default` outside `switch`; `default` not last;
missing arm braces; duplicate type name (`sum` colliding with
`type`/`enum`/`alias` in the one type namespace). Type: sum contains
itself by value; unknown variant (when the pattern is `_`, the message
suggests `default` — Rust muscle memory will produce `case _`);
construction arity/type mismatch; sum construction in a global
initializer ("not a constant expression"); bare payload-variant
reference without args; parens on payload-less variant; `as` cast to or
from a sum type; float/pointer scrutinee; non-exhaustive switch (names
missing variants; an empty `switch x { }` needs no dedicated rule —
every scrutinee class already errors through the exhaustiveness
stances); missing `default` on integer switch; duplicate case
value/variant; pattern arity mismatch; binding pattern in a pattern
list; duplicate binding name within a single pattern (`Rect(w, w)` —
shared `case`/`is` grammar, lands with Stage D); binding `is` outside
a top-level `&&`-conjunct of an `if`/`elif`/`while` condition ("a
binding `is` is not an expression"; the message hints "write it as an
unparenthesized `&&`-conjunct of the condition", since C muscle memory
produces `if (s is Circle(r)) {` — the parens flip it into expression
position — and the fixture uses that spelling; a binding `is` under
`!` or `||` gets the same treatment); `is` on a non-sum scrutinee (for enums the message suggests
`==`; distinct message text from the int/float flavor, so its own
fixture); **warning** (non-fatal): dead `default` on a fully-listed
sum/enum switch.

---

## Staging

Each stage lands with its docs and tests (repo rule); every stage
leaves master green.

- **Stage A — inert declarations. ✅ Landed 2026-08-04.** Lexer keywords
  (`sum`, `case`, `default`, and `is` pulled forward from Stage E — all
  four reserved at once) + corpus identifier renames (13 files);
  `sum` declarations parsed into `ModuleSymbols` via the prescan
  pattern; storage layout (`{ i32 tag, [k x iN] }`) + `sizeof`;
  nominal coercion + total `as`-cast rejection; `public sum` module
  visibility. Landing note: the by-value containment cycle check turned
  out to be **missing for type-structs too** (`type Node { Node inner }`
  compiled silently) — implemented once for the combined struct/sum
  graph at end of parser pass 1, fixing the latent struct hole.
  No construction, no `switch`. (Precedent: Transforms Stage B.)
- **Stage B — values. ✅ Landed 2026-08-04.** `SumName.Variant(…)`
  construction, copy/assign/pass/return by value (`sret`/`byval` like
  structs), sums in structs/arrays/pointers, global-initializer
  rejection, `==`/arithmetic rejected on aggregate values. Landing
  notes: the layout was revised to a **uniform payload offset**
  (payload never packed into the tag's padding — LLVM first-class
  copies preserve fields, not padding, so packed payload bytes could
  vanish on copy; `List` grew 16→24 accordingly). Two pre-existing
  holes surfaced and were fixed at the root: whole-value `==` on
  type-structs crashed the compiler (aggregate identity satisfied
  `types_coercible`, codegen expected a scalar), and the
  declaration-vs-expression lookahead (`Type *x`) didn't know sum
  names.
- **Stages C+D — `switch`. ✅ Landed together 2026-08-04.** Constant
  patterns (multi-pattern arms, negative labels, `$define` constants),
  destructuring with positional binders/`_`/bare variants, all four
  exhaustiveness stances, duplicate detection post-evaluation,
  dead-`default` warning (`# expected_warning` corpus test), the
  `llvm.trap` else edge (IR-asserted by unit tests), scrutinee evaluated
  once into a slot, `break`/`continue` passing through to the loop, and
  the `complete`-flag termination rule (computed dedup-aware by the
  parser so `stmt_always_returns` stays registry-less). Landing note:
  arm bindings register into the parse-time symbol table before the
  body parses — the same mechanism the third review pass identified as
  forced for `is` chains — and bare enum variant patterns resolve
  against the scrutinee's enum unless the identifier names an enum
  (which falls through to qualified resolution).
- **Stage E — `is`. ✅ Landed 2026-08-04 — the feature line is
  complete.** Both forms with the `&&`-chaining amendment (bindings flow
  left-to-right through the spine, one shared scope, parse-time
  registration); parens/`||` policed in the parser, everything else by
  the checker's synthesis arm; lowering per spec (probe = slot + tag
  compare; copies on the matched edge). Landing notes: two more
  pre-existing bugs surfaced and were fixed at the root — **`&&`/`||`
  did not short-circuit** (eagerly evaluated both sides via `select`;
  the chain guarantee forced the real fix, which `p != null && *p`
  needed anyway), and **`if`/`while` bodies had no codegen scope**, so
  a body-local shadowing an outer variable silently clobbered it for
  the rest of the function. Also lifted post-review: **sum construction
  now folds into global initializers** (anonymous padded constant
  mirroring the storage layout; storage alignment forced) — the v1
  deferral below stands only for sum-typed fields/elements inside
  constant struct/array initializers.

Docs owed across the stages, beyond the handbook /
`09-syntax-reference.md` / skill updates already noted:
`doc/compiler/02-parser.md`, `03-ast.md` (new nodes),
`05-typechecker.md` (exhaustiveness + the termination rule),
`06-codegen.md` (layout, switch lowering, trap edge). And
`Pay-As-You-Go-Correctness.md`'s four-stance list must be reconciled to
the nominally-closed-enum rule below so the two docs don't contradict.

## Test plan

Runtime corpus: one `sum_types.ap` (construct, copy, byval pass/return,
sums in structs/arrays, pointer-to-sum, `sizeof`), one `switch.ap`
(integer/bool/enum sections summed in `main`, including a value block
wrapping a coverage-complete switch whose arms all `return`), one
destructuring program folding in a recursive case (mini expression tree
eval — exercises pointer-indirected recursion) plus `is` sections
(binding-free in boolean expressions, `if`/`elif` bindings, the
`while … is Cons` list walk). Module fixture:
`public sum` constructed and matched from an importer. Failure fixtures
(one-per-file as required): the parse/type diagnostics above, `parser_`
/ `type_` prefixed — including a value block containing a
*non*-covering switch (fails the every-path-returns rule). The
dead-`default` warning rides a runtime corpus program via the harness's
`# expected_warning:` annotation (precedent: `signedness_warning.ap`).
The forged-tag trap edge cannot be a corpus program (an executed trap
kills the in-process JIT harness) — it is asserted by a Rust unit test
over emitted IR. Chain coverage (third review pass): runtime — a chain
whose match succeeds but whose guard fails (success block skipped), a
binding-`is` as a non-first conjunct, two binding-`is` conjuncts where
the second scrutinee dereferences the first's binding, and a `while`
chain exited by the *guard* conjunct while the match still succeeds
(per-iteration re-binding observable). Failure fixtures — cross-conjunct
duplicate binding (`a is Circle(r) && b is Circle(r)`, redeclaration
error); the `… && x || y` demotion spelling (pins the root-spine
definition, distinct from the parens fixture); a binding read in a
conjunct before its binding conjunct (undefined variable).
`-O0`/`-O2` agreement comes free from the harness.

## Review resolutions (language-designer, 2026-08-03)

All five open questions resolved in favor of the proposal's choices:

1. **Colon-less `case Pattern {…}` — kept.** The colon in C serves
   labels and fall-through, both absent here; mandatory `{` terminates
   the pattern list unambiguously. Supersedes the parent doc's
   `case 3:` sketch.
2. **Unqualified variant patterns — kept** (reversed post-landing, see
   Post-landing amendments below), qualified form also
   accepted. The scrutinee pins the namespace; per-arm `Shape.` is
   exactly the re-demanded repetition the identity doc rejects.
   In-language precedent: struct-literal field names are already bare
   in a type-pinned context. (Qualification would buy no preprocessor
   protection anyway — `$define` rewrites any identifier token,
   including after `.`; pre-existing hazard, handbook footnote.)
3. **Trap edge — kept**, pinned to `llvm.trap`. Consistent with
   "boundaries never lie": refinements will later insert aborting
   runtime checks, so a trapping else-edge is precedent, not anomaly.
   Backend-portable (QBE has `hlt`).
4. **Bare `Variant` on a payload variant — kept.** Construction must
   supply data; matching may discard it. `Variant(_, _)` would
   re-demand arity knowledge that binds nothing and churn on field
   additions.
5. **`default` must be last — kept.** Free placement in C exists only
   to serve fall-through; last-position matches reading order to
   checking order and simplifies both diagnostics.

A second pass the same day reviewed the `is` addition: **approved with
changes** (all folded — `for`-header exclusion stated, duplicate
binding-name diagnostic added, `is` RHS pinned to the pattern parser,
fix-hint on the not-an-expression diagnostic, `const` bullet extended).

A third pass (2026-08-04) reviewed the maintainer's `&&`-chaining
amendment: **approved with changes**, all folded — root-`&&`-spine
definition of "top-level", shared success-block scope for chain
bindings, parse-time binding registration recorded as forced by the
parser's design, syntactic two-form classification (bare name vs any
parens), chain lowering via the existing short-circuit machinery, and
the chain test additions. The reviewer confirmed the precedence split
is unambiguous (the pattern parser can never swallow `&&`) and that
copies — not references — are what make `while` chains re-bind safely.
The reviewer independently confirmed the expression/condition split is
sound at the comparison tier (`!s is Rect` and `a is B is C` both fail
cleanly on scrutinee typing rather than surprising), that unparenthesized
conditions make "entire condition" a real grammatical position, and
that the keyword `is` has zero identifier-position uses in the repo.

## Post-landing amendments (2026-08-04, after real use)

Three maintainer decisions from the first day of actually writing sums:

1. **Qualification is mandatory** — `case Shape.Circle(r)`, `s is
   Shape.Circle`, `case Color.Red`. This reverses review resolution 2
   (unqualified patterns): inferring the type from the scrutinee read as
   non-transparent in practice — a pattern now spells its type the way
   every other variant access does. Bare variant names error with the
   exact fix ("variant patterns are qualified — write `Shape.Circle`").
2. **Single-level pointer scrutinees auto-deref** for both `switch` and
   `is`, lifting the v1 pointer exclusion — `switch p` and `p is
   Shape.Circle(r)` on a `Shape*` match the pointee (and skip the
   whole-value copy; the pointer is the slot). Null derefs are the
   user's problem, deliberately unchecked. Deeper pointers still error.
3. **`u0*` ↔ `Sum*` casts confirmed as designed** — the explicit `as`
   and the depth-1 implicit bridge both work (sum *values* still admit
   no casts); now pinned by corpus tests.
