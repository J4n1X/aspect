# Aspect Compiler

**A while ago, I thought "Rust is too sophisticated, C is too simple, and I am bored. Why not try to make something that takes many aspects I like and create my own language?" Anyway, that was a massive mistake, and now we're here, trying to deliver on that premise.**

A compiler for the Aspect programming language, written in Rust. Aspect is a statically-typed systems programming language that compiles to LLVM IR.

## Features

- **Statically typed** with explicit type annotations
- **Types - The poor man's classes** which can have functions but are not capable of polymorphism by design.
- **Low-level control** with pointers and manual memory management
- **LLVM backend** for optimized machine code generation
- **Cross-compilation** via `--target`, including freestanding 32-bit x86 (i386) for OS development
- **C interoperability** through extern function declarations
- **Multiple optimization levels** (O0-O3)

## Getting Started

If you wanna learn how to use the language, you can jump right in and read the [Programmer's Handbook](doc/handbook.md)

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
- `-r, --relocation-model <MODEL>` - Relocation model (default: `default`). `pic`
  emits position-independent code (PC-relative / GOT-relative symbol access) for
  shared libraries and PIE executables; `static` forces absolute addressing (e.g.
  for a freestanding kernel image); `dynamic-no-pic` is the macOS-style hybrid;
  `default` lets LLVM choose per the target triple. Mirrors `llc`'s
  `-relocation-model`.
- `--verify-each` - Verify IR after each optimization pass (slower, useful for debugging)
- `-I, --include-dir` - Include a directory for module search
- `-D, --define` - Define a preprocessor value
- `--target <TRIPLE>` - Compilation target triple (default: the host). Selects the
  LLVM target machine and seeds the `OS_*`/`ARCH_*` preprocessor defines. See
  [Cross-Compilation](#cross-compilation) below.

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

# Emit a position-independent object (for a shared library or PIE)
aspc compile program.ap --emit obj --relocation-model pic

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

### Shell Completions

`aspc completions <SHELL>` prints a completion script for `<SHELL>` to stdout.
Supported shells: `bash`, `zsh`, `fish`, `elvish`, `powershell`. Install the
script where your shell looks for completions, then start a fresh shell:

```bash
# bash — needs the `bash-completion` package; file must be named `aspc`.
# Loaded lazily on first `aspc <Tab>`.
mkdir -p ~/.local/share/bash-completion/completions
aspc completions bash > ~/.local/share/bash-completion/completions/aspc

# zsh — drop into a directory on your $fpath (add `fpath=(~/.zfunc $fpath)`
# before `compinit` in ~/.zshrc if it isn't already), file named `_aspc`.
mkdir -p ~/.zfunc
aspc completions zsh > ~/.zfunc/_aspc

# fish — auto-loaded, no extra setup.
aspc completions fish > ~/.config/fish/completions/aspc.fish
```

Completion fires for a command named `aspc`, so the binary must be on `PATH`
under that name. The script is generated from the live CLI, so it always matches
the subcommands and flags of the binary that produced it — regenerate it after
upgrading `aspc`.

## Compiling to Native Executable

To compile a Aspect program to a native executable, proceed as follows:

```bash
# Emit object code directly
aspc compile program.ap -e obj -o program.o

# Link to executable
gcc -o program program.o
```

## Cross-Compilation

`aspc` is a cross-compiler by construction: LLVM emits object code for the
`--target` triple in-process, so producing code for another architecture needs
no separate assembler — only a linker for the final step. The `x86` backend
covers both 64-bit (`x86_64-*`) and 32-bit (`i386`/`i486`/`i586`/`i686-*`)
targets.

### 32-bit x86 (i386)

Every 32-bit x86 triple resolves to a 32-bit ABI (4-byte pointers) and seeds the
`ARCH_I386` preprocessor define, so `$ifdef ARCH_I386` selects i386-specific
code paths. Inline `asm fn`/`naked fn` are supported: the register model is the
32-bit file (`eax`/`ax`/`al`, `esi`/`si`, …), with no `r8`-`r15`, no REX-only
low bytes (`sil`/`dil`/…), and no SSE (`xmm*`), none of which i386 can encode.

```bash
# Freestanding kernel object — no OS, no CRT. Link with your own linker script.
aspc compile kernel.ap --target i386-unknown-none-elf --emit obj -o kernel.o
ld -m elf_i386 -T linker.ld -o kernel.elf kernel.o        # GNU ld
# or: ld.lld -m elf_i386 -T linker.ld -o kernel.elf kernel.o

# Hosted 32-bit Linux binary
aspc compile program.ap --target i686-unknown-linux-gnu --emit obj -o program.o
ld -m elf_i386 -o program program.o                       # if freestanding (_start)
```

`_start` (like `main`) is implicitly public and survives dead-code elimination,
so a freestanding entry point is preserved. `--emit exe` (invoking a linker for
you) is not yet implemented — link the emitted object yourself, as above.

## Environment Variables

### `ASPC_<MODE>_FLAGS`

Flags in `ASPC_<MODE>_FLAGS` — where `<MODE>` is the upper-cased subcommand name
(`ASPC_COMPILE_FLAGS`, `ASPC_INTERPRET_FLAGS`, `ASPC_LEX_FLAGS`,
`ASPC_PARSE_FLAGS`) — are spliced into that subcommand's arguments before parsing.
The value is split with shell-word rules (quotes and escapes honoured). This is
the way to stop repeating project-wide flags like `-I lib` on every invocation:

```bash
export ASPC_COMPILE_FLAGS="-I lib --target i386-unknown-none-elf"
aspc compile kernel.ap --emit obj -o kernel.o     # -I lib and --target applied automatically
```

Injected flags land *before* your command-line arguments, so an explicit flag
still wins for single-valued options (e.g. a command-line `-O3` overrides an
env-supplied `-O1`), while repeatable options such as `-I`/`-D` accumulate. A
value that is not valid shell syntax is reported on stderr and ignored rather
than aborting the run.

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

- [`doc/handbook.md`](doc/handbook.md) — learn the language: syntax, types, structs, memory, the standard library, and common idioms.
- [`doc/compiler/`](doc/compiler/00-overview.md) — how the compiler itself is built, stage by stage.

(DISCLAIMER: The documentation is almost entirely AI generated and may contain errors, as most AI things do.)

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