# TJLB Demos

Showcase programs for the TJLB language. **Demos are not part of the test
suite** — they exist to be read and run. (The shared standard library below
*is* regression-tested, via `tests/programs/stdlib_check.tjlb`.)

Run any demo with the JIT interpreter:

```bash
cargo run -- interpret demos/<name>.tjlb            # or target/…/tjlb-parser
cargo run -- interpret demos/life.tjlb -- watch     # args after --
```

Or compile to native via `./compile-file.sh demos/<name>.tjlb`.

## Feature showcases

| Demo | What it shows |
|------|---------------|
| [`mandelbrot.tjlb`](mandelbrot.tjlb) | ASCII Mandelbrot renderer. Tight `f64` loops, escape-time iteration, palette indexing. |
| [`life.tjlb`](life.tjlb) | Conway's Game of Life on a torus. Type-struct with private cell buffer, O(1) double-buffer swap, `std/rand` soup seeding. Pass any arg (e.g. `-- watch`) for ANSI animation. |
| [`calc.tjlb`](calc.tjlb) | Precedence-climbing expression evaluator in a single self-recursive method. Encapsulated cursor state, error carrying via a result struct. |
| [`vm.tjlb`](vm.tjlb) | Stack-based bytecode VM. Opcode dispatch through an array of function pointers — `ops[opcode](vm)` — running factorial and summation bytecode. |
| [`wordfreq.tjlb`](wordfreq.tjlb) | Word frequency counter. Composes the hash map, generic sort, and a `for_each` function-pointer callback. |
| [`sort_demo.tjlb`](sort_demo.tjlb) | One type-erased sort, four orderings: ints both ways, strings, and a `Person[6]` struct array by two different keys. |

## Language-tour demos

| Demo | What it shows |
|------|---------------|
| [`hello.tjlb`](hello.tjlb) | `$include`, printing, heap allocation with `sizeof(T)`. |
| [`types.tjlb`](types.tjlb) | Guided tour of the numeric type system (widths, signedness, casts, bit ops). Self-contained. |
| [`string_demo.tjlb`](string_demo.tjlb) | The `String` type-struct: factories, sret returns, autoref methods, encapsulation. |
| [`vec_demo.tjlb`](vec_demo.tjlb) | `VecI32` dynamic array: push/pop/at, amortised growth. |
| [`float_demo.tjlb`](float_demo.tjlb) | `f64` printing and NaN semantics (`x != x`). |
| [`list_init.tjlb`](list_init.tjlb) | List initializers: full / empty / partial / expression elements. |
| [`concat_args.tjlb`](concat_args.tjlb) | `argc`/`argv` handling. Run with `-- hello world`. |
| [`ask_name.tjlb`](ask_name.tjlb) | Interactive stdin (blocks — run in a terminal). |
| [`stress_test.tjlb`](stress_test.tjlb) | Kitchen-sink compiler exercise; `bench.tjlb` + `bench.c` compare against C. |

## The demo standard library (`std/`)

Plain `.tjlb` files pulled in with `$include` — no module system yet, so
paths are relative to the including file.

| Module | Provides |
|--------|----------|
| [`std/c/`](std/c/) | Raw libc externs: `stdio`, `stdlib`, `string`. |
| [`std/io/print.tjlb`](std/io/print.tjlb) | `print`/`println` for strings, all integer widths, `f64`. No `printf` (no varargs). |
| [`std/mem/alloc.tjlb`](std/mem/alloc.tjlb) | Byte-count allocation wrappers; pair with `sizeof(T)`. |
| [`std/math/math.tjlb`](std/math/math.tjlb) | min/max/clamp/abs per width, gcd/lcm, `ipow`, exact `isqrt_u64`, Newton `sqrt_f64`, floor/ceil/round, `PI`/`TAU`/`E`. |
| [`std/rand/rand.tjlb`](std/rand/rand.tjlb) | `Rng` type-struct (xorshift64\*): `next_u64`, `below`, `range_i64`, `next_f64`, `chance`. Deterministic per seed. |
| [`std/sort/sort.tjlb`](std/sort/sort.tjlb) | Type-erased `sort_bytes(base, n, size, cmp)` (quicksort + insertion), stock comparators, typed wrappers `sort_i32`/`sort_i64`/`sort_f64`/`sort_cstr`. |
| [`std/collections/map_str_i64.tjlb`](std/collections/map_str_i64.tjlb) | `MapStrI64`: FNV-1a, open addressing, key-owning `put`/`get_or`/`contains`/`for_each`/`destroy`. |
| [`std/string/String.tjlb`](std/string/String.tjlb) | Growable heap string. |
| [`std/vec/vec_i32.tjlb`](std/vec/vec_i32.tjlb) | Dynamic `i32` array. |
