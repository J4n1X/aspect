# TODOS

| Feature                       | Description                                                                                 | Priority |
|-------------------------------|---------------------------------------------------------------------------------------------|----------|
| Function Pointers             | Come up with a syntax and implement function pointers                                       | MEDIUM   |
| Structures                    | Implement structs                                                                           | MEDIUM   |
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