# The Aspect Handbook

A guide to *writing* Aspect programs. If you want to know how the compiler
works internally, see [`doc/compiler/`](compiler/00-overview.md) instead —
this handbook stays entirely on the user side of the fence: syntax,
semantics, the standard library, and the idioms real Aspect code uses.

Aspect is a small, statically-typed, low-level language that compiles to
LLVM IR. Think "a C-like language with a few opinions": explicit widths,
explicit casts, manual memory management, no garbage collector, no
exceptions, no generics (yet) — and a handful of features C doesn't have,
like type-structs with encapsulation, function-pointer types, and
expression-position blocks.

This handbook teaches the language top to bottom. For the exhaustive,
formal version of everything here — full grammar, precedence table, every
edge case — see [`doc/compiler/09-syntax-reference.md`](compiler/09-syntax-reference.md).
It is referenced throughout rather than duplicated.

---

## Contents

1. [Getting started](#1-getting-started)
2. [A whirlwind tour](#2-a-whirlwind-tour)
3. [Lexical basics](#3-lexical-basics)
4. [Types](#4-types)
5. [Operators and expressions](#5-operators-and-expressions)
6. [Variables and scope](#6-variables-and-scope)
7. [Control flow](#7-control-flow)
8. [Functions](#8-functions)
9. [Type-structs](#9-type-structs)
10. [Pointers, arrays, and memory](#10-pointers-arrays-and-memory)
11. [The preprocessor](#11-the-preprocessor)
12. [Modules](#12-modules)
13. [Standard library tour](#13-standard-library-tour)
14. [Idioms and patterns](#14-idioms-and-patterns)
15. [Common pitfalls](#15-common-pitfalls)
16. [Where to go next](#16-where-to-go-next)

---

## 1. Getting started

### Requirements

- Rust (2024 edition) to build the compiler itself
- LLVM 19.1 (Aspect's codegen is pinned to it via Inkwell's `llvm19-1` feature)
- `gcc` (or another linker) if you want native executables

### Building

```bash
cargo build --release
```

This produces the compiler binary at `target/release/aspc`. Everything
below assumes `aspc` is that binary (or `cargo run --` during development).

### Hello, world

```aspect
$import std/io

fn main(u32 argc, u8 **argv) -> i32 {
    println("Hello, Aspect!")
    return 0
}
```

Save this as `hello.ap`. Every Aspect entry point has this exact
signature: `fn main(u32 argc, u8 **argv) -> i32`. There is no other
supported shape for `main` — the JIT interpreter and the test harness
both call into it directly.

The `$import std/io` line pulls in the standard library's print
functions. Because the stdlib lives in this repo under `lib/`, you need
to tell the compiler where to find it with `-I lib` whenever a program
imports anything under `std/`.

### Running it

Three ways to execute a program, roughly in order of "fastest to try"
to "what you'd ship":

```bash
# JIT-compile and run in-process — no intermediate files
aspc interpret -I lib hello.ap

# Emit LLVM IR (or an object file) and inspect it
aspc compile -I lib hello.ap --print
aspc compile -I lib hello.ap -e obj -o hello.o

# Compile straight to a native executable (uses llc-19 + gcc)
./compile-file.sh hello.ap   # produces hello.out (passes -I lib itself)
```

`interpret` forwards trailing arguments to the program as `argv[1..]`:

```bash
aspc interpret -I lib demos/concat_args.ap -- hello world
```

Two more subcommands exist for looking under the hood while learning the
language — useful when you're not sure how something parses:

```bash
aspc lex hello.ap     # print the token stream
aspc parse hello.ap    # print the AST
```

---

## 2. A whirlwind tour

One program, annotated, touching most of the language at once. Don't
worry about absorbing every detail here — each piece gets its own section
below.

```aspect
$import std/io

# A type-struct: a named aggregate that can carry methods.
type Point {
    public i32 x    # `public` opts a field into external access;
    public i32 y    # fields are private by default.

    # An instance method — first param is the bare identifier `this`.
    const fn magnitude_squared(this) -> i32 {
        return this.x * this.x + this.y * this.y
    }

    # A static method (no `this`) — called as Point.origin().
    public fn origin() -> Point {
        return Point { x = 0, y = 0 }
    }
}

fn distance_from_origin(Point p) -> f64 {
    i32 mag2 = p.magnitude_squared()
    return sqrt_placeholder(mag2 as f64)
}

# Newton's method square root — just to keep this example self-contained.
fn sqrt_placeholder(f64 x) -> f64 {
    if x <= 0.0 { return 0.0 }
    f64 guess = x
    i32 i = 0
    while i < 20 {
        guess = 0.5 * (guess + x / guess)
        i += 1
    }
    return guess
}

fn main(u32 argc, u8 **argv) -> i32 {
    Point p = Point { x = 3, y = 4 }
    Point origin = Point.origin()

    f64 d = distance_from_origin(p)
    print("distance: ")
    println_f64(d)              # 5.000000

    i32[3] scores = {90, 85, 100}
    i32 total = 0
    for (i32 i = 0; i < 3; i += 1) {
        total += scores[i]
    }
    print("total: ")
    println_i32(total)          # 275

    return 0
}
```

Run it with `aspc interpret -I lib tour.ap`. What's on display: modules
(`$import`), a type-struct with private/public fields and both method
kinds, struct literals, explicit casts, `while`/`for` loops, a stack
array with a list initializer, and the standard library's print helpers.

---

## 3. Lexical basics

### Comments

```aspect
# a line comment — runs to the end of the line

#- a block comment
   spans multiple lines,
   closed explicitly -#
```

A comment starting with exactly `#-` is a block comment; anything else
starting with `#` is a line comment. Block comments don't nest.

### Statement terminators

A newline ends a statement, exactly like a semicolon — the two are
interchangeable:

```aspect
i32 x = 1
i32 y = 2;   i32 z = 3   # semicolons let you fit several on one line
```

The corollary: **expressions cannot span multiple lines**.

```aspect
# ERROR — parsed as two separate (broken) statements
i32 result = a
           + b

# OK — the whole expression is on one line
i32 result = a + b
```

### Identifiers and keywords

Identifiers are `[A-Za-z_][A-Za-z0-9_]*`. Reserved keywords:

```
fn  extern  const  type  struct  alias  public  sizeof
while  if  else  elif  for  switch
break  continue  as  return
true  false
```

`struct` and `switch` are reserved for future use — today, type-structs
are declared with `type`, not `struct`, and there is no `switch`
statement; `if`/`elif`/`else` is the only branching construct.

---

## 4. Types

### Primitives

| Type | Meaning | Size |
|---|---|---|
| `i8` `i16` `i32` `i64` | signed integers | 1/2/4/8 bytes |
| `u8` `u16` `u32` `u64` | unsigned integers | 1/2/4/8 bytes |
| `f32` `f64` | floating point | 4/8 bytes |
| `bool` | `true`/`false` | 1 byte storage, `i1` value |
| `u0` | void — return-type only, never a value | — |

There's no distinct `char` type — bytes are `u8`, and string literals are
byte pointers (`u8*`), just like C.

### Literals

```aspect
i32 dec = 42
i32 hex = 0xFF
i32 bin = 0b10110101
f64 pi  = 3.14159      # float literals need digits on BOTH sides of `.`
u8 *s   = "hello\n"    # \n \r \t \\ \" are the supported escapes
bool ok = true
```

There's no negative literal syntax — `0 - 128` is how you write a
negative number; unary minus is sugar for exactly that.

### Signedness and widening

Signedness is part of the type, not a runtime property, and it matters:
shifting a negative `i32` right sign-extends, shifting a `u32` right
zero-fills. Pick the unsigned width when you're doing bit manipulation
that shouldn't care about sign (see `demos/types.ap` for a worked
example with packed RGBA colour channels).

Implicit integer coercion is gated on **width alone, not signedness** —
if the target type is at least as wide as the source, no cast is needed,
whether or not the two sides agree on sign:

```aspect
i32 a = 5
i64 b = a         # i32 -> i64: wider, same sign — implicit
u64 c = a         # i32 -> u64: wider, DIFFERENT sign — also implicit
u32 d = a         # i32 -> u32: SAME width, different sign — implicit too
```

That last line silently reinterprets the bit pattern (a negative `i32`
assigned to a same-width `u32` comes out as the huge unsigned value you'd
expect from two's-complement, with no diagnostic at all — there is
currently no compiler warning for any of this, implicit or not).
Narrowing — a strictly smaller target width — is the one case that's
always a hard type error without an explicit cast:

```aspect
i64 big = 300
i8 small = big as i8    # narrowing: `as` is mandatory
```

### Casts

`as` is the only cast operator, and it binds very tightly — tighter than
any binary operator:

```aspect
x + 1 as i64   # parses as x + (1 as i64), NOT (x + 1) as i64
```

It covers numeric conversions, pointer-to-integer / integer-to-pointer,
and pointer-to-pointer reinterpretation:

```aspect
i32 n = 65
u8 c = n as u8
f64 f = n as f64
u0* raw = malloc(16)
i32* p  = raw as i32*
```

`as` is **mandatory** for: narrowing an integer to a smaller width,
converting between an integer and a float (either direction — there's no
implicit `int → float` promotion, unlike C), and converting a pointer
to/from an integer. It's **legal but not required** everywhere else `as`
appears above — e.g. `raw as i32*` is one valid way to sharpen a `u0*`,
but plain `i32* p = raw` already works without it (see
[Pointers](#pointers) below).

### `const`

`const` marks a value (or the pointee of a pointer) as read-only. It
must immediately precede the base type:

```aspect
const i32 LIMIT = 100
const u8 *label = "readonly bytes"
```

### Pointers

`T*` is a pointer to `T`; `T**` a pointer to a pointer, and so on.
`&expr` takes an address, `*expr` dereferences.

```aspect
i32 x = 10
i32 *p = &x
*p = 20          # x is now 20
```

Two pointers of the **same depth** coerce into one another implicitly
regardless of pointee type — the type checker only compares pointer
depth here, not what's on the other side of it. This isn't limited to
`u0*`; it's true of any two sized pointers:

```aspect
i32 *p = &x
u8 *q = p        # i32* -> u8*, no cast — same depth, any pointee type
```

In practice this means the compiler won't stop you from assigning the
"wrong" pointer type as long as the depth matches — worth remembering
when a pointer holds a surprising value; see [§15](#15-common-pitfalls).

### The opaque pointer `u0*`

`u0*` is Aspect's `void*` — the universal untyped pointer, used
throughout the standard library for allocators and type-erased APIs
(`malloc`, `sort_bytes`'s comparator arguments, and so on).

- Beyond the general same-depth coercion above, `u0*` (depth exactly 1)
  additionally bridges **any** pointer depth — a `Point*`, `Point**`, or
  deeper all coerce to and from `u0*` directly, no cast needed. That's
  the one thing genuinely unique to `u0*`; nothing else crosses depths
  implicitly.
- It is *opaque*: you cannot dereference it, subscript it, or do pointer
  arithmetic on it. Cast to a sized pointer first (`p as Point*`, or
  `p as u8*` to treat it as raw bytes).
- Null checks work directly: `p == null`, `if p { ... }`, `!p`.
- `u0**` (depth 2) does *not* get the depth-crossing treatment `u0*`
  gets — but it's an ordinary pointer otherwise, so it coerces with any
  other depth-2 pointer (`Point**`, `i32**`, …) under the general
  same-depth rule above. What actually makes `u0*` special is only its
  *own* opacity — `u0**` can be dereferenced fine; you just get a `u0*`
  back, which then can't be dereferenced further without a cast.

```aspect
u0 *raw = malloc(sizeof(i32) * 10)   # untyped allocation
i32 *xs = raw                        # implicit, depth-crossing coercion
xs[0] = 42                           # fine now that it's a sized pointer
free(xs)                             # any pointer coerces back into free's u0*
```

### Arrays

Two different things share bracket syntax — don't confuse them:

```aspect
u8[256] buf          # STACK array: 256 bytes, allocated at compile time
u8 *heap = u8[256]   # HEAP allocation: same size, but as an expression —
                      # equivalent to malloc(256), returns u8*
```

A preallocated array decays to a pointer in any expression context — pass
it directly to a function expecting `T*`, no `&` and no cast needed.

### `sizeof`

A compile-time `u64` byte size, valid for any type — primitives,
pointers, function pointers, arrays, and type-structs (padding
included):

```aspect
i32* xs = malloc(n * sizeof(i32))
Point* p = malloc(sizeof(Point))
```

Parentheses are required: `sizeof(T)`, not `sizeof T`.

### `alias`

A transparent compile-time typedef — no distinct type identity, just a
name for an existing type:

```aspect
alias byte u8
alias Callback fn(i32) -> u0

byte b = 255
```

Aliases and type-structs may reference each other and be used before
their textual declaration point — see [§8 Functions](#forward-references-and-mutual-recursion) for why order doesn't matter.

---

## 5. Operators and expressions

### Precedence (low to high)

| Operators | |
|---|---|
| `\|\|` | logical OR |
| `&&` | logical AND |
| `== != < > <= >=` | comparison (result is `bool`) |
| `\|` | bitwise OR |
| `^` | bitwise XOR |
| `&` | bitwise AND |
| `<< >>` | shifts |
| `+ -` | additive (also pointer arithmetic) |
| `* / %` | multiplicative |
| `as` | cast — binds tighter than any binary operator |
| `- ! ~ & *` (unary) | negation, logical/bitwise NOT, address-of, deref |
| `() []` | call, subscript — tightest |

Full details (associativity, exact grammar) are in
[09-syntax-reference.md § Operator precedence](compiler/09-syntax-reference.md#operator-precedence).

Comparisons and `&&`/`\|\|`/`!` produce `bool`. Everything else preserves
the type of the left operand (after any widening).

### Value blocks

A `{ ... }` in *expression* position, where every path terminates in
`return <expr>`, evaluates to that value — a block-as-expression:

```aspect
i32 clamped = {
    if x > 100 {
        return 100
    }
    return x
}
```

Rules worth knowing:

- A `return` inside a value block binds to the *innermost* value block,
  not the enclosing function — you can't early-return the function from
  inside one.
- Every path must end in `return <expr>` — a bare `return` (no value) is
  an error here, and loops never satisfy the rule (even an obviously
  terminating `while true { return 1 }` is rejected; the checker doesn't
  try to prove loop behaviour).
- `break`/`continue` pass straight through to the enclosing *loop* —
  value blocks are transparent to them.
- They execute statements, so they're never compile-time constants:
  global initializers can't use them.
- Disambiguation from list initializers (`{1, 2, 3}`) is automatic: a
  brace expression that parses as a comma-separated list *is* a list;
  anything else — which must contain a `return` — is a value block. A
  `{` in statement position is always a plain block, never a value block.

---

## 6. Variables and scope

```aspect
fn main(u32 argc, u8 **argv) -> i32 {
    i32 x = 10
    {
        i32 x = 20    # shadows the outer x
        x += 5        # inner x = 25
    }
    return x          # outer x = 10
}
```

- One global scope holds functions and global variables.
- Each function body, and each `{ }` block, opens a new child scope.
- A `for` loop header shares its scope with the loop body — a variable
  declared in `for-init` is visible inside the loop.
- Shadowing is allowed; the outer binding reappears once the inner scope
  closes.

Global variables:

```aspect
i32 counter = 0
const f64 GRAVITY = 9.81
```

Global-variable **initializer expressions** are the one place where
declaration order still matters (everything else — function calls,
struct/alias references, method lookups — is order-independent; see
[§8](#forward-references-and-mutual-recursion)). An initializer can only
see globals defined earlier in the same file.

---

## 7. Control flow

`if`/`elif`/`else`, `while`, and `for` are the full set. **Braces are
always required** — there is no single-statement-without-braces form.

```aspect
if x > 0 {
    positive()
} elif x < 0 {
    negative()
} else {
    zero()
}

while n > 0 {
    n -= 1
}

for (i32 i = 0; i < 10; i += 1) {
    process(i)
}

for (;;) {          # all three clauses are optional
    if done() { break }
}
```

`break` and `continue` work as expected inside `while`/`for`, and pass
through value blocks to reach the enclosing loop.

One nuance: inside a `for` header, only `;` separates the three clauses
— a newline there would close the header early, so keep `for (...)` on
one line.

---

## 8. Functions

```aspect
fn add(i32 a, i32 b) -> i32 {
    return a + b
}

fn log(u8 *msg) -> u0 {     # u0 return type = "no value" (may be omitted)
    println(msg)
}

fn log2(u8 *msg) {          # `-> u0` can be dropped entirely
    println(msg)
}
```

### `extern fn` — calling into C

```aspect
extern fn puts(u8 *s) -> i32
extern fn malloc(u64 size) -> u0*
```

No body, just a signature — the linker (or, under `interpret`, `dlopen`
against the host process) resolves it. This is how the entire `std/c/*`
layer of the stdlib is written; see [§13](#13-standard-library-tour).

### Forward references and mutual recursion

Declaration order doesn't matter for functions, type-structs, methods,
or aliases: the parser registers every top-level signature in a first
pass before parsing any bodies, so a function can call something declared
later in the file, methods can call later methods or free functions, and
mutual recursion works directly — no forward-declaration dance needed:

```aspect
fn is_even(i32 n) -> bool {
    if n == 0 { return true }
    return is_odd(n - 1)      # is_odd is defined below — this is fine
}

fn is_odd(i32 n) -> bool {
    if n == 0 { return false }
    return is_even(n - 1)
}
```

(The sole exception, noted above, is global-variable *initializer
expressions* — those still only see earlier globals.)

### Function pointers

`fn(T1, T2) -> R` is a type — and it *is* a pointer, there's no separate
non-pointer function type. A bare function name, or `&name`, produces a
value of that type:

```aspect
alias BinOp fn(i32, i32) -> i32

fn add(i32 a, i32 b) -> i32 { return a + b }
fn mul(i32 a, i32 b) -> i32 { return a * b }

fn apply(BinOp op, i32 a, i32 b) -> i32 {
    return op(a, b)     # indirect call — regular call syntax
}

fn main(u32 argc, u8 **argv) -> i32 {
    return apply(&add, 2, 3) + apply(mul, 2, 3)   # `&add` and bare `mul` both work
}
```

Dispatch tables — an array of function pointers indexed by an opcode or
tag — are a common pattern; see `demos/vm.ap` for a small bytecode
interpreter built exactly that way. Arrays of function pointers need the
parenthesised-type form to keep the lexer from folding brackets onto the
wrong thing:

```aspect
(fn(i32) -> i32)[3] table = {&add_one, &sub_one, &double}
i32 result = table[1](10)
```

There are no closures — a function pointer only ever names a free
function (or, informally, a static method), never a bound instance. See
`demos/wordfreq.ap` for the usual workaround (a global variable the
callback writes into).

---

## 9. Type-structs

`type Name { ... }` declares a named aggregate — Aspect's answer to
structs/classes, deliberately without inheritance or polymorphism ("the
poor man's classes," per the README).

```aspect
type Point {
    public i32 x
    public i32 y

    const fn magnitude_squared(this) -> i32 {
        return this.x * this.x + this.y * this.y
    }
}
```

### Fields: private by default

Fields (and methods) are **private unless marked `public`**. From
outside the type's own methods, a private field can't be read, assigned,
or named in a struct literal, and a private method can't be called.

### Struct literals

```aspect
Point p = Point { x = 3, y = 4 }
```

Every field must be named — no partial initialization, no defaults. If
a type has any private field, an external struct literal for it is
impossible by construction: the only way to build one is through the
type's own `public` static factory methods. This is the encapsulation
pattern the whole stdlib uses (`String.from_cstring`, `VecI32.new`,
`MapStrI64.with_capacity`, …).

### Methods: instance vs. static

A method whose first parameter is the bare identifier `this` (no type
annotation) is an **instance** method; anything else is **static**.

```aspect
type Counter {
    i32 value    # private

    public fn new() -> Counter {          # static — no receiver
        return Counter { value = 0 }
    }

    public fn bump(this) -> i32 {         # instance — implicit &Counter receiver
        this.value = this.value + 1       # compound assignment only targets
        return this.value                 # a plain variable, not a field — see §15
    }

    public const fn peek(this) -> i32 {   # const instance method
        return this.value
    }
}

fn main(u32 argc, u8 **argv) -> i32 {
    Counter c = Counter.new()      # static call: Type.method(...)
    c.bump()                       # instance call: value.method(...)
    return c.peek()
}
```

The two call forms are strict and not interchangeable: `Counter.bump(&c)`
(UFCS-style) and `c.new()` are both rejected at parse time with a
diagnostic pointing at the mismatch.

A value receiver auto-references at the call site (`obj.m()` becomes
`m(&obj)` under the hood) — you never write `&` yourself for `this`. A
pointer receiver (`Point* p; p.m()`) is passed through unchanged, and
field access through a single-level pointer auto-derefs too:
`p.x` works whether `p` is a `Point` or a `Point*`.

`const fn` requires `this`, and propagates const through it — any
`this.field = ...` inside a `const fn` is a type error. Use it for
read-only accessors (see `len()`/`c_str()` on `String` in the stdlib).

### Self- and mutually-referential structs

Type names are pre-registered before any bodies are parsed, so a struct
can reference itself or another struct defined later, as long as it's
through a pointer (a struct can't contain itself by value, only C's
usual restriction):

```aspect
type Node {
    public i32 value
    public Node* next     # fine — Node isn't fully defined yet, but a
}                          # pointer to it doesn't need to be
```

---

## 10. Pointers, arrays, and memory

There's no garbage collector and no destructors. Every heap allocation
you make, you free yourself — and every stdlib container that owns
memory (`String`, `VecI32`, `MapStrI64`, …) exposes an explicit
`destroy()` you're expected to call.

### Pointer arithmetic and subscripting

```aspect
i32 *xs = malloc(5 * sizeof(i32))
xs[0] = 10
i32 *second = xs + 1     # pointer arithmetic: `+ 1` moves by sizeof(i32)
*second = 20
free(xs)
```

### List initializers

```aspect
i32[5] full    = {10, 20, 30, 40, 50}   # every slot given
i32[5] empty   = {}                      # all slots zero
i32[5] partial = {7, 8}                  # remaining slots zero
i32[3] derived = {x * 2, x + 10, x * x}  # arbitrary expressions
```

### Heap allocation idiom

```aspect
$import std/mem
$import std/c/stdlib   # if you want raw malloc/free directly

type Point { public i32 x; public i32 y }

fn main(u32 argc, u8 **argv) -> i32 {
    Point *p = malloc(sizeof(Point))
    p.x = 3
    p.y = 4
    free(p)
    return 0
}
```

Since `sizeof(T)` is a compile-time constant, `malloc(n * sizeof(T))` is
the standard idiom — there's no per-type allocator helper to reach for.

### No destructors — call `destroy()`

```aspect
$import std/string
$import std/io          # for println — std/string doesn't re-export it

fn main(u32 argc, u8** argv) -> i32 {
    String s = String.from_cstring("hi")
    s.append_cstring(" there")
    println(s.c_str())
    s.destroy()          # you MUST do this yourself — nothing calls it for you
    return 0
}
```

---

## 11. The preprocessor

A token-level pass runs before parsing and expands line-anchored `$`
directives — chosen instead of `#` because `#` is already comments, and
`@` is reserved for the (planned) metasystem. A `$` must be the first
token on its line.

### Defines

```aspect
$define DEBUG                  # flag define — no value
$define MAX_SIZE 1024          # value = rest of the line, any tokens
$undefine DEBUG                # no-op if not defined
```

Substitution is by identifier token — `u8[MAX_SIZE]` works because the
array size is its own token. There are no function-like macros; that's
left to the (not-yet-built) metasystem. Redefinition without
`$undefine` first is an error, so overridable defaults write a guard:

```aspect
$ifndef MAX_SIZE
$define MAX_SIZE 1024
$endif
```

Compiler-provided defines: `OS_LINUX` / `OS_WINDOWS` / `OS_MACOS`,
`ARCH_X86_64` / `ARCH_AARCH64`, `ASPECT_VERSION_MAJOR` /
`ASPECT_VERSION_MINOR`. The CLI can inject more with `-D NAME[=VALUE]`.

### Conditionals

```aspect
$ifdef OS_LINUX
    extern fn epoll_create1(i32 flags) -> i32
$elseifdef OS_MACOS
    extern fn kqueue() -> i32
$else
    # portable fallback
$endif

$if MAX_SIZE > 4096
    const u64 BUCKETS = 64
$else
    const u64 BUCKETS = 4
$endif
```

`$if` evaluates a constant integer expression over literals, defines,
and `defined(NAME)`. Unlike C, an **undefined identifier in `$if` is an
error**, not a silent zero.

Full directive grammar: [09-syntax-reference.md § Preprocessor](compiler/09-syntax-reference.md#preprocessor).

---

## 12. Modules

Two directives, `$module` and `$import`, form the language's load unit
and visibility boundary.

```aspect
# in lib/std/io/print.ap:
$module std/io
$import std/c/stdio

fn println(u8* s) -> i32 { return puts(s) }
```

```aspect
# in your program:
$import std/io

fn main(u32 argc, u8** argv) -> i32 {
    return println("hello")
}
```

Compile with the search root that holds the library: `aspc interpret app.ap -I lib`.

Key rules to internalize:

- **A module is a set of files**, not a single file — multiple files can
  declare the same `$module`.
- A file with no `$module` belongs to the anonymous root module — fine
  for entry points.
- **`$import` resolution is convention + verification**: `$import std/io`
  looks for `<root>/std/io.ap` (file form) or every `.ap` file directly
  inside `<root>/std/io/` (directory form, non-recursive). Every file it
  loads must declare exactly `$module std/io`, or it's a hard error.
- **Import-once**: importing the same module twice (directly, or in a
  diamond) loads it once.
- **Visibility does not trickle down, and this is enforced**: your file
  sees its own module's symbols plus the symbols of modules it *directly*
  imports — not the transitive closure. If `middle` imports `hidden` and
  you import `middle`, you cannot use `hidden`'s symbols without
  importing `hidden` yourself:

  ```
  error: function 'hidden_value' is defined in module 'hidden', which the
  root module does not import
  ```

- **v1 caveat — flat namespace**: there's no `io.println` qualified
  syntax. Symbol names must be globally unique across everything loaded,
  even across modules that don't see each other.

Full details: [10-modules.md](compiler/10-modules.md).

---

## 13. Standard library tour

Everything lives under `lib/std/`, compiled in with `-I lib`. Because
imports aren't transitive, import exactly the modules whose symbols your
file calls directly — see the note on `std/sort` below for a concrete
example of that rule biting.

| Import | Provides |
|---|---|
| `std/c/stdio`, `std/c/stdlib`, `std/c/string` | Raw `extern fn` libc bindings, at header granularity |
| `std/io` | `print`/`println` for strings and every integer width, plus `f64` |
| `std/mem` | Byte-count allocation wrappers (`alloc_bytes`, `zalloc_bytes`, `free_ptr`) — pair with `sizeof(T)` |
| `std/math` | `min`/`max`/`clamp`/`abs` per width, `gcd`/`lcm`, `ipow`, `isqrt_u64`, `sqrt_f64`, `floor`/`ceil`/`round`, `PI`/`TAU`/`E` |
| `std/rand` | `Rng` (xorshift64\*): `next_u64`, `below`, `range_i64`, `next_f64`, `chance` — deterministic per seed |
| `std/sort` | Type-erased `sort_bytes(base, n, size, cmp)`, stock comparators, typed wrappers `sort_i32`/`sort_i64`/`sort_f64`/`sort_cstr` |
| `std/string` | Growable heap `String` |
| `std/vec` | Dynamic array `VecI32` |
| `std/collections` | `MapStrI64` — FNV-1a hash map, open addressing |

There are no generics, so containers are monomorphized by hand — `VecI32`
is the `i32` instantiation; a `VecU8` or `VecPoint` would be its own
file, mechanically derived by substituting the element type throughout
(see the recipe at the top of `lib/std/vec/vec_i32.ap`).

### Quick reference by module

```aspect
$import std/io
print("no newline")  println("with a newline")
print_i32(n)  println_i32(n)   # also _u32 _i64 _u64 _f64

$import std/math
min_i64(a, b)  max_f64(a, b)  clamp_u64(v, lo, hi)  abs_i64(v)
gcd_u64(a, b)  ipow_u64(base, e)  sqrt_f64(x)

$import std/rand
Rng rng = Rng.seeded(42)
u64 raw = rng.next_u64()
u64 die = rng.below(6) + 1          # 1..=6
if rng.chance(30) { ... }           # ~30% true

$import std/sort
sort_i32(xs, n)                                    # typed convenience
sort_bytes(base, n, sizeof(T), &my_cmp)            # type-erased, any T

$import std/string
String s = String.from_cstring("hi")
s.append_cstring(" there")
println(s.c_str())
s.destroy()

$import std/vec
VecI32 v = VecI32.new()
v.push(10)
i32 last = v.pop()
v.destroy()

$import std/collections
MapStrI64 m = MapStrI64.new()
m.put("apple", 3)
i64 n = m.get_or("apple", 0)
m.for_each(&visit_fn)               # fn(u8* key, i64 value)
m.destroy()
```

`$import std/sort` gives you `sort_bytes`/`sort_i32`/etc. but *not*
`strcmp` — if your own code calls `strcmp` directly, import
`std/c/string` yourself too. This is the non-transitive visibility rule
from §12 in practice.

---

## 14. Idioms and patterns

A few shapes recur throughout the stdlib and demos — worth knowing
because they're the idiomatic way to work around missing language
features (generics, exceptions, closures) rather than signs you're doing
something unusual.

### Type erasure for "generic" algorithms

Without generics, one way to write an algorithm once for every type is
to erase the element type behind `u0*` and a size/comparator passed at
the call site — exactly libc's `qsort` trick:

```aspect
alias Comparator fn(u0*, u0*) -> i32

fn cmp_person_by_age(u0* a, u0* b) -> i32 {
    Person* pa = a as Person*
    Person* pb = b as Person*
    if pa.age < pb.age { return 0 - 1 }
    if pa.age > pb.age { return 1 }
    return 0
}

sort_bytes(crew, 6, sizeof(Person), &cmp_person_by_age)
```

See `lib/std/sort.ap` and `demos/sort_demo.ap`.

### Encapsulation via private fields + factory methods

Because a struct literal must name every field, and private fields can't
be named from outside, a type with any private field can only be
constructed through its own `public` static methods:

```aspect
type Handle {
    u0* raw       # private — nobody outside can touch this directly

    public fn open(u8* path) -> Handle {
        return Handle { raw = do_open(path) }   # only Handle's own code can do this
    }
}
```

This is how `String`, `VecI32`, and `MapStrI64` all guarantee "no
invalid instance can exist" — every field the invariant depends on is
private, and the only doors in are the factories.

### Error handling via result structs

There are no exceptions and no built-in `Option`/`Result`. The idiom is
a small type-struct carrying a value plus an ok/error flag, returned by
value:

```aspect
type EvalResult {
    public i64 value
    public bool ok
}

fn safe_divide(i64 a, i64 b) -> EvalResult {
    if b == 0 {
        return EvalResult { value = 0, ok = false }
    }
    return EvalResult { value = a / b, ok = true }
}

fn main(u32 argc, u8** argv) -> i32 {
    EvalResult r = safe_divide(10, 0)
    if !r.ok {
        println("division failed")
        return 1
    }
    return r.value as i32
}
```

See `demos/calc.ap` for a complete expression evaluator built this way,
including how a sticky error flag on `this` simplifies propagating
failure out of deeply recursive parsing without any of Aspect's
control-flow constructs needing to know about it.

### Dispatch tables

An array of function pointers indexed by a tag, called through the
subscript — the same shape real bytecode interpreters use:

```aspect
(fn(VM*))[OP_COUNT] ops = {&op_halt, &op_push, &op_add, ...}
ops[opcode](vm)
```

See `demos/vm.ap` for a full stack-machine interpreter built this way.

### Double buffering for stateful simulation

For "step the whole state forward" problems (cellular automata,
particle systems), compute the next generation into a second buffer and
swap pointers rather than mutating in place — avoids read-after-write
hazards and is O(1) to swap:

```aspect
public fn swap_with(this, Board* other) {
    u8* tmp = this.cells
    this.cells = other.cells
    other.cells = tmp
}
```

See `demos/life.ap` (Conway's Game of Life on a torus) for the complete
pattern, including wrapping-coordinate neighbour counting.

### No closures — use a global for callback state

Function-pointer values only ever name free functions — there's no
capturing. When a callback (e.g. `MapStrI64.for_each`) needs to
accumulate into something, the accumulator is a global the callback
writes into directly:

```aspect
u8** g_words
u64 g_word_count = 0

fn collect_word(u8* word, i64 count) {
    g_words[g_word_count] = word
    g_word_count += 1
}

freq.for_each(&collect_word)
```

See `demos/wordfreq.ap`.

---

## 15. Common pitfalls

A quick checklist for the mistakes that actually happen while writing
Aspect:

- **Multi-line expressions don't work.** A newline ends the statement
  wherever it is. Keep expressions on one line, or use a value block
  ([§5](#value-blocks)) if you need multiple statements to produce one
  value.
- **Braces are mandatory** on every `if`/`elif`/`else`/`while`/`for`
  body. There is no single-statement-without-braces shorthand.
- **Narrowing always needs `as`.** `i64` → `i32`, `i32` → `i8`,
  pointer ↔ integer — all explicit, no exceptions.
- **Implicit coercion is looser than you'd expect, and silent.**
  Integer coercion is gated on width alone, so `i32 → u32` (same width,
  opposite sign) needs no cast and reinterprets the bit pattern; sized
  pointers of matching depth coerce into each other regardless of
  pointee type (`i32* → u8*` needs no cast either). Neither case
  produces a warning — there is currently no warning mechanism in the
  compiler at all. See [§4](#signedness-and-widening) and
  [§4](#pointers).
- **Compound assignment (`+=` and friends) only targets a plain
  variable** — not a field (`this.x += 1`) and not a subscript
  (`xs[i] += 1`). Both are parse errors ("compound assignment requires a
  variable"); spell it out instead: `this.x = this.x + 1`,
  `xs[i] = xs[i] + 1`.
- **String literals are `u8*`, not `const u8*`.** The type checker
  doesn't currently treat the two as compatible, so a function expecting
  `const u8*` can reject a string literal. Prefer plain (non-`const`)
  pointer parameters for functions meant to take strings.
- **Forgetting `-I lib`.** Any program that `$import`s `std/...` needs
  the stdlib's root on the module search path, or resolution fails.
- **Imports aren't transitive.** Importing a module that itself imports
  something doesn't give you that something — import it directly if you
  call it directly (see [§12](#12-modules)).
- **Global initializers are order-sensitive** even though almost nothing
  else in the language is — an initializer expression only sees globals
  declared earlier in the file.
- **No generics.** Containers and algorithms are either monomorphized by
  hand per type (`VecI32`) or type-erased through `u0*` + a
  size/comparator (`sort_bytes`). Don't look for a template mechanism;
  there isn't one yet (see `TODO.md`).
- **No destructors.** Every stdlib type that owns heap memory needs an
  explicit `.destroy()` call, or it leaks.
- **`switch` isn't implemented.** It's a reserved keyword for future
  use; `if`/`elif`/`else` is the only branching construct today.

---

## 16. Where to go next

- **Formal grammar, precedence, every edge case:**
  [`doc/compiler/09-syntax-reference.md`](compiler/09-syntax-reference.md)
- **Module system in full:** [`doc/compiler/10-modules.md`](compiler/10-modules.md)
- **Runnable, annotated example programs:** [`demos/`](../demos/README.md) —
  start with `hello.ap`, `types.ap`, and `list_init.ap` for a language
  tour, then `life.ap`, `calc.ap`, `vm.ap`, `wordfreq.ap`, and
  `sort_demo.ap` for the idioms in [§14](#14-idioms-and-patterns) applied
  to complete programs.
- **How the compiler itself is built**, if you're curious or want to
  contribute: [`doc/compiler/00-overview.md`](compiler/00-overview.md)
  onward.
- **What's planned but not built yet** (inline assembly, direct
  syscalls, a metasystem for code generation, methods-as-values): `TODO.md`
  at the repository root.
