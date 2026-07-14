# TJLB Compiler

A compiler for the TJLB programming language, written in Rust. TJLB is a statically-typed, systems programming language that compiles to LLVM IR.

## Features

- **Statically typed** with explicit type annotations
- **Low-level control** with pointers and manual memory management
- **LLVM backend** for optimized machine code generation
- **C interoperability** through extern function declarations
- **Multiple optimization levels** (O0-O3)

## Requirements

- Rust (2021 edition)
- LLVM 19.1
- For compiling to native executables:
  - `llc` (LLVM static compiler)
  - `gcc` or another C compiler/linker

## Building

```bash
cargo build --release
```

The compiler binary will be available as `target/release/tjlb-parser`.

## Usage

The compiler provides four main commands:

### Lexical Analysis (Tokenization)

Tokenize a source file and print the tokens:

```bash
tjlb-parser lex <FILE>
```

Example:
```bash
tjlb-parser lex program.tjlb
```

### Parsing (AST Generation)

Parse a source file and print the Abstract Syntax Tree:

```bash
tjlb-parser parse <FILE>
```

Example:
```bash
tjlb-parser parse program.tjlb
```

### Compilation

Compile a source file and choose what artifact to emit:

```bash
tjlb-parser compile <FILE> [OPTIONS]
```

Options:
- `-e, --emit <TARGET>` - Output target: `ir`, `obj`, or `exe` (default: `ir`; `exe` currently unimplemented)
- `-o, --output <OUTPUT>` - Output file path (default depends on emit target)
- `-p, --print` - Print IR to stdout even when writing to a file
- `-O, --optimize <LEVEL>` - Optimization level (0-3, default: 0)
- `--verify-each` - Verify IR after each optimization pass (slower, useful for debugging)

Examples:
```bash
# Print IR to stdout
tjlb-parser compile program.tjlb

# Write IR to a file
tjlb-parser compile program.tjlb -o program.ll

# Emit an object file (defaults to program.o when -o is omitted)
tjlb-parser compile program.tjlb --emit obj

# Compile with optimization level 2
tjlb-parser compile program.tjlb -O 2

# Compile with O3 and verify after each pass
tjlb-parser compile program.tjlb -O 3 --verify-each

# Write to file and also print to stdout
tjlb-parser compile program.tjlb -o program.ll --print
```

### Interpretation (JIT)

Compile and immediately execute the program in-process via LLVM's JIT — no
intermediate files, no external runtime required:

```bash
tjlb-parser interpret <FILE> [-O LEVEL] [-- ARGS...]
```

Options:
- `-O, --optimize <LEVEL>` - Optimization level (0-3, default: 0)
- Trailing positional arguments are forwarded to the program as `argv[1..]`
  (the source path is used as `argv[0]`). Use `--` to separate them from this
  CLI's own flags.

The program must define `main(u32 argc, u8 **argv) -> i32`. The integer
returned by `main` is reported as the execution result.

Examples:
```bash
# Run with no extra args
tjlb-parser interpret program.tjlb

# Pass args through to main
tjlb-parser interpret demos/concat_args.tjlb -- hello world foo

# Run with optimizations
tjlb-parser interpret program.tjlb -O 2 -- arg1 arg2
```

## Compiling to Native Executable

To compile a TJLB program to a native executable, you can use the provided script or run the commands manually:

### Using the Script

```bash
./compile-file.sh program.tjlb
```

This will produce `program.out`.

### Manual Compilation

```bash
# Emit object code directly
tjlb-parser compile program.tjlb --emit obj -o program.o

# Link to executable
gcc -o program program.o
```

## Example

Here's a simple "Hello World" style program:

```tjlb
extern fn puts(u8 *str) -> u0

fn main(u32 argc, u8 **argv) -> i32 {
    const u8 *message = "Hello, TJLB!"
    puts(message)
    return 0
}
```

## Running Tests

```bash
cargo test
```

## Documentation

For language syntax and features as well as the function and makeup of the compiler, see the doc directory.

## License

See LICENSE file for details.

## Artificial Intelligence Disclosure

This project uses or used AI for the following purposes:

1. Documentation of the codebase, including the maintaining of the documentation directory
2. The creation of the vscode-tjlb syntax highlighting extension and some demos.
3. Assistance in planning of features (implementations are done by hand)
4. Research into required topics for compiler development.
5. Some inline comments to improve readability 
6. The DSL Procedural Macro System for the Parser
7. Commit messages
8. Most tests.
9. Some of the demos. The reasoning for this is that if the AI understands how to write code in my language, a user should be able to as well.

The reasoning for using it in these cases is rather simple: I am lazy, and I hate documenting stuff. 