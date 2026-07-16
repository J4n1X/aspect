# Aspect Demos

Showcase programs for the Aspect language. **Demos are not part of the test
suite** — they exist to be read and run. (The shared standard library below
*is* regression-tested, via `tests/programs/stdlib_check.ap`.)

Run any demo with the JIT interpreter. Demos import the standard library
(`lib/std/**`), so pass the `-I lib` search root:

```bash
cargo run -- interpret -I lib demos/<name>.ap            # or target/…/aspc
cargo run -- interpret -I lib demos/life.ap -- watch     # args after --
```

Or compile to native via `./compile-file.sh demos/<name>.ap`
(the script passes `-I lib` itself).

## Feature showcases

| Demo | What it shows |
|------|---------------|
| [`mandelbrot.ap`](mandelbrot.ap) | ASCII Mandelbrot renderer. Tight `f64` loops, escape-time iteration, palette indexing. |
| [`life.ap`](life.ap) | Conway's Game of Life on a torus. Type-struct with private cell buffer, O(1) double-buffer swap, `std/rand` soup seeding. Pass any arg (e.g. `-- watch`) for ANSI animation. |
| [`calc.ap`](calc.ap) | Precedence-climbing expression evaluator in a single self-recursive method. Encapsulated cursor state, error carrying via a result struct. |
| [`vm.ap`](vm.ap) | Stack-based bytecode VM. Opcode dispatch through an array of function pointers — `ops[opcode](vm)` — running factorial and summation bytecode. |
| [`wordfreq.ap`](wordfreq.ap) | Word frequency counter. Composes the hash map, generic sort, and a `for_each` function-pointer callback. |
| [`sort_demo.ap`](sort_demo.ap) | One type-erased sort, four orderings: ints both ways, strings, and a `Person[6]` struct array by two different keys. |

## Language-tour demos

| Demo | What it shows |
|------|---------------|
| [`hello.ap`](hello.ap) | `$import std/...`, printing, heap allocation with `sizeof(T)`. |
| [`types.ap`](types.ap) | Guided tour of the numeric type system (widths, signedness, casts, bit ops). Self-contained. |
| [`string_demo.ap`](string_demo.ap) | The `String` type-struct: factories, sret returns, autoref methods, encapsulation. |
| [`vec_demo.ap`](vec_demo.ap) | `VecI32` dynamic array: push/pop/at, amortised growth. |
| [`float_demo.ap`](float_demo.ap) | `f64` printing and NaN semantics (`x != x`). |
| [`list_init.ap`](list_init.ap) | List initializers: full / empty / partial / expression elements. |
| [`concat_args.ap`](concat_args.ap) | `argc`/`argv` handling. Run with `-- hello world`. |
| [`ask_name.ap`](ask_name.ap) | Interactive stdin (blocks — run in a terminal). |
| [`stress_test.ap`](stress_test.ap) | Kitchen-sink compiler exercise; `bench.ap` + `bench.c` compare against C. |

## The standard library (`lib/std/`)

Real modules under [`../lib/std/`](../lib/std/), pulled in with
`$import <module>` and resolved against the `-I lib` search root. See
[`doc/10-modules.md`](../doc/10-modules.md) for the module system.

| Import | Provides |
|--------|----------|
| `std/c/stdio`, `std/c/stdlib`, `std/c/string` | Raw libc externs at header granularity. |
| `std/io` | `print`/`println` for strings, all integer widths, `f64`. No `printf` (no varargs). |
| `std/mem` | Byte-count allocation wrappers; pair with `sizeof(T)`. |
| `std/math` | min/max/clamp/abs per width, gcd/lcm, `ipow`, exact `isqrt_u64`, Newton `sqrt_f64`, floor/ceil/round, `PI`/`TAU`/`E`. |
| `std/rand` | `Rng` type-struct (xorshift64\*): `next_u64`, `below`, `range_i64`, `next_f64`, `chance`. Deterministic per seed. |
| `std/sort` | Type-erased `sort_bytes(base, n, size, cmp)` (quicksort + insertion), stock comparators, typed wrappers `sort_i32`/`sort_i64`/`sort_f64`/`sort_cstr`. |
| `std/collections` | `MapStrI64`: FNV-1a, open addressing, key-owning `put`/`get_or`/`contains`/`for_each`/`destroy`. |
| `std/string` | Growable heap `String`. |
| `std/vec` | Dynamic `i32` array `VecI32`. |

Imports are not transitive: `$import std/sort` does not hand you
`strcmp` — a demo that calls libc directly imports the `std/c/*` module
itself (see the import lists at the top of each demo).
