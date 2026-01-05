# TJLB Language Reference

TJLB is a statically-typed, low-level programming language designed for systems programming. It features explicit type annotations, manual memory management, and compiles to LLVM IR.

## Table of Contents

- [Comments](#comments)
- [Types](#types)
- [Variables](#variables)
- [Functions](#functions)
- [Operators](#operators)
- [Control Flow](#control-flow)
- [Pointers](#pointers)
- [Arrays](#arrays)
- [Type Casting](#type-casting)
- [External Functions](#external-functions)
- [Examples](#examples)

---

## Comments

TJLB supports single-line and multi-line comments:

```tjlb
# This is a single-line comment

#- 
This is a
multi-line comment
-#
```

---

## Types

TJLB uses explicit size-based types:

### Integer Types

| Type | Description | Size |
|------|-------------|------|
| `i8` | Signed 8-bit integer | 1 byte |
| `i16` | Signed 16-bit integer | 2 bytes |
| `i32` | Signed 32-bit integer | 4 bytes |
| `i64` | Signed 64-bit integer | 8 bytes |
| `u8` | Unsigned 8-bit integer | 1 byte |
| `u16` | Unsigned 16-bit integer | 2 bytes |
| `u32` | Unsigned 32-bit integer | 4 bytes |
| `u64` | Unsigned 64-bit integer | 8 bytes |

### Floating Point Types

| Type | Description | Size |
|------|-------------|------|
| `f32` | 32-bit floating point | 4 bytes |
| `f64` | 64-bit floating point | 8 bytes |

### Void Type

| Type | Description |
|------|-------------|
| `u0` | Void type (no value) |

### Type Modifiers

- `const` - Marks a value as constant
- `*` - Pointer type (can be stacked for multi-level pointers)

Examples:
```tjlb
i32 value              # Signed 32-bit integer
const i32 CONSTANT     # Constant signed 32-bit integer
i32 *ptr               # Pointer to i32
const u8 *str          # Pointer to constant u8 (string)
i8 **argv              # Pointer to pointer to i8
```

---

## Variables

### Global Variables

Global variables are declared at the top level of the program:

```tjlb
i32 counter = 100
const i32 MAX_SIZE = 1024
```

### Local Variables

Local variables are declared within functions:

```tjlb
fn example() {
    i32 x = 10
    i32 y = 20
    i32 sum = x + y
}
```

### Variable Assignment

```tjlb
x = 42
counter = counter + 1
```

### Compound Assignment

```tjlb
x += 1      # x = x + 1
x -= 5      # x = x - 5
x *= 2      # x = x * 2
x /= 4      # x = x / 4
x %= 3      # x = x % 3
x &= 0xFF   # x = x & 0xFF
x |= 0x01   # x = x | 0x01
x ^= 0x10   # x = x ^ 0x10
x <<= 2     # x = x << 2
x >>= 1     # x = x >> 1
```

---

## Functions

### Function Declaration

```tjlb
fn function_name(type1 param1, type2 param2) -> return_type {
    # function body
    return value
}
```

### Function Without Return Value

Use `u0` for functions that don't return a value:

```tjlb
fn print_number(i32 n) -> u0 {
    # ... implementation
}

# Or omit the return type entirely for void functions
fn print_number(i32 n) {
    # ... implementation
}
```

### Function Calls

```tjlb
i32 result = add(5, 3)
print_message("Hello")
```

### Main Function

The entry point of a TJLB program:

```tjlb
# Simple main
fn main() -> i32 {
    return 0
}

# Main with command-line arguments
fn main(i32 argc, i8 **argv) -> i32 {
    return 0
}
```

---

## Operators

### Arithmetic Operators

| Operator | Description |
|----------|-------------|
| `+` | Addition |
| `-` | Subtraction |
| `*` | Multiplication |
| `/` | Division |
| `%` | Modulo |

### Comparison Operators

| Operator | Description |
|----------|-------------|
| `==` | Equal |
| `!=` | Not equal |
| `<` | Less than |
| `>` | Greater than |
| `<=` | Less than or equal |
| `>=` | Greater than or equal |

### Logical Operators

| Operator | Description |
|----------|-------------|
| `&&` | Logical AND |
| `\|\|` | Logical OR |
| `!` | Logical NOT |

### Bitwise Operators

| Operator | Description |
|----------|-------------|
| `&` | Bitwise AND |
| `\|` | Bitwise OR |
| `^` | Bitwise XOR |
| `~` | Bitwise NOT |
| `<<` | Left shift |
| `>>` | Right shift |

### Pointer Operators

| Operator | Description |
|----------|-------------|
| `&` | Address-of (reference) |
| `*` | Dereference |

---

## Control Flow

### If Statement

```tjlb
if condition {
    # then block
}

if condition {
    # then block
} else {
    # else block
}
```

### Elif Chain

```tjlb
if x > 10 {
    # ...
} elif x > 5 {
    # ...
} elif x > 0 {
    # ...
} else {
    # ...
}
```

### While Loop

```tjlb
while condition {
    # loop body
}
```

Example:
```tjlb
i32 i = 0
while i < 10 {
    i = i + 1
}
```

### For Loop

```tjlb
for (init; condition; increment) {
    # loop body
}
```

Example:
```tjlb
for (i32 i = 0; i < 10; i += 1) {
    # loop body
}
```

### Break and Continue

```tjlb
while condition {
    if should_skip {
        continue
    }
    if should_stop {
        break
    }
}
```

---

## Pointers

### Declaring Pointers

```tjlb
i32 *ptr           # Pointer to i32
i32 **ptr_to_ptr   # Pointer to pointer to i32
const u8 *str      # Pointer to constant u8
```

### Reference (Address-of)

```tjlb
i32 value = 42
i32 *ptr = &value  # ptr points to value
```

### Dereference

```tjlb
i32 *ptr = &value
*ptr = 100         # Modify value through pointer
i32 x = *ptr       # Read value through pointer
```

### Pointer Arithmetic

```tjlb
i32 *ptr = some_address
ptr = ptr + 1      # Move to next i32 (adds sizeof(i32) bytes)
ptr += 5           # Move forward 5 elements
```

### Passing Pointers to Functions

```tjlb
fn modify(i32 *ptr) {
    *ptr = *ptr + 10
}

fn main() -> i32 {
    i32 value = 32
    modify(&value)
    return value  # Returns 42
}
```

---

## Arrays

### Stack-Allocated Arrays

```tjlb
u8[1024] buffer    # Allocate 1024 bytes on the stack
i32[10] numbers    # Allocate array of 10 i32 values
```

### Array Access (Indexing)

```tjlb
buffer[0] = 65 as u8       # Set first element
i32 first = numbers[0]     # Read first element
buffer[i] = value          # Dynamic indexing
```

### Getting Array Address

```tjlb
u8[256] buffer
u8 *ptr = &buffer as u8*   # Get pointer to array start
```

---

## Type Casting

Use the `as` keyword to cast between types:

```tjlb
i64 big = 1000
i32 small = big as i32

u8 byte = 65
i32 num = byte as i32

i32 *ptr = 0x1000 as i32*  # Cast integer to pointer
i32 addr = ptr as i32      # Cast pointer to integer
```

---

## External Functions

Declare external C functions using `extern`:

```tjlb
extern fn puts(u8 *str) -> u0
extern fn read(i32 fd, u8 *buf, u64 size) -> i64
extern fn strlen(const i8 *str) -> u64
```

These functions can then be called like regular functions:

```tjlb
puts("Hello, World!")
i64 bytes_read = read(0, buffer, 256 as u64)
```

---

## Examples

### Hello World

```tjlb
extern fn puts(u8 *str) -> u0

fn main() -> i32 {
    const u8 *message = "Hello, World!"
    puts(message)
    return 0
}
```

### Fibonacci

```tjlb
fn fib(i32 n) -> i32 {
    if n <= 1 {
        return n
    }
    return fib(n - 1) + fib(n - 2)
}

fn main() -> i32 {
    return fib(10)  # Returns 55
}
```

### String Length

```tjlb
fn strlen(u8 *str) -> i32 {
    i32 counter = 0
    while str[counter] != 0 as u8 {
        counter = counter + 1
    }
    return counter
}
```

### Memory Operations

```tjlb
fn memset(u8 *dst, u64 len, u8 c) -> u0 {
    for (u64 i = 0; i < len; i += 1 as u64) {
        dst[i] = c
    }
}

fn main() -> i32 {
    u8[256] buffer
    memset(&buffer as u8*, 256 as u64, 0 as u8)
    return 0
}
```

### Working with Command-Line Arguments

```tjlb
extern fn puts(u8 *str) -> u0

fn main(i32 argc, i8 **argv) -> i32 {
    if argc > 1 {
        i8 *first_arg = argv[1]
        puts(first_arg as u8*)
    }
    return 0
}
```

### Bitwise Operations

```tjlb
fn main() -> i32 {
    i32 a = 12    # Binary: 1100
    i32 b = 10    # Binary: 1010

    i32 and_result = a & b    # 1000 = 8
    i32 or_result = a | b     # 1110 = 14
    i32 xor_result = a ^ b    # 0110 = 6

    return and_result + or_result + xor_result  # 28
}
```

---

## Notes

- Statements are terminated by newlines (not semicolons, except in for loop headers)
- The language requires explicit type annotations for all variables
- String literals are null-terminated
- Array indexing uses the `ptr[index]` syntax
- The `as` keyword is required for type conversions
