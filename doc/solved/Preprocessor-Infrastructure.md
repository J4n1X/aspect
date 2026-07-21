# Preprocessor Infrastructure Plan

**Status:** Implemented 2026-07-14 — all eight steps landed (defines,
conditionals, modules with enforced visibility, stdlib migration to
`lib/std`, `$include` removed, docs rewritten; see § Implementation order).
**Supersedes:** Draft 1 (text-level `@include`/`@define`/`@ifdef`) and the shipped `$include`.

## Overview

Extend the existing **token-level** preprocessor (`src/preprocessor/`) into the
language's conditional-compilation and module layer:

- **Defines** — `$define NAME` / `$define NAME <tokens>` / `$undefine NAME`,
  plus compiler-provided platform defines and a `-D` CLI flag. Motivation:
  platform-specific code paths (e.g. a future inline-asm `sqrt` on x86-64
  vs a Newton fallback elsewhere).
- **Conditionals** — `$ifdef` / `$if` / `$elseifdef` / `$elseif` / `$else` /
  `$endif`, nestable.
- **Modules** — `$module <path>` declares which module a file belongs to;
  `$import <path>` pulls a module into the compilation. Module paths are
  resolved against compiler search directories (`-I`). **`$include` is
  removed** — imports fully replace it.

**Sigil decision:** directives keep the `$` sigil. `@` is reserved wholesale
for the Three-Hook Metasystem (doc/plans/Three-Hook-Metasystem.md) —
attributes today (`@nopanic`, `@trusted(...)`), transform/expansion anchors
tomorrow. The two layers never share a namespace, so a line-leading
`@identifier` is unambiguously metasystem cargo and a `$identifier` is
unambiguously preprocessor. No reserved-name table, no disambiguation rules,
and the lexer already has `TokenKind::Dollar`.

The module layer is deliberately shaped for the metasystem: expansions must
be *imported, never defined in the invoking file*, which requires exactly
the module identity + location machinery built here. v1 modules are **load
units, not namespaces** — the symbol table stays flat; namespacing is
future work.

---

## Things to settle first (raised, with proposed resolutions)

### 1. Token-level, not text-level

Draft 1 described a text/line preprocessor. The shipped `src/preprocessor/`
is token-level (lex first, then transform the token stream), and it stays
that way:

- Positions carry a `file_id`; errors inside imported files already point
  at the right file (`TypeChecker::format_error`). A text-level splice
  would destroy that and need `#line`-style bookkeeping.
- Define substitution over tokens is word-boundary-safe for free and can
  never rewrite string literals.
- **Verified:** the scanner only folds `T[N]` when N is a literal;
  `u8[MAX_SIZE]` lexes as `u8` `[` `MAX_SIZE` `]`, and the parser's
  type-suffix rule (the `(i32*)[3]` machinery) accepts `type-atom [ int ]`.
  So `$define MAX_SIZE 1024` works in array types with token-level
  substitution. No text-level pass needed.

### 2. Import order vs. no-forward-references — a real footgun

The language resolves calls at parse time with **no forward references**
(only self-recursion works). Imports splice token streams in encounter
order, so whether `A` can call into `B` depends on import order, and
mutual dependencies *between modules* are impossible to express at all.

**Resolution: the two-pass prototype registration TODO is a prerequisite**
(or co-requisite) for `$import`. Pass 1 collects all function prototypes,
type-structs, and aliases across the full expanded stream; pass 2 parses
bodies. With that in place, import order stops being semantic and module
cycles (A imports B imports A) degrade gracefully via import-once instead
of producing baffling "undefined function" errors. Ship it first.

*(Landed 2026-07-14: `do_parse_program` prescans struct names and aliases,
registers every prototype in pass 1 while brace-skipping bodies, and parses
bodies in pass 2. Only global-variable initializers remain order-sensitive.
Regression: `tests/programs/forward_references.ap`.)*

### 3. `$module` is authoritative; paths only locate files

If `search/std/math/vector.ap` declares `$module std/math`, the compiler
trusts the declaration — but if a file loaded for import `std/math`
declares anything *else*, that's a hard error naming both. Keeps the
declared identity and the on-disk location from drifting apart silently.

---

## Directive reference

All directives are **line-anchored**: `$` must be the first token on the
line (leading whitespace fine). Everything until the newline belongs to the
directive. The conditional-compilation directives
(`$if`/`$ifdef`/…/`$endif`) are valid at any brace depth — the initial
"top level only" restriction was relaxed for them so conditional
compilation works inside a function body. The state-mutating directives
(`$define`/`$undefine`/`$module`/`$import`) remain top-level only; a
line-leading one inside a block is an error.

### Defines

```aspect
$define DEBUG                      # flag define (no value)
$define MAX_SIZE 1024              # value = rest-of-line token sequence
$define GREETING "hello"           # any tokens, string literals included
$undefine DEBUG                    # removes; no-op if not defined
```

- Object-like only. **No function-like macros** — parameterised code
  generation is the metasystem expansion hook's job; the preprocessor
  will not grow a second macro language.
- Substitution is by identifier token: wherever `MAX_SIZE` appears as an
  `Identifier` token, the define's token sequence is spliced in
  (substituted tokens keep the use-site position). Substitution is
  recursive but a name may expand at most once per chain (self-reference
  guard, like C).
- **Redefinition is an error** (use `$undefine` first). Catches
  import-order surprises early; relax later if it stings. `-D` definitions
  count as prior defines: a file-level `$define` of a `-D`-injected name is
  the same redefinition error. Files that want overridable defaults write
  the `$ifndef` guard (`$ifndef MAX_SIZE` / `$define MAX_SIZE 1024` /
  `$endif`).
- Defines are global once made: a define is visible in every file
  processed after it, including imported ones. (Consequence of flat
  token-stream processing; revisit with real module isolation.)

**Compiler-provided defines** (the actual point of all this):

| Define | When |
|---|---|
| `OS_LINUX` / `OS_WINDOWS` / `OS_MACOS` | target OS |
| `ARCH_X86_64` / `ARCH_AARCH64` | target arch |
| `ASPECT_VERSION_MAJOR` / `_MINOR` | compiler version, integer tokens |

**CLI:** `-D NAME` and `-D NAME=VALUE` (repeatable) inject defines before
the entry file is processed — the standard build-system hook.

### Conditionals

```aspect
$ifdef OS_LINUX
    extern fn epoll_create1(i32 flags) -> i32
$elseifdef OS_MACOS
    extern fn kqueue() -> i32
$else
    # portable fallback
$endif

$if MAX_SIZE > 4096
    const u64 BUCKETS = 64
$elseif MAX_SIZE > 512
    const u64 BUCKETS = 16
$else
    const u64 BUCKETS = 4
$endif
```

- `$ifdef NAME` / `$ifndef NAME` — true iff NAME is (not) defined.
  `$ifndef` earns its keep via the overridable-default pattern (see
  Defines). There is no `$elseifndef` — chains spell it
  `$elseif !defined(NAME)`.
- `$if EXPR` — EXPR is a **constant integer expression** over: integer
  literals, defined names (substituted first; must expand to constant
  integer expressions), `defined(NAME)` (1 or 0), the operators
  `+ - * / % << >> & | ^ ! && || == != < > <= >=`, and parentheses.
  Undefined identifiers in an `$if` are an **error**, not silently 0 —
  C's silent-zero rule is a famous bug factory.
  Implementation: a ~100-line Pratt evaluator over the already-lexed
  tokens; precedence table borrowed from the parser.
- Chain form: `$if/$ifdef` then any mix of `$elseif`/`$elseifdef`, at most
  one `$else`, closed by `$endif`. Arbitrary nesting; false branches are
  skipped with nesting tracked (inner `$if`/`$endif` pairs counted, their
  contents discarded, directives inside NOT executed).
- Everything works inside a skipped branch except unterminated blocks:
  `$endif` must still match up.

### Modules

```aspect
# in lib/std/math/basic.ap:
$module std/math

fn gcd_u64(u64 a, u64 b) -> u64 { ... }
```

```aspect
# in an application file:
$import std/math
$import std/collections

fn main(u32 argc, u8** argv) -> i32 {
    return gcd_u64(48, 36) as i32
}
```

**`$module <path>`** — declares the module this file belongs to.

- Path grammar: `segment('/'segment)*`, segments are identifiers.
  Bare tokens, no quotes — one form only.
- At most one `$module` per file, before any non-directive token.
- Multiple files may declare the same module: a module is the SET of
  files that declare it. (`std/math` can be `basic.ap` + `trig.ap`.)
- A file with no `$module` belongs to the anonymous root module (fine for
  entry points and one-off scripts).
- Nesting is purely hierarchical naming: importing `std` does NOT import
  `std/math`. Every import is exact.
- The compiler records `file_id -> module path` in the program — this
  becomes queryable by metasystem rules later (`module_of(fn)`), and is
  the identity the expansion import constraint will hang off.

**`$import <path>`** — makes the module part of the compilation.

**Resolution is convention + verification** (decided 2026-07-14). Per `-I`
root, in flag order, `$import std/math` looks for:

- **file form:** `<root>/std/math.ap`, or
- **directory form:** every `.ap` file directly inside `<root>/std/math/`
  (non-recursive).

The first root that yields either form wins; a root offering *both* forms
is an error. Every loaded file's `$module` declaration must equal the
import path — a mismatch or a missing declaration is a hard error naming
the file, its declaration, and the import that pulled it in (§ 3). There is
no tree scanning: the import path *is* the location contract, and
`$module` is the verified identity.

Semantics:
- Loaded files are lexed and preprocessed recursively (they may
  themselves `$import`).
- **Imports do not trickle down — and this is enforced** (decided
  2026-07-14). A module's imports are visible to all files of that module,
  but not to its importers. The symbol *table* stays flat (load-unit
  semantics, codegen unchanged); *resolution* is checked: the parser knows
  each file's module (`pos.file_id` → module), each module's direct
  imports, and each symbol's defining module (its `pos.file_id`), so
  resolving a function/type/global defined in a module the current module
  never imported is a compile error ("function `memcpy` lives in `std/mem`,
  which `std/math` does not import"). Same-module references are always
  visible. Real encapsulation in v1, and it encourages good source
  structure.
- **Import-once by module identity**: importing `std/math` twice —
  directly or diamond-shaped — loads it once. Another import merely makes the functions of that imported module visible to the file/module that imports it. File-level canonical-path
  dedup stays as a second guard (two module paths must not silently load
  one file twice).
- Cycles are permitted and terminate via import-once; with two-pass
  prototype registration they are also *semantically* harmless.
- v1 has no *namespacing*: symbols land in one flat global namespace (no
  `math.gcd` syntax yet), so names must be globally unique across all
  loaded modules — visibility is enforced per the trickle-down rule above,
  but two modules exporting the same name still collide. Name it plainly
  in the docs.

---

## Architecture changes (`src/preprocessor/`)

Current: `mod.rs` (driver: lex entry file, walk tokens, dispatch on
`Dollar`) + `include.rs`. Positions carry `file_id`; `PreprocessedSource`
returns tokens + file registry. The `Dollar` token and dispatch loop are
reused as-is; the driver gains line-anchoring.

Target shape:

```
src/preprocessor/
├── mod.rs          # driver: line-anchored $-dispatch, PreprocessedSource
├── defines.rs      # define table, substitution, self-reference guard
├── conditional.rs  # block collection ($if..$endif), Pratt const-evaluator
├── modules.rs      # $module/$import: resolution, module registry, dedup
└── errors.rs       # PreprocessError (grown from today's LexerError reuse)
```

- `PreprocessedSource` grows: `pub modules: Vec<(u16 /*file_id*/, String /*module path*/)>`,
  `pub imports: HashMap<String /*module*/, Vec<String /*direct import*/>>`,
  plus the search-root list used (for error reporting). The parser threads
  `modules` + `imports` into function/type/global resolution for the
  visibility check.
- Driver keeps a define table (`HashMap<String, Vec<Token>>`), a module
  registry (`HashMap<String, ModuleStatus>` for import-once + cycle
  state), and the conditional stack.
- Dispatch: at each line start, if the token is `Dollar` and the next is
  an identifier in the directive table → handle; unknown directive name →
  error with a did-you-mean; `Dollar` mid-line → error ("directives must
  start a line").
- `main.rs`: add `-D` / `-I` flags (clap, repeatable) to `compile`,
  `interpret`, `parse`, `lex`; thread them into the preprocessor. Keep the
  `preprocess`-style debugging story via `aspc lex` (tokens are the
  preprocessor's output — a separate `-E` equivalent is `lex` now).

## Killing `$include`

1. Land defines + conditionals + modules.
2. Port `demos/std/**` to `$module std/...` declarations; move it to a
   real `lib/std/` tree; demos say `$import std/io` and the demo runner
   grows `-I lib`. `tests/programs/stdlib_check.ap` imports the same way
   (its `../../demos/std` relative includes disappear — that coupling was
   always a smell).
3. Port the `include_*.ap` test programs to import tests.
4. Delete `src/preprocessor/include.rs` and the `$include` docs section.
   No deprecation period — the language is pre-1.0 and single-user;
   keeping two mechanisms is worse than one breaking change.

## Test plan

Unit (preprocessor):
- define/undefine round-trip; redefinition error; flag defines in `$ifdef`
- substitution: word boundaries (token identity), string literals
  untouched, array-size position (`u8[MAX_SIZE]`), recursive define with
  self-reference guard
- `$if` evaluator: precedence, `defined()`, undefined-identifier error,
  division by zero error
- chain handling: `$elseif`/`$elseifdef` mixes, nesting, skipped-branch
  nesting integrity, unterminated/stray-directive errors
- modules: file-form and directory-form resolution, search-root order,
  both-forms-in-one-root error, import-once (diamond), cycle termination,
  `$module` mismatch / missing-declaration errors, module-not-found error
  listing candidates

Integration (`tests/programs/`):
- `-D`-injected define selects a branch that returns the expected code
- `-D NAME` + file `$define NAME` → redefinition error
- `$ifndef` overridable-default pattern, with and without `-D` override
- two-module import (file form + directory form) end-to-end
- diamond import compiles without duplicate-symbol errors
- visibility: A imports B, B imports C → A calling into C is a compile
  error; two files of one module share that module's imports
- platform define smoke test (`$ifdef OS_LINUX` on the Linux CI)

## Implementation order

1. ~~Two-pass prototype registration~~ — **done 2026-07-14**.
2. Driver: line-anchored dispatch + directive table (reuses `Dollar`).
3. `defines.rs` + `-D` + compiler-provided platform defines.
4. `conditional.rs` (`$ifdef`/`$ifndef`/`$else`/`$endif` first, then the
   `$if` evaluator + `$elseif*`).
5. `modules.rs` + `-I` (resolution, registry, import-once, `$module`
   verification).
6. ~~Visibility enforcement~~ — **done 2026-07-14** (functions, methods,
   type-structs, aliases, globals; `ParserError::NotImported`).
7. ~~Stdlib/demos/test migration; delete `$include`~~ — **done 2026-07-14**
   (stdlib moved to `lib/std/**` with `$module` declarations, demos and
   `stdlib_check.ap` import it with `-I lib`, include tests re-expressed
   as module tests, `src/preprocessor/include.rs` and
   `LexerError::IncludeError` deleted).
8. ~~Docs~~ — **done 2026-07-14** (`doc/09` §Preprocessor rewritten,
   `doc/10-modules.md` written, `doc/00-overview.md` pipeline updated).

Steps 4 and 5 are independent of each other (both sit on step 2/3's
driver + define table) and can proceed in parallel; step 6 needs 5's
data structures.

## Resolved questions (2026-07-14)

- **Module discovery**: convention + verification — the import path maps
  to a file (`<path>.ap`) or directory (`<path>/`) under the `-I` roots,
  and every loaded file's `$module` declaration is verified against the
  import path. No declaration-based tree scanning.
- **Import visibility**: non-transitivity is *enforced* in v1 at parse-time
  resolution (functions, types, globals). Flat table, checked lookups.
- **`$ifndef`**: exists (overridable-default pattern); no `$elseifndef`.
- **`-D` collisions**: uniform redefinition error — `-D` counts as a prior
  define; overridable defaults use the `$ifndef` guard.
- **Metasystem attribute syntax**: `@` is wholly reserved for
  attributes/transforms — no sharing, no reserved directive names. When
  hook #1 (expansions) lands, its `identifier { ... }` capture sites and
  `@attribute` markers coexist with `$` directives without interaction;
  the only rule worth writing down is that the preprocessor never
  interprets `@` and the parser never interprets `$`.
