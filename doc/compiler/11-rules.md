# Rules (`src/meta/`)

Rules are the third hook of the three-hook metasystem — **post-typecheck
governance judgments**. This document covers the shipped slice: **Phase 2a**,
rules as Rust builtins (no JIT). The full design is
[`doc/plans/Three-Hook-Metasystem.md`](../plans/Three-Hook-Metasystem.md); the
`std/meta` handle ABI that Phase 2b JIT'd rules will use is
[`doc/plans/Meta-Module-JIT-Interface.md`](../plans/Meta-Module-JIT-Interface.md).

## Where it runs

`meta::run_rules(&Program)` runs **after** type checking and **before** code
generation, from both entry points:

- `build_program` (`src/main.rs`) — the `compile`/`interpret` driver.
- `parse_and_typecheck` (`tests/integration_tests.rs`) — the corpus harness, so
  every generated test sees rules.

It **modifies nothing**: it reads the fully-typed `Program` and returns
`Vec<Judgment>`. `Error` judgments fail the build; `Report` judgments are notes
(stderr, or the harness's warning channel — assertable with `# expected_warning:`).

## The declaration

`rule <anchor> <checker_fn>` parses into `Program::rules: Vec<RuleDecl>`
(`src/parser/ast.rs`). `rule` is a **soft keyword**: the top-level loop
(`do_parse_program`) detects it by lookahead (`Parser::is_rule_decl` — `rule`
followed by `@`, or by two identifiers), so a type or global literally named
`rule` still parses. A rule takes no `public`/`export`/attributes.

`RuleAnchor` is `Type(String)` or `Attribute(String)` — an enum from the start
so `function`/`module` anchors can be added later without breaking the AST.

## Execution (`src/meta/`)

- **`mod.rs`** — `run_rules` validates and dispatches each `RuleDecl`. It
  resolves a type anchor via `ModuleSymbols::struct_id` (following a one-hop
  `alias`), resolves an attribute anchor to its carrier positions, looks the
  `checker_fn` up in the builtin registry, then runs it and stamps the rule
  name onto each `Judgment`. Identical declarations are de-duplicated. Unknown
  type anchors and unknown checkers become `Error` judgments (the latter with a
  Levenshtein did-you-mean). Anchor resolution is a flat, whole-program lookup
  and does **not** honor `public type` — governance sees all (§8 of the design).
- **`query.rs`** — `QueryIndex::build(&Program)` walks the typed AST once to
  build Tier-1 dictionaries. Phase 2a needs two: `instantiations_of(struct_id)`
  (construction sites — `StructLiteral` and value `alloc` of the type) and
  `attr_carriers(name)`. The layer is designed to grow.
- **`builtins.rs`** — the registry. Two builtins ship:
  - `singleton` (type anchor, errors): the type may be *constructed* at most
    once; each construction past the first is an error. "Construction" is a
    struct literal or `alloc`; **not** counted (documented v1 blind spots):
    value copies, uninitialized declarations, arrays, by-value parameters,
    struct-returning calls, and embedded struct-typed fields.
  - `audit` (any anchor, reports): a checker-only rule that lists every site of
    its anchor — proof that attribute-anchor resolution and the report channel
    work.

A builtin has type `RuleFn = fn(&QueryIndex, &ResolvedAnchor, Position) ->
Vec<RawJudgment>`; the whole-program `QueryIndex` is passed in (not just the
anchor) so the same builtins port unchanged to the Phase 2b JIT'd
`fn(Program) -> Judgments` shape.

## Diagnostic format

`meta::format_judgment` renders `file:line:col: rule <name>: <msg>`, resolving
the file via `pos.file_id` — mirroring `TypeChecker::format_error`.

## Explicitly deferred (Phase 2b and later)

JIT'd Aspect-authored rule bodies, the `std/meta` handle ABI, the hygiene rule
(every attribute claimed by some rule), and the Tier-2 flow-sensitive query
layer (reachability/escape/dominance — warning-only linters).
