# Proposal: A Three-Hook Metasystem for User-Extensible Compilation

**Status:** Draft 0.1 — design consolidation from exploratory discussion
**Scope:** Core compilation architecture. Surface syntax, module system details, and standard library are out of scope except where they constrain the design.

---

## 1. Thesis

The language is built around one stance: **the programmer states intent about structure and constraint, and the compiler enforces and exploits it.**

Extensibility and restriction are two faces of one mechanism. The language can change itself (expansions, transforms), and it can bind its own changing (rules). Restriction always outranks generation — enforced by phase order, not policy.

The core language is deliberately minimal: functions, structs, references, and the hook machinery. Everything else — traits, polymorphism, effect checking, foreign imports — is a library.

---

## 2. Pipeline Overview

```
parse                    closed grammar, token-tree fences
  └─ EXPANSIONS          user hook #1: pre-parse generation
       └─ elaboration    demand-driven typecheck (obligation solver)
            └─ TRANSFORMS  user hook #2: obligation handlers, AST rewriting
                 └─ RULES   user hook #3: post-typecheck judgment
                      └─ codegen (LLVM)
```

Three user-programmable hooks around a fixed core. Each hook lives exactly where its inputs exist:

| Hook | Phase | Sees | Produces | Cannot |
|------|-------|------|----------|--------|
| **Expansion** | pre-parse | raw token trees | new syntax → AST | see types, names, other items |
| **Transform** | during elaboration | typed AST, program facts | AST rewrites (re-checked) | create new surface syntax |
| **Rule** | post-typecheck | fully typed program, query API | judgments (errors, warnings, reports) | modify anything |

---

## 3. The Parser: Closed, With Fences

The grammar is fixed and boring, with two open productions:

1. **Attributes.** `@identifier` or `@identifier(args)`, legal in fixed positions (before functions, types, fields). The parser attaches them to nodes as **inert metadata** — it never interprets them. Attributes are cargo, not keywords. Meaning is assigned later, or never.

2. **Token-tree capture.** `identifier { ... }` where the identifier names an imported expansion: the parser counts delimiters, banks everything inside as a raw token tree, and attaches it to an expansion node. It does not attempt to understand the interior.

Consequences:

- A broken DSL inside braces cannot derail parsing of the rest of the file.
- Dumb tooling (brace matching, indexing) keeps working on any file.
- Every extension point is **syntactically visible**. You can always see where the host language ends. This is load-bearing: governance rules can only forbid what they can locate.

Deliberately rejected: reader macros / fully mutable grammar (Racket-style). Seamless syntax mutation would make the language's own restriction machinery unable to find what it governs.

---

## 4. Expansions (Hook #1 — Pre-Parse)

**Signature concept:** token tree in, AST out.

- Invoked by identifier at a capture site: `sql { SELECT ... }`, `derive_eq(Point)`.
- Superpower: arbitrary interior syntax. Limitation: sees only the captured tokens — no types, no resolved names, no other items. Anything derivable from an item's own tokens belongs here; anything needing resolved facts does not.
- **Parse contract:** an expansion declares whether its interior is `raw-tokens` or a host-language shape (`expr`, `item`, ...). Contracted shapes arrive pre-parsed as AST, so simple expansions never hand-parse tokens. (Direct fix for Rust's biggest proc-macro ergonomic tax.)
- **Locality constraint:** expansions must be *imported*, never defined in the file that invokes them. They run before that file typechecks, so they are compiled ahead of it, as separate modules the compiler loads. This constraint shapes the module system and must hold from v1.

---

## 5. Elaboration: Demand-Driven Typechecking

The typechecker is not a tree-walk; it is a **worklist solver**. This is the hardest single component in the design, and the piece that makes transforms sound.

- When checking hits something unresolvable — unknown method, missing impl, unbound name — it does not error. It files an **obligation**: a keyed record of the form *(kind, subject)*, e.g. `(missing-method, Point.foo)`.
- Checking continues elsewhere. Obligations sit on the worklist awaiting a handler.
- An error becomes real only at **quiescence**: no handler can make progress and obligations remain. The undischarged obligation *is* the error message, pointing at the original demand site — not at post-hoc rewrite wreckage.

Prior art (internal, never user-exposed): rustc's trait solver, Lean/Agda metavariable elaboration, Scala 3 typed macros, C++ on-demand template instantiation. Exposing the obligation queue as a user hook is this language's delta.

---

## 6. Transforms (Hook #2 — Obligation Handlers)

**Signature concept:** obligation + typed program facts in, AST rewrite out.

- A transform registers for an obligation key: "I answer `missing-method` obligations for types marked `@dynamic`."
- When it fires, its output AST is fed back into elaboration — which may discharge obligations or create new ones. Generation and checking interleave until quiescence.
- Transforms are the home for everything that needs resolved information: vtable synthesis from the set of implementors, `@logged` call-site wrapping, C-declaration import driven by actual usage, derived impls that depend on field *types* rather than field tokens.

### Determinism discipline (non-negotiable)

Interleaving makes check order observable. To keep compilation deterministic without proving confluence:

1. Obligations are keyed *(kind, subject)*.
2. **At most one registered handler per key.** Two handlers claiming the same key is a compile error.
3. Handlers must be **pure functions of program facts** — no I/O, no hidden state, no dependence on firing order.

### Termination

Rewrite → re-check → rules is a fixpoint loop. v1 policy: **bounded rounds** (compiler flag, sane default). A transform that hasn't quiesced within the bound is an error naming the oscillating obligation. Provably-shrinking rewrites can relax this later; do not block on it.

---

## 7. Rules (Hook #3 — Post-Typecheck Judgment)

**Signature concept:** typed program in, judgments out. Rules modify nothing.

- Registration: `rule <anchor> <checker_fn>` where the anchor is a **type name** or an **attribute identifier**:
  - `rule singleton_t ensure_is_singleton`
  - `rule @nopanic ensure_graph_nopanic`
- Anchor kinds are an enum from day one (`type | attribute`, extensible to `function | module`) so new anchors never break existing rules.
- Rules run **last**, over everything — including expansion output, transform output, and rule code itself (rules apply to rules; self-failing rules need cycle detection, which is cheap). No generation step can smuggle in code the ruleset forbids. This is the constitution clause, enforced by phase order.
- Rules are written in the language, in the files they govern, JIT'd to LLVM after typecheck and executed against the program. Unlike expansions, they need no import constraint — by their phase, everything is compiled.
- **Checker-only rules** are first-class: rules that fail nothing and emit reports (data mining, audits).

### The query API is the real product

Rules never touch raw AST. They program against a query layer: `call_sites_of(fn)`, `instantiations_of(type)`, `implementors_of(trait)`, `has_attr(node, "@trusted")`, `reachable_from(fn)`. This keeps compiler internals rewritable forever and makes typical rules ~10 lines. Most of the layer wraps dictionaries the compiler already builds.

### The transitive-property idiom

Transitive rules (effect-like properties: `@nopanic`, `@noalloc`) get a standard trio, documented as *the* idiom in the standard rule library:

1. **The walker** — graph reachability check with a conservative default at opaque boundaries.
2. **The trust attribute** — scoped exemption: `@trusted(nopanic)`, argument names the claim. Trusted-for-nopanic is not trusted-for-noalloc.
3. **The auditor** — checker-only rule listing every trust assertion: "show me everywhere we vouched instead of verified," as a one-liner.

### Hygiene rule: no silent attributes

After registration, every attribute occurring in the program must be **claimed** by at least one rule or expansion. Unclaimed attributes are an error naming the nearest match (`@nopanik` → "did you mean @nopanic?"). Escape hatch for deliberate inert markers is itself a rule/config. The system lints its own extension mechanism.

---

## 8. Governance: The Anti-Fragmentation Story

Infinite malleability historically fragments ecosystems (the Lisp curse). Here, the immune system is built from the same cells as the mutability:

- Extension points are syntactically visible (fences, attributes) → rules can find them.
- Rules can therefore bind them: `no expansions outside dsl/`, `transforms must be registered in MANIFEST`, `this module accepts no rewrites`.
- A project's ruleset is its **dialect constitution**: machine-enforced, versioned with the code.

The language does not solve feature composition in general. It gives every codebase the vocabulary to declare its dialect and enforce it. That is the defensible position.

---

## 9. What This Deliberately Is Not (v1)

- **Not an open typechecker.** The core judgment (functions, structs, references) is fixed. Transforms feed it; rules run after it; nobody redefines it. Two generation hooks and one judgment hook around a trustworthy core is a language; an open core is a compiler construction kit, and those don't get finished. Revisit only after months of real use show the query API is insufficient.
- **Not separate compilation.** Whole-program compilation in v1. Transitive rules and transform obligation resolution both want the whole program; a cross-unit rule-state story is future work. Do not promise separate compilation until it exists.
- **Not perfect C source interop.** Foreign import is a *library*: a transform/expansion pair that ingests C *declarations* (via libclang), emits typed signatures, and links against a real C compiler's output. Never translate C bodies (Zig's `@cImport` retreat is the cautionary tale).
- **Not crash-proof metaprogramming.** Purity of handlers is asserted and spot-checked, not proven, in v1.

---

## 10. Build Order

1. **Parser** — closed grammar, attributes as inert metadata, token-tree fences.
2. **Core checker as obligation solver** — hardest component; single-threaded, whole-program. Obligations + quiescence errors, *no user handlers yet*.
3. **Rules** — registration syntax, JIT of rule functions, minimal query API (call sites, instantiations, attribute lookup). Ship the singleton rule and the trio idiom as proof.
4. **Expansions** — token-tree capture, parse contracts, import-before-use constraint. Ship `derive_eq` as proof.
5. **Transforms** — handler registration, keyed obligations, bounded fixpoint. Ship trait-style vtable synthesis as proof.
6. **Standard rule library** — hygiene rule, audit idiom, governance examples.

Each stage is independently usable; each proof feature exercises exactly one hook.

---

## 11. Open Questions

- Expansion ordering when stacked on one item (`@logged @traced fn`) — pick inside-out or outside-in *now*, arbitrarily, and document it.
- Obligation key granularity: is *(kind, subject)* fine-grained enough to keep the one-handler rule livable, or does it need scoping (per-module handler priority)?
- Rule state across compiler invocations (incremental builds) — deferred with separate compilation, but the durable-rule-state design should be sketched before the module system freezes.
- What the query API returns for code produced by transforms: original demand site, generated site, or both (source mapping policy).
- Diagnostics API for expansions/transforms: they must be able to register errors in their own vocabulary with spans into user-written source. Deserves first-class design before hook #2 ships, or error quality dies the C++ template death.