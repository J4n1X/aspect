# Ergonomics Overhaul — Making Aspect Pleasant to Extend

Status: **proposal.** No implementation, no commits. This doc
exists to answer one question: *what would make this codebase pleasant to
work on for the next five years, at any cost short of starting over?*

Explicitly **not** a cleanup list. Cosmetic fixes (bool parameters,
`unreachable!` accessors, dead comments) are excluded — they are real but
they do not bite often enough to matter. Everything below is chosen for
one property: it reduces the number of places you must edit to express a
new idea.

---

## Method

Measured against the tree at `4b23317`, not inferred from the docs.

| Measurement | Value |
|---|---|
| Total Rust (`src/` + `aspect-macros/src/`) | 20,492 lines |
| Largest file | `parser/expressions.rs`, 991 |
| Warm `cargo check` | ~1.1 s |
| Corpus tests | 135 |
| Sites poking `LangType` internals | 217 |
| Hand-written `ExprKind` walkers | 4 |
| Hand-written `StatementKind` walkers | 3 |
| Error enums / total variants | 5 / 107 |
| `Position` mentions across `src/` | 929 |

Two of those numbers matter more than the rest, and neither is the one
that gets complained about. The codebase is **small** and the feedback
loop is **fast**. Size and build time are not the problem. Change
amplification is.

---

## Diagnosis: where the pain actually comes from

Adding one expression form to Aspect today requires, at minimum:

1. a variant in `ExprKind` (`parser/ast.rs`)
2. construction in `parser/expressions.rs`
3. a synthesis/check arm in `typechecker/checker/expressions.rs`
4. a lowering arm in `codegen/expressions.rs`
5. a folding arm in `codegen/const_eval.rs`
6. one or more error variants, each with `Position` threaded by hand
7. possibly a `LangType` shape that the flat representation cannot express

Steps 3–5 are three separate hand-written traversals of the same tree.
Step 6 is 107 bespoke format strings. Step 7 is the wall that stopped
flow narrowing before it started.

Every idea in this document attacks one of those three.

---

## Move 1 — One traversal, three interpretations

**The problem.** `ExprKind` has ~20 variants. It is matched 34 times in
the checker, 29 in codegen, 26 in `const_eval`. `StatementKind` is
matched 16 / 16 / 13 in parser / checker / codegen. These are not
incidental — they are the same walk, three times, with different payloads
at the leaves.

`codegen/const_eval.rs` (400 lines) is the clearest case: a shadow copy
of `codegen/expressions.rs` (759 lines) that folds in Rust instead of
emitting IR. The insight was already had — `ValueEmitter` exists precisely
to share `RuntimeEmitter` and `ConstantEmitter` behaviour — but it shares
the **leaves** and duplicates the **walk**. It is inside out.

**The move.** A fold with an associated output type:

```rust
pub trait ExprFold {
    type Out;
    fn literal(&mut self, lit: &LiteralValue, ty: LangType, pos: Position) -> Self::Out;
    fn binary(&mut self, l: Self::Out, op: BinaryOp, r: Self::Out, ty: LangType, pos: Position) -> Self::Out;
    fn call(&mut self, name: SymbolId, args: Vec<Self::Out>, ty: LangType, pos: Position) -> Self::Out;
    // … one method per variant, each receiving *already-folded* children
}

pub fn fold_expr<F: ExprFold>(f: &mut F, e: &Expression) -> F::Out;
```

One recursion, written once. `const_eval` implements it with
`Out = BasicValueEnum` and constant leaves; runtime codegen implements it
with `Out = BasicValueEnum` and builder leaves. `const_eval.rs` collapses
to its genuinely distinct part — the "is this foldable" refusals — and
loses roughly 300 lines.

**The honest caveat.** This does *not* fit the typechecker as it stands.
Bidirectional checking pushes an expected type *down* into children
(`check_expression`), and a bottom-up fold only carries information up.
Two options, both real:

- Keep the checker hand-written and take the win on codegen + const_eval
  only. Cheaper, still deletes ~300 lines, still means two walkers instead
  of three.
- Generalise to a fold with a downward context parameter
  (`type Ctx; fn binary(&mut self, ctx: Ctx, …)`). This does model
  bidirectional checking, at the cost of a more abstract trait. Higher
  ceiling, more design risk.

**The payback that matters.** After this, adding an expression form means
adding one trait method with a default. Phases that do not care about the
new node do not change *and still compile*. That is the difference between
a feature costing four files and costing one.

**Cost.** 800–1200 lines rewritten. Risk: medium — mechanical, and the 135
corpus tests are a genuine safety net at both -O0 and -O2.

---

## Move 2 — Arena AST with `ExprId`, and side tables

**The problem.** The AST is `Box<Expression>` / `Vec<Expression>`
throughout (18 sites in `ast.rs`). Consequences:

- `expr_type` is *stamped in place* by the checker — the AST is mutated
  during checking, so nothing can hold a reference across a check
- no stable identity for a node, so no way to key information about an
  expression *outside* the node
- clone is deep; `&mut` recursion fights the borrow checker constantly

**The move.** Nodes live in `Vec<Expression>` inside the `Program`;
children are `ExprId(u32)`. `Copy`, comparable, hashable.

The payoff is not memory. It is that **side tables become possible**:

```rust
struct Analysis {
    types:       IndexVec<ExprId, LangType>,
    refinements: IndexVec<ExprId, Option<Refinement>>,
    narrowed:    HashMap<(ScopeId, SymbolId), TypeId>,
}
```

Every future analysis attaches to expressions without touching `ExprKind`,
without another mutation pass, and without a new field on a shared struct.
Open question 1 in `Pay-As-You-Go-Correctness.md` — side table vs. field —
stops being a dilemma, because side tables become the cheap default.

**Cost.** 1000–1500 lines. Risk: medium-high; it touches every phase, but
each edit is shallow and local (`&expr` → `ast[id]`).

---

## Move 3 — Interned types: `LangType` becomes an id

**The problem.** 217 sites read `pointer_depth` (103), `size_bits` (31),
`array_size` (30), `is_const` (27). Structure is encoded in scalar fields
rather than in the type:

```rust
pub struct LangType { base, size_bits, pointer_depth: u32, is_const, array_size: Option<u32> }
```

`pointer_depth: u32` is `Ptr(T)` flattened to a counter. `array_size:
Option<u32>` is `Array(T, n)` flattened. `size_bits` is meaningless for
structs (`0` by convention) and `32` for enums by a different one.

Downstream symptoms, all traceable to this one decision:

- `element_type()` and `decay_to_pointer()` rebuild the struct field by
  field because there is no structure to recurse on
- `Display` special-cases `Bool`, then nominal types, then needs
  `unreachable!("handled above")`
- `Display` cannot reach the registry, so a **second type printer** exists
  — `checker/expressions.rs:339`, three identical branches for
  Struct/Enum/Sum
- error variants carry **pre-rendered `String`s** (`AsmUnpinnableType {
  found: String }`) because `LangType`'s own rendering is inadequate
- six predicates (`is_void_value`, `is_opaque_ptr`, `is_plain_int`,
  `is_plain_float`, `is_plain_numeric`, `is_pointer_like`) that a real ADT
  would express as `matches!`

**The move.** Hash-consed registry; `LangType` becomes `TypeId(u32)`.

```rust
enum Type {
    Void, Bool,
    Int { signed: bool, bits: u32 },
    Float { bits: u32 },
    Ptr(TypeId),
    Array(TypeId, u32),
    Const(TypeId),
    Struct(StructId), Enum(EnumId), Sum(SumId), Fn(SigId),
}
```

Still `Copy`, so signatures keep their shape. Because it is hash-consed,
**type equality becomes a `u32` compare** — cheaper than today's
five-field comparison. The registry rides on `Program` as an `Rc`,
exactly as `ModuleSymbols` already does.

Refinements then fall out as `Refined(TypeId, Refinement)` — a variant,
not a bolt-on — and flow narrowing gets the lattice it currently lacks:
narrowing a binding is rebinding a name to a different `TypeId`.

**The real risk, stated plainly.** Today `ty.pointer_depth` is free.
Tomorrow it is `reg.kind(ty)`. Thread the registry badly and this makes
ergonomics *worse* at 217 sites. Mitigations:

- pre-intern primitives as constants (`TypeId::I32`) so common comparisons
  stay a `u32` equality with no registry access
- put the accessors on a `Types<'_>` view bundling `&Registry`, so call
  sites read `t.is_pointer()` rather than `reg.is_pointer(t)`
- **prototype on `codegen/types.rs` alone before committing to the rest**

**Cost.** 1500–2500 lines. Risk: high. This is the one that must be
prototyped before it is chosen.

---

## Move 4 — Spans and one diagnostic type

**The problem.** 5 error enums, 107 variants, 929 `Position` mentions.
Every variant carries its own `Position` and every message string ends in
a hand-written `at {position}`. And `Position` is a **point**:

```rust
pub struct Position { line: usize, column: usize, file_id: u32 }
```

No length. So no diagnostic can ever underline the offending text, and no
error can carry two labelled locations ("expected because of this return
type" / "found here"). That ceiling is baked into 178 declaration sites.

**The move.** `Span { file_id, start: u32, len: u32 }` on tokens and AST
nodes, and one diagnostic type for the whole compiler:

```rust
pub struct Diagnostic {
    severity: Severity,
    code: Option<&'static str>,   // "E0142" — greppable, documentable
    message: String,
    primary: (Span, String),
    secondary: Vec<(Span, String)>,
    help: Vec<String>,
}
```

Phase error enums stay as *constructors* of `Diagnostic`, not as the
transport. `at {position}` disappears from 107 strings because rendering
owns it.

**Why this is worth more than it looks.** It is the only move here that
improves the language *for its users*, not just for you. Aspect's
diagnostics currently cannot point at anything. Rust's ability to underline
and cross-reference is a large part of why it feels good, and it is
entirely a data-structure decision.

**Cost.** 1200–1800 lines, but shallow — mostly mechanical rewrites of
error construction. Risk: low. Fully independent of Moves 1–3; can land
at any time, including first.

---

## Move 5 — Typed ids and interned symbols

**The problem.** Identity is `String` or bare `u32` everywhere:
`Variable(String)`, `FunctionCall { name: String }`, `FieldAccess { field:
String }`, `struct_id: u32`, `sum_id: u32`, `enum_id: u32`, `variant: u32`.
Nothing stops a `sum_id` being passed where a `struct_id` belongs — the
registry unifies the id space, so it will even silently resolve.

Worse, methods are mangled to the **string** `"Type$method"` and re-parsed
downstream. Structure encoded in a string is the most C thing in the tree.

**The move.** Newtypes (`StructId`, `SumId`, `EnumId`, `FnId`, `SymbolId`)
— zero runtime cost, and mixing them becomes a compile error. A `SymbolId`
interner replaces `String` keys in scopes and AST nodes. Method identity
becomes `(StructId, SymbolId)` instead of a formatted string.

**Cost.** 400–700 lines. Risk: low. The compiler finds every site for you.
This is the cheapest real win in the document and it pairs naturally with
Move 3.

---

## Sequencing

Ordered by (payback ÷ risk), respecting dependencies:

1. **Move 5** (typed ids). Cheap, mechanical, low risk, and it makes
   Moves 1–3 safer by making id confusion impossible while you churn.
2. **Move 4** (spans + diagnostics). Independent, low risk, immediately
   felt on every error you write, and it upgrades the language for its
   users.
3. **Move 1, codegen half** (fold over `const_eval` + runtime codegen).
   Self-contained, deletes ~300 lines, proves the fold design before
   betting the checker on it.
4. **Move 3** (interned types) — *prototype first on `codegen/types.rs`*.
   Decide on evidence, not on this document.
5. **Move 2** (arena AST). Largest structural change; do it once Move 3
   has settled what a type is.
6. **Move 1, checker half** — only if the context-carrying fold survives
   contact with bidirectional checking.

Total, if all of it lands: roughly 5,000–7,000 lines rewritten across a
20,500-line tree. That is a quarter to a third of the compiler. It is
also, deliberately, *never* a rewrite — every step leaves the 135-program
corpus green, which is the only reason a change of this size is
survivable by one person.

---

## Considered and rejected

- **Salsa / query-based incremental compilation.** Solves a problem this
  compiler does not have. `cargo check` is 1.1 s and compilation is
  whole-program by design. Enormous conceptual overhead for nothing.
- **Replacing `#[parse_rule]` with a parser combinator library.** The DSL
  (290 lines in `aspect-macros/src/expand.rs`, 16 macros) is one of the
  better-leveraged pieces of Rust in the tree and is not a pain source.
  Extend it; do not replace it.
- **Splitting the crate into per-phase crates.** Would lengthen the 1.1 s
  loop and buy nothing at 20k lines.
- **Deleting features to shrink the codebase.** The tree is small. Size
  is not the problem, and the measurements say so.

---

## Open questions

1. Does the context-carrying fold (Move 1, second option) actually model
   bidirectional checking, or does `check`/`synth` need to stay
   hand-written? Settle with a spike on three representative variants
   before committing.
2. Does `Const(TypeId)` as a wrapper variant work, or does constness want
   to stay a bit on the id? A wrapper doubles interned entries for every
   type used both ways.
3. Move 2 and Move 3 both want to land "first" — arena ids make type
   interning easier to thread, interned types make arena nodes smaller.
   Which order costs less?
4. Do spans want byte offsets into the original source, given that the
   preprocessor **inlines** imported token streams? A span must resolve
   back to the right file *and* offset after inlining.
