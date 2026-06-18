# TODOS

| Feature                       | Description                                                                                 | Priority |
|-------------------------------|---------------------------------------------------------------------------------------------|----------|
| Struct by-value ABI           | SysV/Win64 aggregate classification so structs can cross the `extern`/C boundary by value (small-struct-in-registers, sret>16 on SysV; ≤8B-in-register vs by-ref on Win64). Until then `extern` by-value struct params/returns are rejected. | LOW |
| Direct syscalls (no libc)     | Skip libc altogether and emit `syscall`/`int 0x80` directly so `interpret` / linked binaries run with no `-lc` dependency. Linux: well-documented numbers + register conventions per arch (x86-64 / aarch64 / riscv64), each `extern fn read/write/openat/close/exit_group/...` becomes a tiny inline-asm body. Windows: hard — syscall numbers are unstable across builds; would need to go via `ntdll!Nt*` thunks instead. Start Linux-only; gate Windows behind an explicit feature flag once syscall stubs land. | LOW |
| Noalias handling              | This could also improve optimizations, by reducing the amount of moves and memory.          | LOW      |
| Implement Bash Completion     | This can be done for free with clap-complete. File stored to ~/.bash_completion.d/          | LOW      |


## Done

| Feature                       | Description                                                                                 | Priority |
|-------------------------------|---------------------------------------------------------------------------------------------|----------|
| Pointer Arithmetic            | Math with pointers, simple as that                                                          | HIGH     |
| Indexing                      | Index things by ptr\[offset\]                                                               | HIGH     |
| Static memory allocation      | Be able to implement memory allocations on the stack or preallocated it (BSS)               | HIGH     |
| General Type Checking         | Implicit casting, pointer arithmetic validation, array-to-pointer decay, const enforcement  | HIGH     |
| Better scope handling         | Unified LocalVar/GlobalVarInfo structs; scope-aware alloca allocation; shadowing works      | HIGH     |
| Constant overflow detection   | Literals auto-size to context (`u64 x = 3000000000` → i64); overflow emits compile error    | HIGH     |
| Better literal casting        | Literals should take any shape, at any time                                                 | MEDIUM   |
| Source file for typecheck err | Print information on what source file the type checker warning or error came from           | LOW      |
| Global Variable Assignment    | Handle expressions in the global space such that you can assign them to each other and more | LOW      |
| Type-annotating typechecker   | Bidirectional checker stamps `expr_type` during checking; codegen reads final widths. See `doc/solved/Bidirectional-Typechecker.md` | LOW      |
| Join Parser and Codegen Table | Generic `ScopeStack<T>` (`src/scope.rs`) now backs the parser, typechecker, and codegen scopes | MEDIUM   |
| Visitor System For Kinds      | Resolved by design: `ExprKind`/`StatementKind` dispatch sites are exhaustive `match`es, so the compiler already flags every site when a variant is added. A uniform visitor was rejected as net-negative (passes are too divergent). | MEDIUM   |
| Define overflow behavior      | Signed overflow is UB → signed `add`/`sub`/`mul` carry `nsw`; unsigned stays wrapping. (`src/codegen/value_emitter.rs`) | LOW      |
| Introduce boolean             | `bool` type: i1 value, i8 storage; comparisons/`&&`/`\|\|`/`!` yield it; loads tagged `!range !{i8 0, i8 2}`. Also added `inbounds` on indexing/pointer GEPs. | LOW      |
| Type-Structs (aliases+structs)| `alias`/`type` system: `TypeBase::Struct(u32)` + unified `ModuleSymbols`. Methods (`fn name(this, ...)` instance / `Type.name(...)` static / `const fn` const-receiver), encapsulation (`public` opt-in, hidden default), struct by-value via sret/byval. See `doc/plans/Struct-System.md`. | MEDIUM |
| Function pointers             | `fn(args) -> R` as a type; `&func` / bare `func` produces a function-pointer value; indirect call through any expression of fn-ptr type via `build_indirect_call`. Composes with type-struct fields (vtables) and arrays via parens-grouped types `(fn(...) -> R)[N]`. | MEDIUM |
| Parens-grouped types          | `(T)[N]` / `(T)*` — explicit grouping that stops the lexer's greedy `T[N]`/`T*` folding. Unlocks "array of fn-pointers", "array of pointers", "pointer to fn-pointer". | LOW |
| Preprocessor (`$include`)     | `$include "path"` splices another source file's tokens in (recursive, include-once on canonical path, resolved relative to the directive's file). Lives in `src/preprocessor/`; new directives slot in as sibling modules. See `doc/09-syntax-reference.md` § Preprocessor. | MEDIUM |
| `sizeof(T)`                   | Compile-time `u64` byte size of any type (primitive, pointer, function pointer, array, type-struct with padding). Lowered to a single constant at codegen via the target data layout. Eliminates the need for `alloc_<type>_array` helpers in the stdlib — `malloc(n * sizeof(T))` is the idiom now. | LOW |