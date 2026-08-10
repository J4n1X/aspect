# CLAUDE.md

Developer guide for the Aspect compiler — the conventions, architecture, and repo rules that aren't obvious from the code alone. Read it before making changes.

## Project

The Aspect compiler: a statically-typed, C-like systems language compiled to LLVM IR. Rust workspace over Inkwell 0.9 / LLVM 19.1. Binary `aspc`, library crate `aspect`, proc-macro crate `aspect-macros`, sources use `.ap`. (The directory is named `tjlb-rust` for historical reasons — the language was renamed to Aspect.)

Before touching compiler internals, invoke the `aspect-architecture` skill; before writing Aspect code, invoke the `aspect-handbook` skill. Both are condensed, token-cheap stand-ins for `doc/compiler/00-overview.md` and `doc/handbook.md` respectively — read the full doc only for what the skill tells you it doesn't cover.

## Commands

```bash
cargo build                       # release binary: target/release/aspc
cargo test                        # full suite: unit tests + corpus-generated integration tests
cargo test test_fibonacci         # single generated test (test_<relative_path_underscored>)
cargo test -- --nocapture

cargo run -- lex <FILE>           # token dump
cargo run -- parse <FILE>         # AST dump
cargo run -- compile <FILE> [-e ir|obj] [-O 0..3] [-I DIR] [-D NAME[=V]] [--target TRIPLE]
cargo run -- interpret <FILE> [-O N] [-- ARGS...]   # JIT in-process; main's i32 is the result

./compile-file.sh program.ap      # native executable via llc-19 + gcc → program.out
```

`-I lib` is required for anything importing the standard library (`$import std/io`, …). `ASPC_<MODE>_FLAGS` env vars (e.g. `ASPC_COMPILE_FLAGS="-I lib"`) splice flags in before CLI args.

## Architecture

Whole-program pipeline, one phase per module in `src/`:

1. **Lexer** (`lexer/`) — hand-written scanner → `Token` stream; every token's `Position` carries a `file_id` into the source-file registry, which is how multi-file diagnostics name the right file.
2. **Preprocessor** (`preprocessor/`) — `$`-directives over the token stream: `$define`/`$ifdef`/`$if`, and the module system (`$module`, `$import`, `-I` search roots). `$import` **inlines** the imported file's tokens into one stream at the directive site (import cycles are legal); compilation is whole-program by construction — there is no separate compilation. Produces `PreprocessedSource` (tokens + file registry + module map + import graph).
3. **Parser** (`parser/`) — two-pass over the single token stream: pass 1 parses signatures/globals/struct layouts (after prescans that pre-register `type` names and aliases) and only brace-skips function bodies; pass 2 parses the deferred bodies with the full symbol table — this is what makes forward references work (only global-var initializers stay order-sensitive). Grammar rules are written in a small DSL (`pos!`, `kw!`, `ident!`, `scoped!`, …) expanded by `#[parse_rule]` from `aspect-macros`. The parser is not purely syntactic: it resolves method-vs-field at `.`, mangles method calls to `Type$method` free functions, and enforces import visibility.
4. **Symbols** (`symbol/`) — `ModuleSymbols` (functions, type-structs + fields/methods, aliases, fn-ptr signatures) is built by the parser and **rides on `Program`**, so the checker and codegen consume the same table. Struct ids are interned once at parse time and codegen's GEP field indices depend on them — the registry cannot be rebuilt per phase. `symbol/table.rs` is the transient parse-time variable scope, discarded after parsing.
5. **Typechecker** (`typechecker/`) — single-pass bidirectional checker (`check` against an expected type / `synth`); it stamps `expr_type` and narrows literals, never restructures the AST. Errors are fatal. Implicit coercion (`types_coercible` in `typechecker/types.rs`) is widening-only within a numeric family but **ignores signedness** (`i32 -> u32` at equal width is implicit, no warning); `u0*` is the universal object pointer (C's `void*` rule).
6. **Codegen** (`codegen/`) — Inkwell → LLVM IR. Private (default) symbols get internal linkage and `optimize` runs `globaldce`, so unused stdlib is stripped; `public` symbols (and `main`/`_start`) survive. JIT execution (`jit_execute_main`) powers both the `interpret` subcommand and the whole integration-test harness — no external `lli`. LLVM types carry no signedness: signed vs unsigned is chosen per instruction (`sdiv`/`udiv`, `sext`/`zext`, `SLT`/`ULT`) from `LangType::base` at each site.

Cross-cutting: `src/target.rs` (`TargetSpec`: triple → ABI + `OS_*`/`ARCH_*` preprocessor defines), `src/asm.rs` (per-target register model validating `asm fn`/`naked fn`) and `src/variants.rs` (`VariantSpace`: what a `switch` scrutinee can be matched against — the one copy of that taxonomy, shared by parser, checker and codegen) are pure data usable before any LLVM target machine exists — the checker needs them long before codegen. `src/lib.rs` re-exports every module for the test harness.

The standard library (`lib/std/**`) is written in Aspect. `demos/` are showcase programs, **not** tests.

## Testing new features

Integration tests are corpus-generated: `generate_tests!()` (in `aspect-macros/src/generate_tests.rs`) scans `tests/programs/**/*.ap` at compile time and emits one `#[test]` per file carrying a `# expected:` annotation — adding a file IS adding a test. Every new language feature gets at least one runtime corpus program plus failure fixtures for each new diagnostic. For a small runtime check, prefer folding it into an existing thematically-close program (as a summed section in `main`, see `operators.ap` or `pointers.ap`) over adding a new file — this keeps the corpus from re-fragmenting into one-liners. Compile-failure fixtures are the exception: each halts compilation at its first error, so two diagnostics can never share a file — those stay one-per-file even when the trigger looks similar to a sibling fixture.

- Runtime test: `# expected: <i32>` — the program is JIT-run at **both -O0 and -O2**; each must return that value and the two must agree (a disagreement is its own failure).
- Compile-failure test: `# expected: "frag1", "frag2"` under `tests/programs/failures/`, named by stage prefix (`lexer_`, `parser_`, `type_`, `module_`, `asm_`, …); asserts every fragment appears in the error message (case-insensitive).
- Optional annotations: `# run_args:` (argv tail), `# compile_args: "-I", "lib"` (CLI flags — how stdlib-importing tests work, see `stdlib_check.ap`), `# requires_arch: ARCH_X86_64` (host-gates the generated test; required for arch-specific *failure* tests, which would otherwise compile clean on other hosts).
- Module fixtures for `$import` tests live in `tests/modules/` and `tests/modules_alt/`; they carry no `# expected:` line and are only loaded via `$import`.
- Rust unit tests go in `#[cfg(test)] mod tests` next to the code (see `codegen/functions.rs`, `parser/declarations.rs`).

Programs must define `main(u32 argc, u8 **argv) -> i32`. Never use `demos/` as tests — demo edits are unverified by CI.

## Documentation upkeep (repo rule)

A behaviour change is not done until the docs match:

- `doc/handbook.md` — user-facing language guide; new syntax/features get a section here.
- `doc/compiler/*.md` — one doc per stage; `09-syntax-reference.md` must reflect every syntax change, `07-testing.md` documents the harness.
- `doc/plans/` — active design docs; move completed ones to `doc/solved/`.
- `README.md` — CLI flags and user-visible features.
- `.claude/skills/aspect-handbook/SKILL.md` and `.claude/skills/aspect-architecture/SKILL.md` — condensed derivatives of the handbook/overview docs above; a change big enough to touch those source docs usually needs a matching edit here too, or the skill drifts out of sync and starts misleading agents.

## Language design review (repo rule)

Language-level changes — new syntax, semantics, type-system rules, builtins, attributes/directives, coercion behavior, or any ABI/target-visible surface — get a design review before implementation begins. The review looks for gaps (missing corner cases, interactions with existing features, doc/test debt) and judges whether the change is reasonable and consistent with Aspect's existing design. It resolves to one of three outcomes:

- **Approved** — proceed with implementation as proposed.
- **Approved with changes** — fold its required additions/changes into the proposal before implementing.
- **Rejected** — do not implement as proposed; either drop the change or address its concerns and re-review.

This gate applies to language-level changes only (parser/typechecker/codegen-visible surface) — not to internal refactors, tooling, or bug fixes that don't change the language.

## Git workflow

- Single-maintainer repo: local commits are fine anytime, but publishing — pushing any ref (including `<branch>:master` fast-forwards) or deleting remote branches — is the maintainer's call.
- Changes land as fast-forward pushes to `master`, not through pull requests.
- Keep commit history free of automated co-author or tooling trailers.

## Comments (repo rule)

Every comment must earn its line. This applies identically to `//` and to `///`
doc comments — a doc comment restating a name or signature is the single most
common form of the bloat, and the codebase has repeatedly had to be cleaned of it.

- **Comment why, never what.** If it restates the code in English, delete it.
- **A comment must answer one of four questions**: why this approach over the
  obvious one; what invariant or precondition the reader cannot see locally; what
  breaks if this changes; what upstream bug or edge case it exists to handle.
  Nothing else justifies one.
- **Default to zero.** Function bodies start with no comments and stay that way
  unless a line clears the bar above. Straight-line code, simple accessors,
  obvious loops and plain match arms stay bare. `# Panics`/`# Errors` sections go
  only where the contract is genuinely caller-facing — not on every accessor that
  happens to index a `Vec`.
- **Say it once.** A rationale covering N call sites belongs in exactly one place
  — the type, field or function it is about, or the stage doc under
  `doc/compiler/` — never repeated at each site. Repeated rationale is the worst
  form of this bloat: it reads as thorough and survives every local cleanup.
- **One or two lines.** A rationale that needs a paragraph is documentation. Put
  it in `doc/compiler/*.md` and let the code point there.
- **Audit before committing, as a step.** Re-read what your diff adds
  (`git diff | grep -E '^\+\s*(//|///)'`) and delete everything that only
  restates code. A commit that adds more comment lines than it needed is a defect
  and gets sent back.
- Run `cargo doc` after writing doc comments; rewrite anything that warns. 