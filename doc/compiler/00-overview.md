# Architecture Overview

Aspect is a statically-typed, low-level programming language compiler written in Rust. It compiles a C-like language to LLVM IR via the [Inkwell](https://github.com/TheDan64/inkwell) safe Rust bindings. The binary is named `aspc` and the library crate is `aspect`.

## Project Structure

```
aspect/
├── src/
│   ├── main.rs              # CLI entry point (clap-derived)
│   ├── lib.rs               # Library root — re-exports all modules
│   ├── lexer/               # Tokenization
│   │   ├── scanner.rs       # Lexer implementation
│   │   ├── tokens.rs        # Token and type definitions
│   │   └── errors.rs        # Lexer error types
│   ├── preprocessor/        # $-directive expansion over the token stream
│   │   ├── mod.rs           # Driver: line-anchored dispatch
│   │   ├── defines.rs       # $define/$undefine, -D, platform defines
│   │   ├── conditional.rs   # $ifdef/$if chains + const evaluator
│   │   ├── modules.rs       # $module/$import, -I resolution
│   │   └── errors.rs        # Preprocessor error types
│   ├── parser/              # AST construction
│   │   ├── ast.rs           # AST node types
│   │   ├── expressions.rs   # Expression parsing (Pratt-style)
│   │   ├── statements.rs    # Statement parsing
│   │   ├── types.rs         # Re-export of lexer types
│   │   └── errors.rs        # Parser error types
│   ├── symbol/              # Symbol tables
│   │   ├── table.rs         # Transient parse-time variable scopes
│   │   └── module.rs        # ModuleSymbols: functions/type-structs/aliases, rides on Program
│   ├── typechecker/         # Semantic validation
│   │   ├── checker.rs       # Constraint-based type checker
│   │   ├── types.rs         # Constraint definitions
│   │   └── errors.rs        # Type error types
│   ├── codegen/             # LLVM IR emission
│   │   ├── generator.rs     # CodeGenerator struct + orchestration (not the bulk of IR gen)
│   │   ├── expressions.rs   # walk_expression: the unified expression walker
│   │   ├── statements.rs    # Statement generators
│   │   ├── value_emitter.rs # ValueEmitter trait + RuntimeEmitter + ConstantEmitter
│   │   ├── functions.rs     # declare/generate_function, FunctionScope RAII
│   │   ├── structs.rs       # Type-struct registration, lowering, lvalue path
│   │   ├── asm.rs           # `asm fn` lowering
│   │   ├── globals.rs       # Globals, string literals, list initializers
│   │   ├── scope.rs         # ScopeStack, LocalVar, GlobalVarInfo, VarRef
│   │   ├── types.rs         # LangType → LLVM type translation
│   │   └── errors.rs        # Codegen error types
│   ├── asm.rs               # Target register model shared by checker and codegen
│   ├── target.rs            # Target-triple detection; backs --target
│   ├── scope.rs             # Generic ScopeStack<T> shared across phases
│   └── lib.rs               # Library exports
├── aspect-macros/           # Workspace member: proc macros (see 08-parser-macro-rewrite.md)
│   └── src/
│       ├── lib.rs           # #[parse_rule] + generate_tests!()
│       ├── expand.rs        # DSL macro expansions (DslRewriter)
│       └── generate_tests.rs# Test-suite generation from tests/programs/
├── tests/
│   ├── integration_tests.rs # Integration test suite
│   ├── programs/            # .ap test programs (+ programs/failures/)
│   ├── modules/             # module fixtures imported by test programs
│   └── modules_alt/         # second search root, for -I search-order tests
├── lib/                     # The standard library (lib/std/**), pass -I lib
├── doc/                     # This documentation
├── build.rs                 # Build script
├── Cargo.toml               # Workspace + dependencies and features
└── compile-file.sh          # Native compilation script
```

## Compilation Pipeline

The compiler runs a 6-stage pipeline:

```
Source (.ap)
    │
    ▼
┌──────────┐  Vec<Token>  ┌──────────────┐  expanded Vec<Token>
│  Lexer   │ ───────────▶ │ Preprocessor │ ──────────────┐
└──────────┘              └──────────────┘               │
     ▲                          │ $import loads          │
     └──────────────────────────┘ more files             ▼
                                                   ┌──────────┐   Program (AST)
                                                   │  Parser  │ ──────────────┐
                                                   └──────────┘               │
                                                                              ▼
                                                                      ┌──────────────┐
                                                                      │ Type Checker │
                                                                      └──────────────┘
                                                                              │ Ok / Vec<Error>
                                                                              ▼
                                                                        ┌──────────┐   Module
                                                                        │ Codegen  │ ─────────┐
                                                                        └──────────┘          │
                                                                                              ▼
                                                                                        ┌──────────┐
                                                                                        │ Optimize │
                                                                                        └──────────┘
                                                                                              │
                                                                                              ▼
                                                                                        LLVM IR (.ll)
```

### Stage 1: Lexing (`src/lexer/`)

Tokenizes source text into a flat `Vec<Token>`. Handles:
- Operators (arithmetic, bitwise, logical, comparison, assignment)
- Integer literals (decimal, hex, binary), float literals, string literals
- Keywords (`fn`, `if`, `while`, `for`, `return`, `as`, etc.)
- Built-in types (`i32`, `u64`, `f64`, etc.) parsed as `LangType` tokens
- Comments (`#` line comments, `#- ... -#` block comments) — discarded, not tokenized
- Newlines as explicit tokens (statement terminators)

See [01-lexer.md](01-lexer.md).

### Stage 2: Preprocessing (`src/preprocessor/`)

Walks the token stream expanding line-anchored `$` directives before the
parser sees it:
- `$define`/`$undefine` + `-D` CLI defines + platform defines (`OS_LINUX`, `ARCH_X86_64`, …), identifier-token substitution
- `$ifdef`/`$ifndef`/`$if`/`$elseif`/`$elseifdef`/`$else`/`$endif` conditional chains with a constant-expression evaluator
- `$module`/`$import` — module identity and loading, resolved against `-I` search roots; imported files recurse through the lexer and preprocessor

Output is a single expanded token stream plus a file registry (positions
carry a `file_id`, so downstream errors name the right file) and the
module/import tables the parser's visibility check consumes.

See [09-syntax-reference.md](09-syntax-reference.md) § Preprocessor and [10-modules.md](10-modules.md).

### Stage 3: Parsing (`src/parser/`)

Recursive descent parser with precedence climbing for expressions. Produces:
- `Program` containing `Vec<Function>`, `Vec<GlobalVar>`, and `Vec<String>` (string literal table)
- Builds the `SymbolTable` during parsing (not a separate pass)
- Desugars several constructs at parse time (compound assignments, unary minus, elif, array indexing)

See [02-parser.md](02-parser.md) and [03-ast.md](03-ast.md).

### Stage 4: Type Checking (`src/typechecker/`)

Constraint-based type checker in three phases:
1. Register all function signatures and global variable types
2. Walk each function body collecting `TypeConstraint` entries
3. Verify all constraints, collecting errors into a `Vec`

Type errors are **fatal** — any error aborts compilation. The checker validates:
- Type compatibility (with implicit widening rules)
- Const enforcement
- Pointer dereference validity
- Function argument types and counts
- Return type matching
- Condition types (must be non-void)

See [05-typechecker.md](05-typechecker.md).

### Stage 5: Code Generation (`src/codegen/`)

Emits LLVM IR via Inkwell (pinned to LLVM 19.1). Key design:
- Two-pass function compilation: declare all signatures first, then generate bodies (enables forward references)
- All `alloca` instructions hoisted to the entry block (required for `mem2reg`)
- Opaque pointers throughout (`ptr` type for all pointer/array variables)
- Signedness tracked via `LangType::base` and applied at instruction selection time

See [06-codegen.md](06-codegen.md).

### Stage 6: Optimization (optional)

Runs LLVM's new pass manager with pipeline strings `default<O0>` through `default<O3>`.

## Dependencies

| Crate | Purpose |
|-------|---------|
| `inkwell` 0.9 (`default-features = false`, features `llvm19-1` + `target-x86`) | Safe Rust bindings to LLVM 19.1 |
| `clap` 4.6 (feature `derive`) | CLI argument parsing |
| `anyhow` 1.0 | Contextual error handling |
| `thiserror` 2.x | Derive for custom error enums |
| `indexmap` 2 | `IndexSet` for the string-literal table (insertion-ordered, dedup) |
| `aspect-macros` (path) | In-repo proc macros: `#[parse_rule]`, `generate_tests!()` — see [08-parser-macro-rewrite](08-parser-macro-rewrite.md) |

Dev-dependencies: `pretty_assertions` 1.4.

Versions are given to the precision that matters; see `Cargo.toml` for the exact
pins, which drift with `cargo update`.

External tools required on `PATH`: only `llc-19` (LLVM static compiler), and only for the optional native-compilation script `compile-file.sh`. JIT execution (`interpret` subcommand, integration test runner) goes through Inkwell's ExecutionEngine in-process — no `lli` binary needed.

## CLI Usage

```bash
# Tokenize and print tokens
cargo run -- lex <FILE>

# Parse and print AST
cargo run -- parse <FILE>

# Compile (emit IR by default; `-e exe` is accepted by the CLI but NOT yet
# implemented — it hard-errors. Use compile-file.sh for a native binary)
cargo run -- compile <FILE> [-e ir|obj] [-o OUTPUT] [--print] [-O LEVEL] [--verify-each]

# JIT-compile and execute in-process; trailing args become argv[1..]
cargo run -- interpret <FILE> [-O LEVEL] [-- ARGS...]

# Compile to native executable
./compile-file.sh program.ap   # produces program.out
```

Every subcommand also takes `-D NAME[=VALUE]` (inject a define, repeatable),
`-I DIR` (module search root, repeatable), and `--target TRIPLE` (defaults to
the host triple; seeds the `OS_*`/`ARCH_*` defines that drive `$ifdef` in
*every* subcommand, and additionally selects the LLVM target machine for
`compile`/`interpret`). `--target` is what decides which per-platform stdlib
backend a build sees — `lib/std/io/linux.ap` vs `lib/std/io/posix.ap` — and is
implemented in `src/target.rs`. Every 32-bit x86 triple (`i386`/`i486`/`i586`/
`i686`) is a supported cross target, seeding `ARCH_I386`.

The `ASPC_<MODE>_FLAGS` environment variable (`<MODE>` = upper-cased subcommand,
e.g. `ASPC_COMPILE_FLAGS`) is shell-split and spliced in ahead of the real
argv before parsing, so project-wide flags like `-I lib` or a fixed `--target`
need not be retyped; an explicit command-line flag still wins.

Programs that `$import std/...` need `-I lib`:

```bash
cargo run -- interpret -I lib demos/hello.ap
```

## Library Exports

`src/lib.rs` re-exports every module — `lexer`, `preprocessor`, `parser`, `symbol`,
`typechecker`, `codegen`, plus the supporting `target`, `scope`, and `asm` — so
integration tests and external consumers can invoke the pipeline directly without
going through the CLI. `tests/integration_tests.rs` is the worked example:
`Preprocessor` → `Parser` → `TypeChecker` → `CodeGenerator`, with `TargetSpec`
supplying the target.

## Critical Design Decisions

1. **Signedness is instruction-level**: LLVM types (`i32`, `i64`) carry no signedness. The compiler tracks `LangType::base` (SInt vs UInt) and consults it at every operation site to choose `sdiv`/`udiv`, `sext`/`zext`, `SLT`/`ULT`, etc.

2. **Opaque pointers**: Since LLVM 15, all pointers are `ptr`. `build_load` requires the pointee type as an explicit argument. The compiler stores `llvm_type` alongside every variable pointer for this purpose.

3. **Parse-time symbol table**: The parser builds the symbol table during parsing, enabling type-aware expression parsing (e.g., resolving function return types for call expressions).

4. **Constraint-based type checking**: Rather than checking types inline, the type checker collects constraints in phase 2 and verifies them all in phase 3, allowing multiple errors to be reported at once.

5. **Entry-block alloca hoisting**: All stack allocations are placed in the function's entry block regardless of where the variable is declared, which is required for LLVM's `mem2reg` pass to promote them to SSA registers.
