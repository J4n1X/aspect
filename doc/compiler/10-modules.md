# Modules

Aspect's module system is the language's load unit and visibility boundary.
Two directives carry it (see [09-syntax-reference.md](09-syntax-reference.md)
§ Preprocessor for the general directive rules):

- **`$module <path>`** declares which module a file belongs to.
- **`$import <path>`** makes a module part of the compilation.

Both live in the preprocessor (`src/preprocessor/modules.rs`); the
visibility rule is enforced later, at parse-time symbol resolution.

```aspect
# in lib/std/io/linux.ap — one of the four files declaring $module std/io:
$module std/io
$import std/linux/syscall

fn io_write_bytes(i32 fd, u8* buf, u64 n) -> i64 { return sys_write(fd, buf, n) }
```

```aspect
# in an application file:
$import std/io

fn main(u32 argc, u8** argv) -> i32 {
    return println("hello")     # from lib/std/io/generic.ap — same module
}
```

Compile with the search root that holds the library:

```bash
cargo run -- interpret app.ap -I lib
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

- **file form:** `<root>/std/io.ap`, or
- **directory form:** every `.ap` file directly inside `<root>/std/io/`
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

## Type-struct visibility: `public type`

Importing a module is necessary but not sufficient to use its
type-structs. A `type` is **private to its defining module** by default;
`public type Name { ... }` exports it. The gate fires wherever the type
is *used* from another module — naming it (declarations, casts, `sizeof`,
struct literals, the `Type` in a static `Type.method` call) and calling
its methods, including on an instance:

```
error: type-struct 'Secret' is private to module 'shapes' and cannot be
used from the root module — declare it `public type` to export it
```

The rules that fall out:

- A member's own `public` is capped by the type's: a `public fn` on a
  private type is callable anywhere in the defining module, never
  outside it.
- **Values still flow.** Outside code may hold a foreign private type's
  value — returned from and passed back into the defining module's
  public functions — it just cannot name the type or call methods on it.
  Since Aspect has no type inference, a by-value foreign private struct
  cannot even be bound to a local outside its module; the practical
  opaque-handle shape is a pointer, handed out as `T*` and held as `u0*`.
- **v1 caveat — public fields are not yet gated.** A `public` *field* of
  a foreign private type is currently readable **and writable** through
  a legally obtained instance: field access is enforced by the
  typechecker, which has no file-to-module map today, so only member
  visibility applies. A private type that keeps its fields private is
  fully opaque; one with public fields is methods-opaque but
  fields-transparent. Pinned by
  `tests/programs/module_private_field_access.ap`; the gate lands when
  `Program::file_modules` does.
- An alias does not launder privacy: a visible alias whose target is a
  private type-struct fails the same check at the use site.
- `public` does not bypass the import rule — an exported type is still
  invisible to modules that don't import its module.

Like `file_id`, the visibility is captured by the parser's type-name
prescan (`public` token directly before `type`) so it is known before any
body parses — under import cycles a module's uses can legally precede
the definition in the inlined stream.

## Enum visibility: `public enum`

Enums follow the exact same model. A plain `enum` is private to its
defining module; `public enum Name { ... }` exports it. The gate fires
wherever the enum is *used* from another module — naming it (declarations,
casts) or referencing a variant (`Name.Variant`):

```
error: enum 'Secret' is private to module 'palette' and cannot be used
from the root module — declare it `public enum` to export it
```

As with type-structs, an alias does not launder an enum's privacy, `public`
does not bypass the import rule, and the visibility is captured by a
name-collection prescan (`public` token directly before `enum`) so it is
known before any body parses. Values of a private enum still flow through
the defining module's public functions (e.g. returned as an `i32` derived
from a variant), even though the type itself cannot be named outside.

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
| `std/c/mman` | file | `lib/std/c/mman.ap` | — |
| `std/c/stdio` | file | `lib/std/c/stdio.ap` | — |
| `std/c/stdlib` | file | `lib/std/c/stdlib.ap` | — |
| `std/c/string` | file | `lib/std/c/string.ap` | — |
| `std/c/unistd` | file | `lib/std/c/unistd.ap` | — |
| `std/io` | directory | `lib/std/io/generic.ap`, `lib/std/io/linux.ap`, `lib/std/io/windows.ap`, `lib/std/io/posix.ap` | `std/linux/syscall` (Linux/x86-64), `std/c/unistd` (other POSIX) |
| `std/linux/syscall` | file | `lib/std/linux/syscall.ap` | — |
| `std/math` | file | `lib/std/math.ap` | — |
| `std/mem` | directory | `lib/std/mem/generic.ap` | `std/c/stdlib` |
| `std/mem/page` | directory | `lib/std/mem/page/generic.ap`, `lib/std/mem/page/linux.ap`, `lib/std/mem/page/windows.ap`, `lib/std/mem/page/posix.ap` | `std/linux/syscall` (Linux/x86-64), `std/c/mman` + `std/c/unistd` (other POSIX) |
| `std/rand` | file | `lib/std/rand.ap` | — |
| `std/sort` | file | `lib/std/sort.ap` | `std/c/string` |
| `std/string` | directory | `lib/std/string/String.ap` | `std/c/stdlib`, `std/c/string` |
| `std/vec` | directory | `lib/std/vec/vec_i32.ap` | `std/c/stdlib` |
| `std/collections` | directory | `lib/std/collections/map_str_i64.ap` | `std/c/stdlib`, `std/c/string` |

Note the consequence of enforced non-transitivity: `$import std/sort`
gives you `sort_bytes` and `sort_cstr`, but **not** `strcmp` — if your
file calls `strcmp` itself, it writes `$import std/c/string` itself.

`std/linux/syscall` is the raw Linux/x86-64 syscall layer (`sys_write`, `STDOUT`,
`O_RDONLY`, `SEEK_SET`, …), built on `asm fn` with no libc. `std/io` and
`std/mem/page` import it for their Linux backends. Non-transitivity applies to it
like everything else, and uniformly: `$import std/io` gives you neither
`sys_write` **nor** `STDOUT` — both report "is defined in module
`std/linux/syscall`, which the root module does not import". Its constants are
ordinary `const` globals, not `$define`s (the module contains none), so nothing
about it leaks textually. Want it? Import it yourself.

The `std/c/*` modules are raw libc `extern fn` bindings, importable at
header granularity like their C namesakes. The directory-form modules
(`std/vec`, `std/collections`, …) are the ones expected to grow one file
per concrete type until generics exist.

## Metasystem outlook

The compiler records `file_id → module path` for every file — this
identity is what the planned metasystem hangs its "expansions must be
imported, never defined in the invoking file" constraint on. See
`doc/plans/Three-Hook-Metasystem.md`.
