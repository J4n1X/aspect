# Modules

TJLB's module system is the language's load unit and visibility boundary.
Two directives carry it (see [09-syntax-reference.md](09-syntax-reference.md)
§ Preprocessor for the general directive rules):

- **`$module <path>`** declares which module a file belongs to.
- **`$import <path>`** makes a module part of the compilation.

Both live in the preprocessor (`src/preprocessor/modules.rs`); the
visibility rule is enforced later, at parse-time symbol resolution.

```tjlb
# in lib/std/io/print.tjlb:
$module std/io
$import std/c/stdio

fn println(u8* s) -> i32 { return puts(s) }
```

```tjlb
# in an application file:
$import std/io

fn main(u32 argc, u8** argv) -> i32 {
    return println("hello")
}
```

Compile with the search root that holds the library:

```bash
cargo run -- interpret app.tjlb -I lib
```

## Path grammar

Module paths are bare token sequences — no quotes, one form only:

```
module-path ::= segment ('/' segment)*
segment     ::= identifier
```

## `$module` — declaring identity

- At most one `$module` per file, and it must appear **before any
  non-directive content** (comments and other directives are fine above
  it; content inside a fully-skipped conditional branch doesn't count).
- Multiple files may declare the same module: a module is the **set of
  files** that declare it.
- A file with no `$module` belongs to the **anonymous root module** —
  fine for entry points and one-off scripts. Internally it is the empty
  string; diagnostics call it "the root module".
- Nesting is purely hierarchical *naming*: importing `std` does NOT
  import `std/io`. Every import is exact.

## `$import` — resolution is convention + verification

Per `-I` root, in flag order, `$import std/io` looks for:

- **file form:** `<root>/std/io.tjlb`, or
- **directory form:** every `.tjlb` file directly inside `<root>/std/io/`
  (non-recursive).

The first root that yields either form wins; a root offering *both* forms
is an error. There is no tree scanning: the import path **is** the
location contract.

`$module` declarations are **authoritative and verified**: every file
loaded for `$import std/io` must declare exactly `$module std/io`. A
mismatch — or a missing declaration — is a hard error naming the file,
its declaration, and the import that pulled it in. The path locates the
files; the declaration is the identity; the two may not drift apart.

Imported files are lexed and preprocessed recursively — they may
themselves `$import`.

## Import-once and cycles

Importing a module twice — directly or diamond-shaped (A imports B and C,
which both import D) — loads it **once**, keyed by module identity. A
repeat import merely makes the module's symbols visible to the importing
module. Canonical-path dedup remains as a second guard underneath: two
module paths can never silently load one file twice.

Cycles (A imports B imports A) are permitted and terminate via
import-once. Because the parser registers all prototypes in a first pass
before parsing bodies, import order carries no meaning and cycles are
also semantically harmless.

## Visibility: imports do not trickle down — enforced

A module sees exactly two things: **its own symbols** (all files of the
module, plus that module's imports, shared across its files) and the
symbols of modules it **directly imports**. Imports are not transitive,
and this is *enforced* at parse-time resolution for every symbol kind —
functions, methods, type-structs, aliases, and globals
(`ParserError::NotImported`).

If the entry file imports `middle` and `middle` imports `hidden`, then
`middle`'s code may use everything in `hidden`, but the entry file may
not:

```
error: function 'hidden_value' is defined in module 'hidden', which the
root module does not import
```

Want it? Import it yourself. This is real encapsulation: a module's
dependencies are its own business, and every file states what it uses.

## v1 caveat: load units, not namespaces

The symbol table stays **flat**. There is no `io.println` syntax; a
symbol's name must be **globally unique** across all loaded modules.
Visibility is enforced per the rule above, but two modules exporting the
same function name still collide with a duplicate-symbol error.
Namespacing is future work.

## The standard library (`lib/std`)

The stdlib ships in the repository under `lib/`; compile with `-I lib`.
Both resolution forms are exercised:

| Import | Form | Files | Its own imports |
|---|---|---|---|
| `std/c/stdio` | file | `lib/std/c/stdio.tjlb` | — |
| `std/c/stdlib` | file | `lib/std/c/stdlib.tjlb` | — |
| `std/c/string` | file | `lib/std/c/string.tjlb` | — |
| `std/io` | directory | `lib/std/io/print.tjlb` | `std/c/stdio` |
| `std/math` | file | `lib/std/math.tjlb` | — |
| `std/mem` | directory | `lib/std/mem/alloc.tjlb` | `std/c/stdlib` |
| `std/rand` | file | `lib/std/rand.tjlb` | — |
| `std/sort` | file | `lib/std/sort.tjlb` | `std/c/string` |
| `std/string` | directory | `lib/std/string/String.tjlb` | `std/c/stdlib`, `std/c/string` |
| `std/vec` | directory | `lib/std/vec/vec_i32.tjlb` | `std/c/stdlib` |
| `std/collections` | directory | `lib/std/collections/map_str_i64.tjlb` | `std/c/stdlib`, `std/c/string` |

Note the consequence of enforced non-transitivity: `$import std/sort`
gives you `sort_bytes` and `sort_cstr`, but **not** `strcmp` — if your
file calls `strcmp` itself, it writes `$import std/c/string` itself.

The `std/c/*` modules are raw libc `extern fn` bindings, importable at
header granularity like their C namesakes. The directory-form modules
(`std/vec`, `std/collections`, …) are the ones expected to grow one file
per concrete type until generics exist.

## Metasystem outlook

The compiler records `file_id → module path` for every file — this
identity is what the planned metasystem hangs its "expansions must be
imported, never defined in the invoking file" constraint on. See
`doc/plans/Three-Hook-Metasystem.md`.
