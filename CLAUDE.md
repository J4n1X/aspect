# CLAUDE.md

Developer guide for the Aspect compiler — the conventions, architecture, and repo rules that aren't obvious from the code alone. Read it before making changes.

## Project

The Aspect compiler: a statically-typed, C-like systems language compiled to LLVM IR. Rust workspace over Inkwell 0.9 / LLVM 19.1. Binary `aspc`, library crate `aspect`, proc-macro crate `aspect-macros`, sources use `.ap`. (The directory is named `tjlb-rust` for historical reasons — the language was renamed to Aspect.)

Before touching compiler internals, read `doc/compiler/00-overview.md`; before writing Aspect code, skim `doc/handbook.md`.

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
4. **Symbols** (`symbol/`) — `ModuleSymbols` (functions, type-structs + fields/methods/attributes, aliases, fn-ptr signatures) is built by the parser and **rides on `Program`**, so the checker and codegen consume the same table. Struct ids are interned once at parse time and codegen's GEP field indices depend on them — the registry cannot be rebuilt per phase. `symbol/table.rs` is the transient parse-time variable scope, discarded after parsing.
5. **Typechecker** (`typechecker/`) — single-pass bidirectional checker (`check` against an expected type / `synth`); it stamps `expr_type` and narrows literals, never restructures the AST. Errors are fatal. Implicit coercion (`types_coercible` in `typechecker/types.rs`) is widening-only within a numeric family but **ignores signedness** (`i32 -> u32` at equal width is implicit, no warning); `u0*` is the universal object pointer (C's `void*` rule).
6. **Codegen** (`codegen/`) — Inkwell → LLVM IR. Private (default) symbols get internal linkage and `optimize` runs `globaldce`, so unused stdlib is stripped; `public` symbols (and `main`/`_start`) survive. JIT execution (`jit_execute_main`) powers both the `interpret` subcommand and the whole integration-test harness — no external `lli`. LLVM types carry no signedness: signed vs unsigned is chosen per instruction (`sdiv`/`udiv`, `sext`/`zext`, `SLT`/`ULT`) from `LangType::base` at each site.

Cross-cutting: `src/target.rs` (`TargetSpec`: triple → ABI + `OS_*`/`ARCH_*` preprocessor defines) and `src/asm.rs` (per-target register model validating `asm fn`/`naked fn`) are pure data usable before any LLVM target machine exists — the checker needs them long before codegen. `src/lib.rs` re-exports every module for the test harness.

The standard library (`lib/std/**`) is written in Aspect. `demos/` are showcase programs, **not** tests.

An active design document, `doc/plans/Three-Hook-Metasystem.md`, drives current work (inert attributes landed as its Phase 0; rules-as-builtins Phase 2a is next). Read it before touching attribute or metaprogramming machinery.

## Testing new features

Integration tests are corpus-generated: `generate_tests!()` (in `aspect-macros/src/generate_tests.rs`) scans `tests/programs/**/*.ap` at compile time and emits one `#[test]` per file carrying a `# expected:` annotation — adding a file IS adding a test. Every new language feature gets at least one runtime corpus program plus failure fixtures for each new diagnostic.

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

## Comments

Comments are important, but they should not bloat the codebase.

- Comment why, never what. If the comment just restates the line in English, don't write it.
- No comment on a line unless it answers one of: why this approach over the obvious one, what invariant/precondition the reader can't see locally, what will break if this changes, or what upstream bug/edge case this exists to handle.
- Default to zero comments in straight-line, self-explanatory code (simple getters, obvious loops, straightforward match arms).
- Never add a comment restating the function name or type signature in prose.
- When writing documentation comments in Rust, you should run "cargo doc" afterwards to see if your comment causes a warning, and if it does, you rewrite it. 