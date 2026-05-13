# Testing

## Test Structure

Integration tests live in `tests/integration_tests.rs` and are split into two suites:

1. Runtime tests: compile valid `.tjlb` programs from `tests/programs/`, execute with `lli-19`, and assert the process exit code.
2. Compile-failure tests: compile invalid `.tjlb` programs from `tests/programs/failures/` and assert that compilation fails with stage-appropriate diagnostics.

There is no stdout comparison.

## How Tests Work

Test functions are **generated at compile time** by the `generate_tests!()` proc macro defined in `tjlb-macros/src/generate_tests.rs`. The macro:

1. Scans `tests/programs/` recursively for `*.tjlb` files.
2. Reads the first 10 lines of each file looking for `# expected:` and `# run_args:` annotations.
3. Emits one `#[test]` function per annotated file.

At runtime each generated test calls the appropriate helper:

- **Runtime test**: `compile_and_run[_with_args]` → tokenize → parse → typecheck → codegen → `lli-19` → assert exit code.
- **Failure test**: `assert_compile_error_contains` → runs the compile pipeline, asserts it returns an `Err` whose message contains all expected fragments.

### Annotation format

```tjlb
# expected: 42                         # compile & run; assert exit code == 42
# expected: "frag1", "frag2"           # compile only; assert error contains each fragment
# run_args: "arg1", "arg2"            # optional: argv forwarded to lli-19
```

Files without a `# expected:` line are silently skipped by the macro.

### Adding a new test

Create a `.tjlb` file anywhere under `tests/programs/`, add a `# expected:` line, and `cargo test` picks it up automatically — no changes to `integration_tests.rs` needed.

### Argument Passing

`compile_and_run_with_args()` supports passing command-line arguments to `lli-19`. Controlled via the `# run_args:` annotation, used by `array_access.tjlb` which passes `"array_access_test"` as argv[1].

## Prerequisites

- `lli-19` must be on `PATH` for runtime tests.
- Compile-failure tests do not execute `lli-19`; they only run compiler stages and assert diagnostics.

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
| 10 | `array_access.tjlb` | 17 | Extern `strlen` on `argv[1]` (`"array_access_test"` = 17 chars) |
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

## Compile-Failure Suite

Failure fixtures are stored in `tests/programs/failures/`.

Current coverage:

| Stage | File | Expected diagnostic fragment(s) |
|---|---|---|
| Lexer | `lexer_unterminated_string.tjlb` | `unterminated string` |
| Lexer | `lexer_invalid_escape_sequence.tjlb` | `invalid escape sequence` |
| Parser | `parser_missing_initializer_expression.tjlb` | `expected expression` |
| Type checker | `type_assignment_to_const.tjlb` | `cannot assign to const variable` |
| Type checker | `type_argument_count_mismatch.tjlb` | `expects 2 arguments`, `got 1` |
| Type checker | `type_return_type_mismatch.tjlb` | `type mismatch`, `i32`, `f64` |
| Type checker | `type_invalid_dereference.tjlb` | `cannot dereference non-pointer type` |
| Type checker | `type_list_initializer_too_long.tjlb` | `list initializer has`, `array only has room for 2` |
| Type checker | `literal_overflow.tjlb` | `type mismatch`, `u8`, `i32` |

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
