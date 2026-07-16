# Proposal: A Three-Hook Metasystem for User-Extensible Compilation

**Status:** Draft 0.2 — post-scrutiny plan proposal. §§1–11 are the original design; §§12–14 add the execution model, scrutiny-driven amendments, and a concrete phased implementation plan. Worked example: [Three-Hook-Metasystem-Example.md](Three-Hook-Metasystem-Example.md).
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

1. **Attributes.** `@identifier` or `@identifier(args)`, legal in fixed positions. The parser attaches them to nodes as **inert metadata** — it never interprets them. Attributes are cargo, not keywords. Meaning is assigned later, or never. *Amended (§13):* the fixed positions include not only items (before functions, types, fields) but **statements** (`@debug x = f()`), because site-level decoration is a primary use case. Expression-position attributes are a later extension. Today there is **no `@` token at all** — this is greenfield lexer + AST work (see §14).

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
3. Handlers must be **deterministic in their output** — the AST/judgment they produce is a function of program facts alone. *Amended (§13):* they **may** carry intra-compilation state (a handler is a function inside the hook, so state is natural and useful), provided obligations fire in a **total, source-determined order**. Purity of *output*, not statelessness, is the invariant. Cross-*compilation* persistence is out of scope. I/O is still forbidden (asserted, not proven — see §9).

The two transform triggers — **repair** (a stuck type judgment, e.g. a coercion) and **decoration** (an attribute presence, e.g. `@debug`) — share one worklist. Repairs are seeded *lazily* on check failure and become errors at quiescence; decorations are seeded *eagerly* from every attribute site and **always fire, never diagnose**. Handled attributes are consumed so they cannot re-fire. See §13.

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

Rules never touch raw AST. They program against a query layer: `call_sites_of(fn)`, `instantiations_of(type)`, `implementors_of(trait)`, `has_attr(node, "@trusted")`, `reachable_from(fn)`. This keeps compiler internals rewritable forever.

**Two tiers, and the distinction is load-bearing (amended after scrutiny).** *Tier-1* queries — `call_sites_of`, `instantiations_of`, `has_attr`, `module_of` — really are dictionaries the compiler already builds; rules over them are ~10 lines and can gate compilation. *Tier-2* queries — `reachable_from`, `escapes`, `destroyed_on_all_paths`, dominance — are **flow-sensitive analyses the compiler does not build today** and must implement. Tier-2 rules are best-effort linters: they **emit warnings, not errors**, and must document their false-positive/negative envelope. The original "most of the layer wraps existing dictionaries" claim holds for Tier-1 only; do not promise Tier-2 as free. (The `must_destroy` leak checker in the worked example is Tier-2 — see [Three-Hook-Metasystem-Example.md](Three-Hook-Metasystem-Example.md).)

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

---

## 12. Execution Model — how metaprograms run inside the compiler

This section is the answer to the question the earlier drafts skipped: *all three hooks are user Aspect code that must execute inside the compiler.* Scrutiny found this is the real work — and that most of it is already de-risked.

**JIT is a solved primitive.** The compiler is Rust over **inkwell 0.9 / LLVM 19.1** (`Cargo.toml`). `CodeGenerator::generate(program) -> Module` (`src/codegen/generator.rs`) builds an LLVM module, and `jit_execute` / `jit_execute_main` already JIT-run Aspect — that is exactly what the `interpret` subcommand does (`src/main.rs`). There is **no AST interpreter** to extend; metaprograms compile to LLVM and run via `ExecutionEngine`, reusing the existing path.

**The marshalling ABI is the actual gap.** JIT'd Aspect must read and build the compiler's AST without touching raw Rust memory. Design:

- AST/program nodes are **opaque handles** (`u0*` or an integer id) into a compiler-side arena. The special structs (`Ast`, `Expr`, `Stmt`, `Fn`, `TokenTree`, `Program`, `Judgments`, and concrete list types) are thin handle-wrappers.
- `quote` / `Ast.*` constructors / every query function are `extern` **builtins implemented in Rust**, operating on handles. A metaprogram imports `std/meta`, whose signatures are `extern fn`s the compiler resolves to native implementations at JIT time.
- Consequence: the "query API is the real product" (§7) is literally this builtin surface. Its stability is what keeps compiler internals rewritable.

**Staged compilation driver.** Because expansions run during *parse* of their users, and all hooks are Aspect compiled ahead of use, the driver must: (1) resolve modules (via the Preprocessor-Infrastructure module system), (2) topologically order them so hook-*providing* modules precede hook-*using* ones, (3) parse → typecheck → codegen → JIT each hook module into an in-process **handler registry**, (4) compile user modules against that registry. This co-designs with `$module`/`$import`.

**Hygiene & safety (v1 honesty).** `quote` must **gensym** the identifiers it introduces or spliced user code is captured. A handler can still hang or crash the compiler (bounded fixpoint rounds cap the *loop*, not a single wild handler) and can call libc if it imports it, so "pure" is asserted, not enforced (§9). Accept for v1; a per-handler watchdog and an import allowlist are later work.

---

## 13. Design Refinements (post-scrutiny) — amendments to earlier sections

Consolidated from a heavy design review; each amends the section named.

- **Amends §3 — attributes below item level.** Attributes attach to **statements**, not only items, so `@debug x = f()` is legal. No `@` token exists today → greenfield lexer + `Attribute { name, args }` + an `attrs` field on `Stmt`/item nodes. Stacking order (`@a @b x`) is **decided outside-in** (leftmost applied last): `@a @b f` ≡ `a(b(f))`.
- **Amends §6 — deterministic, not stateless.** Handlers may carry intra-compilation state; the invariant is deterministic *output* plus a total, source-determined firing order. Repair vs decoration obligations share one worklist (seeded lazily vs eagerly); handled attributes are consumed.
- **Amends §6/§8 — coercion transforms are governed.** A user-defined `From -> To` coercion is an implicit conversion — the C++/Scala footgun, made worse by the flat module namespace (an import could change your coercions). Guardrails: a coercion fires **only after built-in coercion fails**, and **only where a rule opts the module in** (`allow coercion String -> u8*`). This folds coercion into the governance thesis (§8) instead of fighting it. *If in doubt, coercion transforms are the one sub-feature worth deferring past v1.*
- **Amends §7 — query API is two-tiered.** Tier-1 (dictionaries, gate-worthy) vs Tier-2 (flow-sensitive analyses the compiler must build, warning-only). Do not ship Tier-2 rules as hard errors.
- **New core-language dependency — value-blocks.** A `{ … }` in expression position whose `return` yields the block value is the primitive every wrapping transform stands on (it lets a decoration wrap a whole body without rewriting its control flow). It is a *language* feature, not a metasystem one, and needs its own short design note. Semantics to pin: disambiguation from list-init `{a, b}`, type = join of all `return`s, all-paths-return with a **void exception**, `return` binds to nearest block, `break`/`continue` pass through to enclosing loops.
- **Metalanguage ergonomics are a real prerequisite.** Aspect today has no `for-in`, generics, closures, or `match` (verified). Metaprograms are therefore verbose (index loops over concrete list structs), which makes **`quote` mandatory, not sugar**. `for-in` as a general language feature would help broadly; it is not a blocker.

---

## 14. Concrete Implementation Plan

Phased, checkbox form. Each phase is independently useful; earlier phases de-risk later ones. Effort tags from a parser/codegen survey: (S)mall / (M)edium / (L)arge.

### Phase 0 — Prerequisites (language + infra)

- [x] **Module system** — finish Preprocessor-Infrastructure (`$module`/`$import`, `-I`). Two-pass prototype registration already landed. (M)
- [x] **Value-blocks** — *landed 2026-07-14.* `ExprKind::ValueBlock`; parser disambiguates vs list-init by speculative list parse with rollback (`parse_brace_expression`); checker: target-directed / first-return-synthesized typing + conservative all-paths-return (loops never count; bare `return` rejected — no void exception, a value block always yields a value); codegen: entry-block result slot + `vblock.exit`, `return` rerouted via `value_block_stack`. Spec: doc/09 §Value blocks. Regression: `tests/programs/value_block.ap`.
- [ ] **Attributes** — `@` lexer token (`src/lexer/scanner.rs`, `tokens.rs`); `Attribute { name, args }`; `attrs` on `Stmt`/items; consume `@ident(args?)` before statement dispatch (`src/parser/statements.rs`); outside-in stacking. (M)
- [ ] **Metaprogramming std (`std/meta`)** — opaque-handle special structs + `extern` builtins (`Ast`/`Expr`/`Stmt`/`Fn`/`TokenTree`/`Program`/`Judgments` + `*List` with `.count()/.at()`); compiler-side arena + handle registry. (L)
- [ ] **`quote` / `$(…)`** — parse contract, desugar to `Ast.*` builders, **hygienic gensym**. (L)

### Phase 1 — Obligation solver (parent §10.2), no user handlers

- [ ] Refactor the checker: at `assert_coercible` (`src/typechecker/checker.rs`) and the unresolved sites (`UndefinedFunction`, `UnknownField`), **file `(kind, subject)` obligations** instead of emitting immediately. (M–L)
- [ ] **Poison/`Unresolved` sentinel type** to suppress cascade errors downstream of an unresolved demand (today `VOID` generates spurious secondaries). (M)
- [ ] Deterministic obligation ordering + bounded-rounds fixpoint scaffold; undischarged obligations → errors at quiescence. **No handlers yet.** (M)
- [ ] Regression: existing error suite unchanged (behaviourally a no-op today).

### Phase 2 — Rules (parent §10.3)

- [ ] **Staged driver + JIT registry** — reuse `CodeGenerator::generate` + `jit_execute`; compile hook modules ahead of users. (M)
- [ ] **Tier-1 query API** (`call_sites_of`, `instantiations_of`, `has_attr`, `module_of`). (M)
- [ ] `rule <anchor> <fn>` + block form; anchor enum (`type | attribute`, extensible). (S–M)
- [ ] **Hygiene rule** — every attribute must be claimed by a rule/expansion; did-you-mean on unclaimed. (S)
- [ ] **De-risk:** implement the first rule (`singleton`) as a **Rust built-in** to validate the query API + governance value *before* committing to Aspect-authored + JIT'd rules. Then port it to Aspect. Ship the trust/audit trio as proof. (M)

### Phase 3 — Expansions (parent §10.4)

- [ ] Token-tree capture at `ident { … }`; parse contracts (`raw-tokens` / `expr` / `item`); import-before-use staging. (M–L)
- [ ] Ship `derive_eq` as proof; then `interp` (bundled in `std/fmt` with its opt-in coercion transform). (M)

### Phase 4 — Transforms (parent §10.5)

- [ ] Shared worklist: **decoration** obligations seeded eagerly from attr sites (always fire, consume the attr), **repair** obligations (coercion/synthesis) seeded lazily; re-check per round, bounded. (L)
- [ ] `transform From -> To` (coercion, governance-gated) and `transform @attr(NodeKind) -> NodeKind` (decoration). (M)
- [ ] Ship `@debug(stmt)` (decoration) and vtable synthesis (repair/synthesis) as the two proofs. (M–L)

### Phase 5 — Tier-2 query API + honest linters

- [ ] Build flow-sensitive analyses (reachability, escape, dominance) behind Tier-2 queries. (L)
- [ ] `must_destroy` as a **warning** linter with documented blind spots; the transitive-property trio (`@nopanic` walker / `@trusted` / auditor). (M)

### Cross-cutting, do-not-skip

- [ ] **Diagnostics API** (parent §11) — hooks register errors with spans into user source; design before Phase 4 or template-style error soup sets in.
- [ ] **Source mapping** (parent §11) — what a query returns for transform-generated code (demand site vs generated site).
- [ ] **Safety** — per-handler watchdog + libc import allowlist (post-v1, but track it).