# TODOS

| Feature                       | Description                                                                                 | Priority |
|-------------------------------|---------------------------------------------------------------------------------------------|----------|
| Global Variable Assignment    | Handle expressions in the global space such that you can assign them to each other and more | LOW      |
| Function Pointers             | Come up with a syntax and implement function pointers                                       | MEDIUM   |
| Structures                    | Implement structs                                                                           | MEDIUM   |
| Multi-Pass for constants      | Run a multi-pass over the code to replace constant values with their literals.              | LOW      |
| General Type Checking         | This includes implicit casting in places where that makes sense.                            | HIGH     |
| Better scope handling         | The way scopes are handled after parsing makes defining variables on the stack too hard.    | HIGH     |

## Done

| Feature                    | Description                                                                                 | Priority |
|----------------------------|---------------------------------------------------------------------------------------------|----------|
| Pointer Arithmetic         | Math with pointers, simple as that                                                          | HIGH     |
| Indexing                   | Index things by ptr\[offset\]                                                               | HIGH     |
| Static memory allocation   | Be able to implement memory allocations on the stack or preallocated it (BSS)               | HIGH     |