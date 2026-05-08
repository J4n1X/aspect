# TODOS

| Feature                       | Description                                                                                 | Priority |
|-------------------------------|---------------------------------------------------------------------------------------------|----------|
| Global Variable Assignment    | Handle expressions in the global space such that you can assign them to each other and more | LOW      |
| Function Pointers             | Come up with a syntax and implement function pointers                                       | MEDIUM   |
| Structures                    | Implement structs                                                                           | MEDIUM   |
| Multi-Pass for constants      | Run a multi-pass over the code to replace constant values with their literals.              | LOW      |

## Done

| Feature                    | Description                                                                                 | Priority |
|----------------------------|---------------------------------------------------------------------------------------------|----------|
| Pointer Arithmetic         | Math with pointers, simple as that                                                          | HIGH     |
| Indexing                   | Index things by ptr\[offset\]                                                               | HIGH     |
| Static memory allocation   | Be able to implement memory allocations on the stack or preallocated it (BSS)               | HIGH     |
| General Type Checking      | Implicit casting, pointer arithmetic validation, array-to-pointer decay, const enforcement   | HIGH     |
| Better scope handling      | Unified LocalVar/GlobalVarInfo structs; scope-aware alloca allocation; shadowing works       | HIGH     |
| Constant overflow detection | Literals auto-size to context (`u64 x = 3000000000` → i64); overflow emits compile error    | HIGH     |