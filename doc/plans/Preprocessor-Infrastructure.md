# Preprocessor Infrastructure Plan

**Status:** Draft 2 — directive set and module story agreed; open questions at the end.
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

### 3. `$module` is authoritative; paths only locate files

If `search/std/math/vector.tjlb` declares `$module std/math`, the compiler
trusts the declaration — but if a file loaded for import `std/math`
declares anything *else*, that's a hard error naming both. Keeps the
declared identity and the on-disk location from drifting apart silently.

---

## Directive reference

All directives are **line-anchored**: `$` must be the first token on the
line (leading whitespace fine). Everything until the newline belongs to the
directive. Directives are only meaningful at the top level of a file;
inside a block, a line-leading `$` is an error (simplifies reasoning, can
be relaxed later). This tightens the shipped behaviour, where `$include`
was recognised anywhere in the stream.

### Defines

```tjlb
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
  import-order surprises early; relax later if it stings.
- Defines are global once made: a define is visible in every file
  processed after it, including imported ones. (Consequence of flat
  token-stream processing; revisit with real module isolation.)

**Compiler-provided defines** (the actual point of all this):

| Define | When |
|---|---|
| `OS_LINUX` / `OS_WINDOWS` / `OS_MACOS` | target OS |
| `ARCH_X86_64` / `ARCH_AARCH64` | target arch |
| `TJLB_VERSION_MAJOR` / `_MINOR` | compiler version, integer tokens |

**CLI:** `-D NAME` and `-D NAME=VALUE` (repeatable) inject defines before
the entry file is processed — the standard build-system hook.

### Conditionals

```tjlb
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

- `$ifdef NAME` — true iff NAME is defined. (`$if !defined(NAME)` covers
  ifndef; a dedicated `$ifndef` is cut for surface area — see open
  questions.)
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

```tjlb
# in lib/std/math/basic.tjlb:
$module std/math

fn gcd_u64(u64 a, u64 b) -> u64 { ... }
```

```tjlb
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
  files that declare it. (`std/math` can be `basic.tjlb` + `trig.tjlb`.)
- A file with no `$module` belongs to the anonymous root module (fine for
  entry points and one-off scripts).
- Nesting is purely hierarchical naming: importing `std` does NOT import
  `std/math`. Every import is exact.
- The compiler records `file_id -> module path` in the program — this
  becomes queryable by metasystem rules later (`module_of(fn)`), and is
  the identity the expansion import constraint will hang off.

**`$import <path>`** — makes the module part of the compilation.

Resolution, for each search root `R` in order:

1. `R/<path>.tjlb` is a file → the module is that single file.
2. `R/<path>/` is a directory → the module is every `*.tjlb` **directly**
   inside it (no recursion — subdirectories are submodules, imported
   explicitly). Files are loaded in sorted (deterministic) order.
3. Neither in any root → error listing every candidate path tried.

Search roots, in order: the entry file's directory, then each `-I`/
`--module-path <dir>` in CLI order. (A `TJLB_PATH` env var can join later.)

Semantics:

- Loaded files are lexed and preprocessed recursively (they may
  themselves `$import`).
- **Import-once by module identity**: importing `std/math` twice —
  directly or diamond-shaped — loads it once. File-level canonical-path
  dedup stays as a second guard (two module paths must not silently load
  one file twice).
- Every loaded file's `$module` declaration must equal the imported path
  (see "authoritative" above).
- Cycles are permitted and terminate via import-once; with two-pass
  prototype registration they are also *semantically* harmless.
- v1 is include-like: all symbols land in the flat global namespace.
  Name it plainly in the docs so nobody expects `math.gcd` yet.

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

- `PreprocessedSource` grows: `pub modules: Vec<(u16 /*file_id*/, String /*module path*/)>`
  plus the search-root list used (for error reporting).
- Driver keeps a define table (`HashMap<String, Vec<Token>>`), a module
  registry (`HashMap<String, ModuleStatus>` for import-once + cycle
  state), and the conditional stack.
- Dispatch: at each line start, if the token is `Dollar` and the next is
  an identifier in the directive table → handle; unknown directive name →
  error with a did-you-mean; `Dollar` mid-line → error ("directives must
  start a line").
- `main.rs`: add `-D` / `-I` flags (clap, repeatable) to `compile`,
  `interpret`, `parse`, `lex`; thread them into the preprocessor. Keep the
  `preprocess`-style debugging story via `tjlb-parser lex` (tokens are the
  preprocessor's output — a separate `-E` equivalent is `lex` now).

## Killing `$include`

1. Land defines + conditionals + modules.
2. Port `demos/std/**` to `$module std/...` declarations; move it to a
   real `lib/std/` tree; demos say `$import std/io` and the demo runner
   grows `-I lib`. `tests/programs/stdlib_check.tjlb` imports the same way
   (its `../../demos/std` relative includes disappear — that coupling was
   always a smell).
3. Port the `include_*.tjlb` test programs to import tests.
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
  import-once (diamond), cycle termination, `$module` mismatch error,
  module-not-found error listing candidates

Integration (`tests/programs/`):
- `-D`-injected define selects a branch that returns the expected code
- two-module import (file form + directory form) end-to-end
- diamond import compiles without duplicate-symbol errors
- platform define smoke test (`$ifdef OS_LINUX` on the Linux CI)

## Implementation order

1. **Two-pass prototype registration** (separate TODO, prerequisite —
   makes import order non-semantic).
2. Driver: line-anchored dispatch + directive table (reuses `Dollar`).
3. `defines.rs` + `-D` + compiler-provided platform defines.
4. `conditional.rs` (`$ifdef`/`$else`/`$endif` first, then the `$if`
   evaluator + `$elseif*`).
5. `modules.rs` + `-I` (resolution, registry, import-once).
6. Stdlib/demos/test migration; delete `$include`.
7. Docs: rewrite `doc/09` §Preprocessor, new `doc/10-modules.md`,
   update `doc/00-overview.md` pipeline diagram.

## Open questions

- **`$ifndef`**: cut in favour of `$if !defined(X)`. Cheap to add if the
  long form gets annoying — decide after a month of use.
- **Define scoping across modules**: today a define leaks into everything
  processed later. Should an imported module see the importer's defines
  (C-style, current plan) or start clean (module-hygienic)? Clean-start
  is more principled but kills the `-D DEBUG` use case unless CLI defines
  are exempted. Deferred until it bites.
- **Directory-form determinism**: sorted filename order is deterministic
  but arbitrary; with two-pass registration it stops mattering. If a
  module ever needs intra-module ordering beyond that, that's a smell
  worth an error, not a feature.
- **Metasystem attribute syntax**: `@` is now wholly reserved for
  attributes/transforms — no sharing, no reserved directive names. When
  hook #1 (expansions) lands, its `identifier { ... }` capture sites and
  `@attribute` markers coexist with `$` directives without interaction;
  the only rule worth writing down is that the preprocessor never
  interprets `@` and the parser never interprets `$`.
