# Testing

## Test Structure

Integration tests live in `tests/integration_tests.rs`. Each test compiles a `.tjlb` program from `tests/programs/` to LLVM IR, executes it with `lli-19`, and checks the **process exit code** as the expected result. There is no stdout comparison.

## How Tests Work

1. `compile_and_run(path)` reads the `.tjlb` source file
2. Runs the pipeline: tokenize → parse → codegen (type checking is **skipped** in the test helper)
3. Writes LLVM IR to a `NamedTempFile` (auto-deleted on drop)
4. Executes with `lli-19` (LLVM interpreter/JIT) as a child process
5. Returns the exit code as `i32`
6. Test asserts `assert_eq!(result, expected_exit_code)`

The `main() -> i32` function's return value becomes the process exit code.

### Argument Passing

`compile_and_run_with_args()` supports passing command-line arguments to `lli-19`, used by `test_array_access` which passes `"array_access_test"` as argv[1].

## Prerequisites

- `lli-19` must be on `PATH` — tests will fail without it.
- Type checking is **skipped** in the test helper (goes parse → codegen directly). Type errors would only be caught at codegen time.

## Test Programs

| # | File | Expected Exit | Description |
|---|------|:---:|-------------|
| 1 | `return_42.tjlb` | 42 | Minimal: `main()` returns `42` |
| 2 | `arithmetic.tjlb` | 27 | Integer arithmetic with parens: `(10 + 5) * 2 - 3` |
| 3 | `pointer_arithmetic.tjlb` | 123 | Pointer add/subtract, cast to int |
| 4 | `fibonacci.tjlb` | 13 | Recursive `fib(7)` |
| 5 | `loops.tjlb` | 60 | While loop (sum 1..5=15) + for loop (×2 twice) |
| 6 | `conditionals.tjlb` | 50 | If/else with `max()` helper: `max(15,20) + max(30,25)` |
| 7 | `global_vars.tjlb` | 103 | Global variable mutation via helper function |
| 8 | `pointers.tjlb` | 42 | Pass-by-pointer: `modify(&value)` → `32 + 10` |
| 9 | `bitwise.tjlb` | 28 | `&`, `\|`, `^` on 12 and 10: `8 + 14 + 6` |
| 10 | `array_access.tjlb` | 18 | Extern `strlen` on `argv[1]` (18 chars) |
| 11 | `break_continue.tjlb` | 22 | Break/continue in for and while loops |
| 12 | `logical_ops.tjlb` | 121 | `&&`, `\|\|`, `!` operators |
| 13 | `bitwise_not.tjlb` | 42 | `~5 = -6`, then `(~5 + 6) + 42` |
| 14 | `variable_shadowing.tjlb` | 10 | Block-scoped shadowing: inner `x=20` doesn't affect outer `x=10` |

## Features Exercised

| Feature | Programs |
|---------|----------|
| Basic return | All |
| Integer arithmetic | `arithmetic`, `loops`, `break_continue` |
| Recursive functions | `fibonacci` |
| If/else | `conditionals`, `logical_ops`, `break_continue` |
| While loops | `loops`, `break_continue` |
| For loops | `loops`, `break_continue` |
| Break/continue | `break_continue` |
| Logical operators | `logical_ops` |
| Bitwise operators | `bitwise`, `bitwise_not` |
| Pointers | `pointers`, `pointer_arithmetic` |
| Pointer arithmetic | `pointer_arithmetic` |
| Global variables | `global_vars` |
| Extern C functions | `array_access` |
| Command-line args | `array_access` |
| Type casts | `pointer_arithmetic`, `array_access` |
| Variable shadowing | `variable_shadowing` |
| Block scoping | `variable_shadowing` |

## Running Tests

```bash
# All tests
cargo test

# Single test by name
cargo test test_fibonacci

# With output
cargo test -- --nocapture
```

## Native Compilation

`compile-file.sh` compiles to a native executable:

```bash
./compile-file.sh program.tjlb   # produces program.out
```

Pipeline: `cargo run -- compile` → `llc-19` (x86-64 Intel syntax assembly) → `gcc` (assemble + link).
