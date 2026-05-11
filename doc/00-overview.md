# Architecture Overview

TJLB is a statically-typed, low-level programming language compiler written in Rust. It compiles a C-like language to LLVM IR via the [Inkwell](https://github.com/TheDan64/inkwell) safe Rust bindings. The binary is named `tjlb-parser` (historical name) and the library crate is `tjlb_rust`.

## Project Structure

```
tjlb-rust/
├── src/
│   ├── main.rs              # CLI entry point (clap-derived)
│   ├── lib.rs               # Library root — re-exports all modules
│   ├── lexer/               # Tokenization
│   │   ├── scanner.rs       # Lexer implementation
│   │   ├── tokens.rs        # Token and type definitions
│   │   └── errors.rs        # Lexer error types
│   ├── parser/              # AST construction
│   │   ├── ast.rs           # AST node types
│   │   ├── expressions.rs   # Expression parsing (Pratt-style)
│   │   ├── statements.rs    # Statement parsing
│   │   ├── types.rs         # Re-export of lexer types
│   │   └── errors.rs        # Parser error types
│   ├── symbol/              # Symbol table
│   │   └── table.rs         # Scoped variable/function symbol table
│   ├── typechecker/         # Semantic validation
│   │   ├── checker.rs       # Constraint-based type checker
│   │   ├── types.rs         # Constraint definitions
│   │   └── errors.rs        # Type error types
│   └── codegen/             # LLVM IR emission
│       ├── generator.rs     # Core IR generation logic
│       ├── types.rs         # LangType → LLVM type translation
│       └── errors.rs        # Codegen error types
├── tests/
│   ├── integration_tests.rs # Integration test suite
│   └── programs/            # .tjlb test programs
├── doc/                     # This documentation
├── LANGUAGE.md              # Full language reference
├── Cargo.toml               # Dependencies and features
└── compile-file.sh          # Native compilation script
```

## Compilation Pipeline

The compiler runs a 5-stage pipeline:

```
Source (.tjlb)
    │
    ▼
┌──────────┐   Vec<Token>
│  Lexer   │ ──────────────┐
└──────────┘               │
                           ▼
                     ┌──────────┐   Program (AST)
                     │  Parser  │ ──────────────┐
                     └──────────┘               │
                                                ▼
                                          ┌───────────────┐   Ok / Vec<Error>
                                          │ Type Checker   │ ──────────────┐
                                          └───────────────┘               │
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

### Stage 2: Parsing (`src/parser/`)

Recursive descent parser with precedence climbing for expressions. Produces:
- `Program` containing `Vec<Function>`, `Vec<GlobalVar>`, and `Vec<String>` (string literal table)
- Builds the `SymbolTable` during parsing (not a separate pass)
- Desugars several constructs at parse time (compound assignments, unary minus, elif, array indexing)

See [02-parser.md](02-parser.md) and [03-ast.md](03-ast.md).

### Stage 3: Type Checking (`src/typechecker/`)

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

### Stage 4: Code Generation (`src/codegen/`)

Emits LLVM IR via Inkwell (pinned to LLVM 19.1). Key design:
- Two-pass function compilation: declare all signatures first, then generate bodies (enables forward references)
- All `alloca` instructions hoisted to the entry block (required for `mem2reg`)
- Opaque pointers throughout (`ptr` type for all pointer/array variables)
- Signedness tracked via `LangType::base` and applied at instruction selection time

See [06-codegen.md](06-codegen.md).

### Stage 5: Optimization (optional)

Runs LLVM's new pass manager with pipeline strings `default<O0>` through `default<O3>`.

## Dependencies

| Crate | Purpose |
|-------|---------|
| `inkwell` 0.7.1 (feature `llvm19-1`) | Safe Rust bindings to LLVM 19.1 |
| `clap` 4.0 (feature `derive`) | CLI argument parsing |
| `anyhow` 1.0 | Contextual error handling |
| `thiserror` 2.0.17 | Derive for custom error enums |

External tools required on `PATH`: `lli-19` (LLVM interpreter/JIT) and `llc-19` (LLVM static compiler).

## CLI Usage

```bash
# Tokenize and print tokens
cargo run -- lex <FILE>

# Parse and print AST
cargo run -- parse <FILE>

# Compile to LLVM IR
cargo run -- compile <FILE> [-o OUTPUT] [--print] [-O LEVEL]

# Compile to native executable
./compile-file.sh program.tjlb   # produces program.out
```

## Library Exports

`src/lib.rs` re-exports all modules (`lexer`, `parser`, `symbol`, `codegen`, `typechecker`) so integration tests and external consumers can invoke the pipeline directly without going through the CLI.

## Critical Design Decisions

1. **Signedness is instruction-level**: LLVM types (`i32`, `i64`) carry no signedness. The compiler tracks `LangType::base` (SInt vs UInt) and consults it at every operation site to choose `sdiv`/`udiv`, `sext`/`zext`, `SLT`/`ULT`, etc.

2. **Opaque pointers**: Since LLVM 15, all pointers are `ptr`. `build_load` requires the pointee type as an explicit argument. The compiler stores `llvm_type` alongside every variable pointer for this purpose.

3. **Parse-time symbol table**: The parser builds the symbol table during parsing, enabling type-aware expression parsing (e.g., resolving function return types for call expressions).

4. **Constraint-based type checking**: Rather than checking types inline, the type checker collects constraints in phase 2 and verifies them all in phase 3, allowing multiple errors to be reported at once.

5. **Entry-block alloca hoisting**: All stack allocations are placed in the function's entry block regardless of where the variable is declared, which is required for LLVM's `mem2reg` pass to promote them to SSA registers.
