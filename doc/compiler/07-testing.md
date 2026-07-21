# Testing

## Test Structure

Integration tests live in `tests/integration_tests.rs` and are split into two suites:

1. Runtime tests: compile valid `.ap` programs from `tests/programs/` through the full pipeline and JIT-execute them in-process via `CodeGenerator::jit_execute_main` — twice, at `-O0` and `-O2`. The `i32` returned by `main` is the asserted result, and the two levels must agree.
2. Compile-failure tests: compile invalid `.ap` programs from `tests/programs/failures/` and assert that compilation fails with stage-appropriate diagnostics.

There is no stdout comparison. Programs must define the canonical
`main(u32 argc, u8 **argv) -> i32` entry point; the test harness prepends the
source path as `argv[0]` and forwards any `# run_args:` entries as `argv[1..]`.

## How Tests Work

Test functions are **generated at compile time** by the `generate_tests!()` proc macro defined in `aspect-macros/src/generate_tests.rs`. The macro:

1. Scans `tests/programs/` recursively for `*.ap` files.
2. Reads the first 10 lines of each file looking for `# expected:`, `# run_args:`, `# compile_args:`, `# requires_arch:`, and `# expected_warning:` annotations.
3. Emits one `#[test]` function per annotated file.

At runtime each generated test calls the appropriate helper:

- **Runtime test**: `compile_and_run[_with_args]` → tokenize → parse → typecheck → codegen → run at **-O0** and again at **-O2** (optimizer with `verify_each`) → assert both returned the expected `i32` **and that the two agree**. A -O0/-O2 disagreement is reported as its own failure ("is optimization-level dependent"), not resolved in favour of either: `-O0` and `-O1+` take materially different paths (an `asm fn` stays a real call at `-O0` and is folded in by `alwaysinline` at `-O1+`), so neither level alone covers the corpus.
- **Failure test**: `assert_compile_error_contains` → runs the compile pipeline, asserts it returns an `Err` whose message contains all expected fragments.

### Annotation format

```aspect
# expected: 42                         # compile & run; assert main's i32 return == 42
# expected: "frag1", "frag2"           # compile only; assert error contains each fragment
# run_args: "arg1", "arg2"            # optional: forwarded as argv[1..] to main
# compile_args: "-I", "lib"           # optional: compiler flags (-D/-I), mirroring the CLI
# requires_arch: ARCH_X86_64          # optional: bare, unquoted; compile this test only on that host arch
# expected_warning: "frag"            # optional: assert a non-fatal typecheck warning contains this fragment
```

`# expected_warning:` rides on a **runtime** (`# expected: <code>`) test: the
program must still compile, run, and return its code, and additionally emit a
type-checker warning whose text contains the fragment (case-insensitive). It is
checked against `TypeChecker::warnings()` — not stderr — via
`assert_warning_contains`, so it does not depend on capturing the in-process
JIT's output. A warning never fails the build on its own, which is why it is an
add-on assertion rather than a separate test shape.

`# requires_arch:` gates the generated test with `#[cfg(target_arch = ...)]`. It exists
because an `$ifdef`-gated *failure* test compiles clean on the wrong arch, which then
trips the "expected compilation to fail" assertion — so the gating has to happen outside
the Aspect source. An unrecognised arch name leaves the test **ungated** rather than
silently disabling it (`cfg_target_arch_for` returns `None` → no gate).

Files without a `# expected:` line are silently skipped by the macro.

### Adding a new test

Create a `.ap` file anywhere under `tests/programs/`, add a `# expected:` line, and `cargo test` picks it up automatically — no changes to `integration_tests.rs` needed.

`tests/programs/` is the **only** scan root. The `demos/` folder is
deliberately not scanned — demos are showcase programs, not regression
tests. The standard library (`lib/std/**`) they share *is* covered, via
`tests/programs/stdlib_check.ap`, which `$import`s it with
`# compile_args: "-I", "lib"`. Module fixtures for import tests live in
`tests/modules/` (and `tests/modules_alt/` for search-order tests); they
carry no `# expected:` line and are only loaded via `$import`.

### Argument Passing

`compile_and_run_with_args()` forwards the `# run_args:` entries as the
program's `argv[1..]` (argv[0] is set to the source path by the harness).
Used by `array_access.ap`, which passes `"array_access_test"` as `argv[1]`.

## Prerequisites

- None beyond `cargo test`. The runtime suite JITs in-process via Inkwell's
  `ExecutionEngine`; no external `lli` binary is needed.

## Test Programs

The corpus is `tests/programs/*.ap`, one program per feature. To list it with the
value each asserts:

```bash
grep -H '^# expected:' tests/programs/*.ap
```

Programs are named after the feature they cover (`struct_*`, `module_*`, `preproc_*`,
`asm_*`, `fnptr_*`, `value_block*`, `void_*`, …), so the listing *is* the coverage map.
It is deliberately not duplicated here: a hand-maintained table drifted 44 programs
behind the corpus, in the very document that tells you how to add a program to it.

## Compile-Failure Suite

Failure fixtures live in `tests/programs/failures/`, one per diagnostic. To list them
with the fragments each asserts:

```bash
grep -H '^# expected:' tests/programs/failures/*.ap
```

Fixtures are grouped by filename prefix: `lexer_`, `parser_`, `preproc_`, `type_`,
`module_`, `asm_`, `public_`, `void_`, `value_block_`. A fixture with no `# expected:`
line is silently skipped by the macro — it generates no test at all.

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
./compile-file.sh program.ap   # produces program.out
```

Pipeline: `cargo run -- compile` → `llc-19` (x86-64 Intel syntax assembly) → `gcc` (assemble + link).
