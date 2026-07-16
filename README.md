# Aspect Compiler

**A while ago, I thought "Rust is too sophisticated, C is too simple, and I am bored. Why not try to make something that takes many aspects I like and create my own language?" Anyway, that was a massive mistake, and now we're here, trying to deliver on that premise.**

A compiler for the Aspect programming language, written in Rust. Aspect is a statically-typed systems programming language that compiles to LLVM IR.

## Features

- **Statically typed** with explicit type annotations
- **Types - The poor man's classes** which can have functions but are not capable of polymorphism by design.
- **Low-level control** with pointers and manual memory management
- **LLVM backend** for optimized machine code generation
- **C interoperability** through extern function declarations
- **Multiple optimization levels** (O0-O3)

## Requirements

- Rust (2024 edition)
- LLVM 19.1
- For compiling to native executables:
  - `llc` (LLVM static compiler, if you want to compile from llvm IR output)
  - `gcc` or another C compiler/linker

## Building

```bash
cargo build --release
```

The compiler binary will be available as `target/release/aspc`.

## Usage

The compiler provides four main commands:

### Lexical Analysis (Tokenization)

Tokenize a source file and print the tokens:

```bash
aspc lex <FILE>
```

Example:
```bash
aspc lex program.ap
```

### Parsing (AST Generation)

Parse a source file and print the Abstract Syntax Tree:

```bash
aspc parse <FILE>
```

Example:
```bash
aspc parse program.ap
```

### Compilation

Compile a source file and choose what artifact to emit:

```bash
aspc compile <FILE> [OPTIONS]
```

Options:
- `-e, --emit <TARGET>` - Output target: `ir`, `obj`, or `exe` (default: `ir`; `exe` currently unimplemented)
- `-o, --output <OUTPUT>` - Output file path (default depends on emit target)
- `-p, --print` - Print IR to stdout even when writing to a file
- `-O, --optimize <LEVEL>` - Optimization level (0-3, default: 0)
- `--verify-each` - Verify IR after each optimization pass (slower, useful for debugging)
- `-I, --include-dir` - Include a directory for module search
- `-D, --define` - Define a preprocessor value

Examples:
```bash
# Print IR to stdout
aspc compile program.ap

# Write IR to a file
aspc compile program.ap -o program.ll

# Emit an object file (defaults to program.o when -o is omitted)
aspc compile program.ap --emit obj

# Compile with optimization level 2
aspc compile program.ap -O 2

# Compile with O3 and verify after each pass
aspc compile program.ap -O 3 --verify-each

# Write to file and also print to stdout
aspc compile program.ap -o program.ll --print
```

### Interpretation (JIT)

Compile and immediately execute the program in-process via LLVM's JIT — no
intermediate files, no external runtime required:

```bash
aspc interpret <FILE> [-O LEVEL] [-- ARGS...]
```

Options:
- `-O, --optimize <LEVEL>` - Optimization level (0-3, default: 0)
- `-I, --include-dir` - Include a directory for module search
- `-D, --define` - Define a preprocessor value
- Trailing positional arguments are forwarded to the program as `argv[1..]`
  (the source path is used as `argv[0]`). Use `--` to separate them from this
  CLI's own flags.

The program must define `main(u32 argc, u8 **argv) -> i32`. The integer
returned by `main` is reported as the execution result.

Examples:
```bash
# Run with no extra args
aspc interpret program.ap

# Pass args through to main
aspc interpret demos/concat_args.ap -- hello world foo

# Run with optimizations
aspc interpret program.ap -O 2 -- arg1 arg2
```

## Compiling to Native Executable

To compile a Aspect program to a native executable, proceed as follows:

```bash
# Emit object code directly
aspc compile program.ap -e obj -o program.o

# Link to executable
gcc -o program program.o
```

## Example

Here's a simple "Hello World" style program:

```aspect
$import std/io

fn main(u32 argc, u8 **argv) -> i32 {
    const u8 *message = "Hello, Aspect!"
    println(message)
    return 0
}
```

## Running Tests

```bash
cargo test
```

## Documentation

For language syntax and features as well as the function and makeup of the compiler, see the doc directory. (DISCLAIMER: The documentation is almost entirely AI generated and may contain errors, as most AI things do.)

## License

See LICENSE file for details.

## Artificial Intelligence Disclosure

This project uses or used AI for the following purposes:

1. Documentation of the codebase, including the maintaining of the documentation directory
2. The creation of the vscode-aspect syntax highlighting extension and some demos.
3. Assistance in planning of features (implementations are done by hand)
4. Research into required topics for compiler development.
5. Some inline comments to improve readability 
6. The DSL Procedural Macro System for the Parser
7. Commit messages
8. Most tests.
9. Some of the demos. The reasoning for this is that if the AI understands how to write code in my language, a user should be able to as well.

The reasoning for using it in these cases is rather simple: I am lazy, and I hate documenting stuff. 