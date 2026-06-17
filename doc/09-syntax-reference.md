# TJLB Syntax Reference

Formal grammar and lexical specification for the TJLB language.

---

## Contents

1. [Notation](#notation)
2. [Lexical rules](#lexical-rules)
   - [Whitespace and comments](#whitespace-and-comments)
   - [Statement terminators](#statement-terminators)
   - [Identifiers and keywords](#identifiers-and-keywords)
   - [Types as tokens](#types-as-tokens)
   - [Literals](#literals)
   - [Operators and punctuation](#operators-and-punctuation)
3. [Grammar (EBNF)](#grammar-ebnf)
   - [Top level](#top-level)
   - [Types](#types)
   - [Statements](#statements)
   - [Expressions](#expressions)
4. [Operator precedence](#operator-precedence)
5. [Scoping rules](#scoping-rules)
6. [Notable constraints](#notable-constraints)

---

## Notation

EBNF conventions used below:

| Syntax | Meaning |
|---|---|
| `item?` | optional |
| `item*` | zero or more |
| `item+` | one or more |
| `a | b` | alternation |
| `'...'` | literal token |
| `/* ... */` | informal description |

Angle brackets `<name>` denote named rules. All grammar rules are
case-sensitive.

---

## Lexical rules

### Whitespace and comments

Horizontal whitespace (space, tab, `\r`, `\f`) is ignored everywhere.
Newlines (`\n`) are **significant** — they act as statement terminators (see
below).

```
line-comment  ::= '#' /* all chars except '-' on that position */
                  /* then reads until newline */

block-comment ::= '#-' /* any text */ '-#'
```

Block comments nest only at the `#-`/`-#` boundary; a `#-` inside a
block comment is just text.

A line comment that starts with exactly `#-` is a block comment, not a
line comment. Any other `# ...` is a line comment.

### Statement terminators

A **newline** token (`\n`) acts as a statement terminator, exactly like
a semicolon. The two forms are interchangeable:

```
stmt ;
stmt\n
```

Inside a `for` loop header `(init ; cond ; incr)` only `;` is accepted;
newlines inside the parentheses would close the `for` clause early.

Expressions **do not** span multiple lines. A newline mid-expression ends
the statement at the last complete expression.

### Identifiers and keywords

```
identifier ::= alpha (alpha | digit | '_')*
alpha      ::= [A-Za-z_]
digit      ::= [0-9]
```

Reserved keywords (not usable as identifiers):

```
fn  extern  const  type  struct
while  if  else  elif  for  switch
break  continue  as  return
true  false
```

### Types as tokens

Types are scanned as a single compound token during lexing so that the
parser can treat them uniformly as `TokenKind::LangType`.

```
type-token ::= const? base-type array-size? pointer-depth

base-type   ::= 'i' digit+      # signed integer (i8 i16 i32 i64 ...)
              | 'u' digit+      # unsigned integer (u8 u16 u32 u64 u0=void)
              | 'f' digit+      # floating-point (f32 f64)
              | 'bool'          # boolean (0 or 1)

array-size  ::= '[' decimal-int ']'   # preallocated array, e.g. u8[256]

pointer-depth ::= '*'*          # zero or more pointer levels
```

Valid types:

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
| `f32` | 32-bit floating point | 4 bytes |
| `f64` | 64-bit floating point | 8 bytes |
| `u0` | Void (no value) | — |
| `bool` | Boolean (0 or 1) | 1 byte (i8 storage, i1 value) |

Comparisons (`<`, `==`, …) and the logical operators (`&&`, `\|\|`, `!`) produce
`bool`. A `bool` coerces to any integer (0 or 1); the reverse needs an explicit
`!= 0` test. A `bool` is written with the `true`/`false` literals (or the integer
literals `0`/`1`). Loads of `bool` variables are tagged `!range !{i8 0, i8 2}` in
the emitted IR.

`const` marks a value as constant and must immediately precede the base
type. Any inline whitespace between `const` and the type is consumed by
the lexer, so `const u8*` and `const u8 *` are both valid.

Type modifier examples:

```tjlb
i32 value              # signed 32-bit integer
const i32 CONSTANT     # constant signed 32-bit integer
i32 *ptr               # pointer to i32
const u8 *str          # pointer to constant u8 (string)
i8 **argv              # pointer to pointer to i8
```

### Literals

#### Integer literals

```
integer-literal ::= decimal-int | hex-int | binary-int

decimal-int ::= digit+
hex-int     ::= '0' ('x'|'X') hex-digit+
binary-int  ::= '0' ('b'|'B') [01]+

hex-digit   ::= [0-9A-Fa-f]
```

All integer literals are parsed as `i64` internally. The type of the
resulting expression is inferred from context or defaults to `i32` for
values that fit, `i64` otherwise. Negative literals do not exist; use
unary minus: `0 - 128`.

#### Float literals

```
float-literal ::= digit+ '.' digit+
```

Float literals require digits on both sides of the decimal point.
`5.` and `.5` are not valid — use `5.0` and `0.5`.  The resulting type is
always `f64`.

#### String literals

```
string-literal ::= '"' string-char* '"'

string-char ::= '\n' | '\r' | '\t' | '\\' | '\"'   # escape sequences
              | /* any other char except '"' and '\n' */
```

A string literal cannot span multiple lines. The resulting expression
type is `u8*` (pointer to the first byte of a null-terminated constant
stored in the data segment).

Supported escape sequences: `\n` `\r` `\t` `\\` `\"`

### Operators and punctuation

```
( )  { }  [ ]  ;  :  ,  .  ->  ?  ~

+  -  *  /  %
==  !=  <  >  <=  >=
&&  ||  !
&  |  ^  <<  >>

=
+=  -=  *=  /=  %=
&=  |=  ^=  <<=  >>=
```

---

## Grammar (EBNF)

### Top level

```
program ::= (newline* top-decl newline*)*

top-decl ::= extern-fn-decl
           | fn-decl
           | global-var-decl

extern-fn-decl ::= 'extern' 'fn' ident '(' param-list ')' return-ann term

fn-decl ::= 'fn' ident '(' param-list ')' return-ann? newline* block

global-var-decl ::= type-token ident ('=' expr)? term

param-list ::= /* empty */
             | param (',' param)*
param      ::= type-token ident

return-ann ::= '->' type-token

term       ::= ';' | '\n'       # statement terminator
```

### Types

```
type  ::= type-token   # (scanned as a single lexer token — see above)
```

Type tokens carry base type, optional `const`, optional array size, and
pointer depth, all determined at lex time.

### Statements

```
stmt ::= block-stmt
       | return-stmt
       | if-stmt
       | while-stmt
       | for-stmt
       | break-stmt
       | continue-stmt
       | var-decl-stmt
       | assign-or-expr-stmt

block-stmt ::= '{' newline* stmt* newline* '}'

return-stmt ::= 'return' expr? term

if-stmt ::= 'if' expr newline* block-body
            ('else' newline* block-body)?
          | 'if' expr newline* block-body
            'elif' ...    # right-recursive elif chain

while-stmt ::= 'while' expr newline* block-body

for-stmt ::= 'for' '(' for-init ';' for-cond ';' for-incr ')' newline* block-body

for-init ::= /* empty */
           | type-token ident ('=' expr)?   # variable declaration
           | expr (assign-op expr)?         # assignment or expression

for-cond ::= /* empty */ | expr

for-incr ::= /* empty */
           | expr (assign-op expr)?

block-body ::= '{' newline* stmt* newline* '}'   # braces required

break-stmt    ::= 'break' term
continue-stmt ::= 'continue' term

var-decl-stmt ::= type-token ident ('=' expr)? term

assign-or-expr-stmt ::= expr (assign-op expr)? term

assign-op ::= '='
            | '+=' | '-=' | '*=' | '/=' | '%='
            | '&=' | '|=' | '^=' | '<<=' | '>>='
```

**Notes:**

- `block-body` always requires braces. Single-statement bodies without
  braces are not supported.
- A `var-decl-stmt` is recognised by the parser when the first token is
  a `type-token`. Everything else is an `assign-or-expr-stmt`.
- The `elif` chain is right-recursive: each `elif` is parsed as a
  nested `if-stmt`.

### Expressions

Expressions use a Pratt (precedence-climbing) parser. Precedence levels
are defined by the `INFIX_OPS` table in `src/parser/expressions.rs` (see
[Operator precedence](#operator-precedence) below).

```
expr ::= cast-or-alloc (infix-op cast-or-alloc)*
                       # -- driven by INFIX_OPS table

cast-or-alloc ::= alloc-expr
                | cast-expr

alloc-expr ::= type-token '[' expr ']'   # dynamic allocation

cast-expr ::= unary-expr ('as' type-token)*

unary-expr ::= '&' unary-expr    # reference (address-of)
             | '*' unary-expr    # dereference
             | '-' unary-expr    # negation  (0 - expr)
             | '!' unary-expr    # logical NOT
             | '~' unary-expr    # bitwise NOT
             | postfix-expr

postfix-expr ::= primary-expr (postfix-suffix)*

postfix-suffix ::= '(' arg-list ')'    # function call
                 | '[' expr ']'        # array/pointer subscript

primary-expr ::= integer-literal
               | float-literal
               | string-literal
               | ident
               | '(' expr ')'

arg-list ::= /* empty */ | expr (',' expr)*
```

**Notes:**

- `as` binds tighter than any binary infix operator. `x + 1 as i64`
  parses as `x + (1 as i64)`, not `(x + 1) as i64`.
- Postfix operations (calls, subscripts) chain: `arr[i][j]` and
  `f()()` are both valid.
- Unary minus has no literal form; it is sugar for `0 - expr`.

---

## Operator precedence

From lowest to highest. Operators at the same level are left-associative
(all current operators are non-associative within a level, since only one
entry per level exists — or they share a level but have the same
precedence, making left-to-right order natural).

| Prec | Operators | Notes |
|---|---|---|
| 1 | `\|\|` | logical OR |
| 2 | `&&` | logical AND |
| 3 | `==` `!=` `<` `>` `<=` `>=` | comparison, result type `i1` (bool) |
| 4 | `\|` | bitwise OR |
| 5 | `^` | bitwise XOR |
| 6 | `&` | bitwise AND |
| 7 | `<<` `>>` | bit shifts |
| 10 | `+` `-` | addition/subtraction, pointer arithmetic |
| 20 | `*` `/` `%` | multiplication, division, modulo |
| — | unary `- ! ~ & *` | parsed by `parse_unary` (above binary) |
| — | `as` type | parsed inside `parse_cast` (above unary) |
| — | `()` `[]` | parsed by `parse_postfix` (tightest) |

Comparison operators produce `i1` (1 = true, 0 = false). When assigned to an integer
variable the `i1` is zero-extended to the target width. Logical NOT (`!`) produces `i32`.
All other binary operators preserve the type of the left operand.

Pointer-to-pointer comparisons (`==`, `!=`, `<`, `>`, `<=`, `>=`) are supported; operands
must both be pointers. Comparison is unsigned (address order) and yields `i1`.

---

## Scoping rules

- There is one global scope (functions, global variables).
- Each function body opens a new scope.
- `{ ... }` block statements open a new child scope.
- For-loop headers share the same scope as the loop body (variables
  declared in `for-init` are visible in the body).
- `scoped!` in the parser implementation manages all scope
  enter/exit via `SymbolTable::enter_scope` / `exit_scope`.

Variable shadowing is allowed: a variable in an inner scope may have the
same name as one in an outer scope. After the inner scope closes, the
outer variable is visible again.

```
fn main(u32 argc, u8 **argv) -> i32 {
    i32 x = 10
    {
        i32 x = 20   # shadows outer x
        x = x + 5    # x = 25 (inner)
    }
    return x         # x = 10 (outer)
}
```

---

## Notable constraints

### Newlines as terminators

A newline ends the current statement. Multi-line expressions are
therefore **not** supported:

```
# ERROR: the parser sees two statements
i32 result = a
           + b

# OK: entire expression on one line
i32 result = a + b
```

### Braces required on all bodies

`if`, `elif`, `else`, `while`, and `for` bodies always require `{ }`.

```
# ERROR
if x > 0
    return x

# OK
if x > 0 {
    return x
}
```

### Literal integer range

Integer literals are scanned into an `i64`. Specifying a value that
exceeds `i64::MAX` is a parse error. Negative values have no literal
form; write `0 - n` instead.

### `as` is explicit and always required for narrowing

The type checker allows implicit widening between compatible integer
categories (`SInt` / `UInt`), but the code generator may emit a warning.
Use explicit `as` to make intent clear and silence the warning:

```
u64 n = 0
n += 1 as u64     # explicit, no warning
```

Narrowing (e.g., `i64` to `i32`, or `i32` to `i8`) always requires `as`.
Pointer-to-integer and integer-to-pointer conversions also require `as`.

### String literals are `u8*`, not `const u8*`

The lexer returns string literal tokens whose expression type is `u8*`
(non-const, pointer depth 1). The type checker does not currently treat
`u8*` and `const u8*` as compatible, so functions expecting `const u8*`
parameters may emit a type error when called with a string literal or a
plain `u8*`. Prefer non-`const` parameter types for externally-declared
functions.

### Array-to-pointer decay

A preallocated array variable (`u8[N]`) decays to a pointer (`u8*`) in
any expression context. To pass the array's address to a function
expecting `u8*`:

```
u8[256] buf
fn takes_ptr(u8 *p) { ... }

takes_ptr(&buf as u8*)   # &buf is u8**, cast to u8*
```

### `for` loop init and increment: no trailing terminator

The `for-init` and `for-incr` sub-statements do **not** consume a
trailing `;` or `\n`; the enclosing `for` header provides the `;`
separators.

### Dynamic allocation syntax

`type[count]` as an expression (not a declaration) allocates `count`
elements of `type` on the heap and returns a pointer:

```
u8 *buf = u8[1024]    # heap-allocate 1024 bytes, return u8*
```

This is separate from the preallocated-array declaration `u8[1024] buf`
(which allocates on the stack at compile time).

---

## Examples

### Hello World

```tjlb
extern fn puts(u8 *str) -> u0

fn main(u32 argc, u8 **argv) -> i32 {
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

fn main(u32 argc, u8 **argv) -> i32 {
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

fn main(u32 argc, u8 **argv) -> i32 {
    u8[256] buffer
    memset(&buffer as u8*, 256 as u64, 0 as u8)
    return 0
}
```

### Command-Line Arguments

```tjlb
extern fn puts(u8 *str) -> u0

fn main(u32 argc, u8 **argv) -> i32 {
    if argc > 1 {
        u8 *first_arg = argv[1]
        puts(first_arg)
    }
    return 0
}
```

### Bitwise Operations

```tjlb
fn main(u32 argc, u8 **argv) -> i32 {
    i32 a = 12    # Binary: 1100
    i32 b = 10    # Binary: 1010

    i32 and_result = a & b    # 1000 = 8
    i32 or_result = a | b     # 1110 = 14
    i32 xor_result = a ^ b    # 0110 = 6

    return and_result + or_result + xor_result  # 28
}
```

---

*Generated from the TJLB compiler source. Grammar rules are derived from
`src/parser/expressions.rs`, `src/parser/statements.rs`, and
`tjlb-macros/src/expand.rs`.*
