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
`rule` still parses. A rule takes no `export`/attributes, but **may take
`public`**, which governs its *reach* (`RuleDecl::vis`): a bare rule judges only
its declaring module; `public rule` judges the whole program, mirroring `public
type`.

`RuleAnchor` is `Type(String)` or `Attribute(String)` — an enum from the start
so `function`/`module` anchors can be added later without breaking the AST.

## Execution (`src/meta/`)

- **`mod.rs`** — `run_rules` validates and dispatches each `RuleDecl`. It
  resolves a type anchor via `ModuleSymbols::struct_id` (following a one-hop
  `alias`), resolves an attribute anchor to its carrier positions, looks the
  `checker_fn` up in the builtin registry, then runs it and stamps the rule
  name onto each `Judgment`. Identical declarations are de-duplicated. Unknown
  type anchors and unknown checkers become `Error` judgments (the latter with a
  Levenshtein did-you-mean). Anchor *resolution* is a flat, whole-program lookup
  and does **not** honor `public type` (a rule can name any type by name). What
  the rule *judges* is then scoped by the rule's own visibility: a private rule's
  query results are restricted to its declaring module (`QueryIndex::in_module`,
  keyed off `RuleDecl::pos`'s `file_id → file_modules`); a `public` rule sees the
  whole program. Both the builtin path (`RuleFn` takes the module filter) and the
  JIT path (`build_ctx` filters its site snapshots) honor it.
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

## Explicitly deferred (beyond the 2b first slice)

The hygiene rule (every attribute claimed by some rule), the Tier-2
flow-sensitive query layer (reachability/escape/dominance — warning-only
linters), the full ~40-builtin read surface (only the singleton slice is
implemented), attribute-anchored rule functions, and the write surface
(`quote` / construction) for expansions and transforms.

## Phase 2b — JIT'd Aspect rule functions (`src/meta/jit.rs`)

A rule checker can now be written **in Aspect** as a `rule fn` and is JIT-compiled
and executed over the program via the `std/meta` opaque-handle ABI. See
`doc/plans/Meta-Module-JIT-Interface.md` §10 for the resolved design.

### The `rule fn` descriptor

`rule fn <name>(...) -> ... { ... }` — a soft keyword (`rule` before `fn`, so it
never clashes with a type named `rule`). A metaprogramming function has three
properties, keyed on `FunctionProto.meta_kind`: `std/meta` is in scope in its
body; it may not be called from ordinary code (the **gate**); and it is
codegen'd only into the JIT-only judge module. `rule <T> <name>` resolves
`<name>` to a builtin first, then a `rule fn` checker `(Program, Type) ->
Judgments`, else an error.

### Injection + gate

`std/meta`'s types + `extern fn meta_*` are injected (never `$import`-ed) into
any compile that declares a `rule fn` — a cheap `rule`+`fn` token scan gates it,
so ordinary programs are untouched (`src/preprocessor/mod.rs`). Injection makes
std/meta *present*; `meta::check_meta_gate` keeps it *gated*: an ordinary
function that names a std/meta type or calls a meta function is a `meta-scope`
error, run before rules so misuse is a clean diagnostic instead of a cryptic
undefined-`meta_*` codegen failure.

### The judge (`src/meta/jit.rs`)

For a `rule <T> <checker>`, `run_rule_fn` builds a **judge module** — a filtered
clone of the program (meta functions + injected std/meta only), codegen'd for
the **host** target with **no `globaldce`** so the meta set survives. The
first-slice read-surface `meta_*` builtins are implemented in Rust over a
per-invocation thread-local `MetaCtx` (an owned handle arena + query snapshot +
judgment accumulator) and bound via `add_global_mapping`; every *other* declared
`meta_*` is bound to a null stub so MCJIT can finalize the never-called wrappers.

### Calling convention (the trampoline)

`(Program, Type) -> Judgments` lowers to `void(ptr sret, ptr byval, ptr byval)`,
passing its `{u64}` structs **on the stack** — which a plain `extern "C"` call
cannot match. So the preprocessor injects, per checker, a **scalar-ABI
trampoline** `__rt_<checker>(u64, u64) -> u64` (Aspect source, reusing codegen's
byval handling) that wraps the two handles, calls the real checker, and returns
the result handle. The judge calls the trampoline via `get_function_address`.

### Artifact stays clean automatically

No artifact partition is needed: after the linkage revision `public` is
module-visibility with *internal* linkage, so the meta code — unreachable from
`main` — is stripped from the artifact by `globaldce`. Only the judge keeps it.

### Known limitations (first slice)

`Expr` handles are position-only (`QueryIndex` stores positions); attribute-
anchored rule fns are unsupported; a user `type` whose name collides with a
reserved std/meta type (e.g. `type Program`) reports a `Duplicate type` error
pointing at `meta.ap` rather than a tailored message.
