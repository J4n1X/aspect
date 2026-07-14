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
3. [Preprocessor](#preprocessor)
   - [Defines](#defines)
   - [Conditionals](#conditionals)
   - [Modules](#modules)
4. [Grammar (EBNF)](#grammar-ebnf)
   - [Top level](#top-level)
   - [Types](#types)
   - [Statements](#statements)
   - [Expressions](#expressions)
5. [Operator precedence](#operator-precedence)
6. [Scoping rules](#scoping-rules)
7. [Notable constraints](#notable-constraints)

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
fn  extern  const  type  struct  alias  public  sizeof
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

**The opaque pointer `u0*`.** `u0` itself is not a value type — declaring a
`u0` variable, parameter, or global is a type error — but `u0*` is the
language's universal object pointer (C's `void*`):

- Any pointer, of any depth, coerces to and from `u0*` implicitly:
  `free(xs)` works for `i32* xs`, and `i32* p = malloc(n)` needs no cast.
- A `u0*` is *opaque*: dereferencing it, subscripting it, and pointer
  arithmetic on it are all rejected — cast to a sized pointer first
  (`p as i32*`, or `p as u8*` for byte offsets).
- Null tests work directly: `p == null`, `if p { ... }`, `!p`.
- `u0**` is an ordinary pointer whose pointee happens to be `u0*`; it
  subscripts fine and is **not** implicitly convertible like `u0*` is.
- `sizeof(u0*)` is the pointer width; `sizeof(u0)` is an error.

Use `u0*` where no particular pointee is expected (allocators, callbacks
over erased elements, opaque handles like `FILE*`); use `u8*` when the
data really is bytes.

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

## Preprocessor

Before the parser runs, a preprocessor stage walks the token stream and
expands `$<directive>` lines in place. The dollar sigil was chosen because
`#` is already taken by line comments; `@` is reserved wholesale for the
metasystem (attributes/transforms) and never interpreted here. The
preprocessor is **token-level**: it operates on the lexer's output, so
substitution is word-boundary-safe and can never rewrite string literals.

Directives are **line-anchored**: `$` must be the first token on its line
(leading whitespace is fine), and everything up to the newline belongs to
the directive. A `$` anywhere else on a line is an error, as is a
line-leading `$` inside a block — directives only exist at the top level
of a file. Unknown directive names error with a did-you-mean suggestion.

The directive families:

| Directives | Purpose |
|---|---|
| `$define` / `$undefine` | object-like defines and substitution |
| `$ifdef` `$ifndef` `$if` `$elseif` `$elseifdef` `$else` `$endif` | conditional compilation |
| `$module` / `$import` | module identity and loading — see [10-modules.md](10-modules.md) |

### Defines

```tjlb
$define DEBUG                      # flag define (no value)
$define MAX_SIZE 1024              # value = rest-of-line token sequence
$define GREETING "hello"           # any tokens, string literals included
$undefine DEBUG                    # removes; no-op if not defined
```

- **Object-like only.** There are no function-like macros — parameterised
  code generation is the metasystem's job; the preprocessor will not grow
  a second macro language.
- Substitution is by identifier token: wherever the name appears as an
  `Identifier`, the define's token sequence is spliced in (substituted
  tokens keep the use-site position). Substitution is recursive, but a
  name expands at most once per chain (self-reference guard, like C).
  Token-level substitution means `u8[MAX_SIZE]` works: the array size is
  its own token.
- **Redefinition is an error** — `$undefine` first. `-D` definitions count
  as prior defines, so a file-level `$define` of a `-D`-injected name is
  the same error. Files that want overridable defaults write the guard:

  ```tjlb
  $ifndef MAX_SIZE
  $define MAX_SIZE 1024
  $endif
  ```
- Defines are global once made: a define is visible in every file
  processed after it, imported files included.

**Compiler-provided defines**, seeded before anything else:

| Define | When |
|---|---|
| `OS_LINUX` / `OS_WINDOWS` / `OS_MACOS` | target OS |
| `ARCH_X86_64` / `ARCH_AARCH64` | target arch |
| `TJLB_VERSION_MAJOR` / `TJLB_VERSION_MINOR` | compiler version, integer tokens |

**CLI:** `-D NAME` and `-D NAME=VALUE` (repeatable) inject defines before
the entry file is processed — the standard build-system hook.

### Conditionals

```tjlb
$ifdef OS_LINUX
    extern fn epoll_create1(i32 flags) -> i32
$elseifdef OS_MACOS
    extern fn kqueue() -> i32
$else
    # portable fallback
$endif

$if MAX_SIZE > 4096
    const u64 BUCKETS = 64
$elseif MAX_SIZE > 512
    const u64 BUCKETS = 16
$else
    const u64 BUCKETS = 4
$endif
```

- `$ifdef NAME` / `$ifndef NAME` — true iff NAME is (not) defined. There
  is no `$elseifndef`; chains spell it `$elseif !defined(NAME)`.
- `$if EXPR` — EXPR is a **constant integer expression** over integer
  literals, defined names (substituted first; must expand to constant
  integer expressions), `defined(NAME)` (1 or 0), the operators
  `+ - * / % << >> & | ^ ! && || == != < > <= >=`, and parentheses.
  **Undefined identifiers in an `$if` are an error**, not silently 0 —
  C's silent-zero rule is a famous bug factory. Division by zero is also
  an error.
- Chain form: `$if`/`$ifdef`/`$ifndef`, then any mix of
  `$elseif`/`$elseifdef`, at most one `$else`, closed by `$endif`.
  Exactly one branch of a chain is active — the first whose condition
  holds (or the `$else` if none did).
- Chains nest arbitrarily. Inside a skipped branch, ordinary tokens are
  dropped and non-conditional directives are inert (`$define` does not
  define, `$import` does not resolve, unknown names do not error), but
  the conditional directives themselves are still tracked so nesting
  stays matched. `$endif` must always match up; a chain must open and
  close within one file.

### Modules

`$module <path>` declares the module a file belongs to; `$import <path>`
makes a module part of the compilation, resolved against the `-I` search
roots. Modules are the language's load unit and visibility boundary —
they get their own chapter: [10-modules.md](10-modules.md).

## Grammar (EBNF)

### Top level

```
program ::= (newline* top-decl newline*)*

top-decl ::= extern-fn-decl
           | fn-decl
           | global-var-decl
           | alias-decl
           | struct-decl

extern-fn-decl ::= 'extern' 'fn' ident '(' param-list ')' return-ann term

fn-decl ::= 'fn' ident '(' param-list ')' return-ann? newline* block

global-var-decl ::= type ident ('=' expr)? term

alias-decl ::= 'alias' ident type term    # compile-time typedef

struct-decl ::= 'type' ident '{'
                  newline* (struct-field (term newline*)?)*
                  newline* (struct-method (term newline*)?)*
                '}'
struct-field  ::= 'public'? type ident                     # fields are private unless `public`
struct-method ::= 'public'? 'const'? 'fn' ident '(' method-params ')' return-ann? newline* block
method-params ::= /* empty */
                | 'this' (',' param-list)?                 # instance method
                | param-list                               # static method (no `this`)
# `public` opts a method into external access; like fields, methods are
# private by default. A `public fn(T) -> R name` *field* (function-pointer
# type) is told apart from a `public fn name(...)` *method* by the token after
# `fn` — a name means method, `(` means a fn-pointer field type.
# `const fn` requires `this`; field access through it propagates const, so
# any `this.field = ...` is rejected. Fields must come before methods.

param-list ::= /* empty */
             | param (',' param)*
param      ::= type ident

return-ann ::= '->' type

term       ::= ';' | '\n'       # statement terminator
```

### Types

```
type       ::= type-atom ('[' integer ']')? ('*')*   # postfix array / pointer modifiers
type-atom  ::= type-token                            # built-in (lexer-folded)
             | ident                                 # named: an alias or type-struct
             | fnptr-type                            # function-pointer type
             | '(' type ')'                          # grouping — disambiguates `(T)[N]` etc.
fnptr-type ::= 'fn' '(' (type (',' type)*)? ')' ('->' type)?
```

**Grouped types.** The lexer eagerly folds `T[N]` and `T*` into the preceding type token, so
`fn(...) -> T[N]` means "fn returning `T[N]`", not "array of fn-pointers". Parens are the
"stop folding here" marker that lets you spell the other shape. They generalise:

```
(fn(i32) -> i32)[3] table   # array of 3 fn-pointers
(fn(i32) -> i32)*  pp       # pointer to a fn-pointer
(i32*)[3]          arr      # array of 3 i32 pointers
```

Built-in type tokens carry base type, optional `const`, optional array size, and
pointer depth, all determined at lex time. A **named type** is an identifier that
resolves to a declared `alias` or `type` (type-struct); the lexer leaves it as a
bare identifier and the parser resolves it against the module symbol table,
attaching any trailing `*` pointer modifiers. An identifier that resolves to no
declared type is an "undefined type" error.

An `alias` is fully transparent: `alias myint i32` makes `myint` an exact stand-in
for `i32` everywhere (variables, parameters, return types), with no distinct type
identity in the type checker or generated IR. Aliases must be declared before use.

A **type-struct** (`type Name { ... }`) is a named aggregate. Fields are **private
by default**; prefix a field with `public` to expose it. Type-struct names may be
referenced before their definition (a name-collection prescan reserves them), so
self- and mutually-referential structs work (via pointer fields). Construct a value
with a **named struct literal** `Name { field = expr, ... }`, which must name *every*
field (no partial init / defaulting). Read or write a field with `base.field`; the
base may be a struct value or a single-level pointer-to-struct (which auto-derefs).
Structs may be passed by pointer (`fn f(Point* p)`) **or by value** (`fn f(Point p)`):
by-value parameters use `byval` and by-value returns use a hidden `sret` out-pointer.
(By-value structs across the `extern`/C boundary await per-target ABI work; tjlb-to-tjlb
calls work today.)

```
struct-literal ::= ident '{' newline* (field-init (',' newline*)?)* '}'   # `ident` must name a type-struct
field-init     ::= ident '=' expr
field-access   ::= postfix '.' ident                                      # read; also a valid assignment target
method-call    ::= postfix '.' ident '(' arg-list ')'                     # instance — autorefs value receivers
                 | ident   '.' ident '(' arg-list ')'                     # static — `ident` is a known type-struct
```

**Methods.** A method inside `type T { ... }` whose first parameter is the bare identifier
`this` (no type annotation) is an *instance* method; otherwise it is *static*. The parser
desugars the method to a free function named `T$method` and synthesises the `this` parameter as
`*T` (or `*const T` for `const fn`). On a method call, a value receiver is auto-referenced —
`obj.m()` lowers to `T$m(&obj, ...)` — and a pointer receiver is passed through unchanged.
Static methods take no receiver; `T.m(...)` lowers to `T$m(...)`.

**Function pointers.** A `fn(T1, T2, ...) -> R` (or `fn(...)` for a `u0`/void return) *is*
a pointer — there is no separate non-pointer function type. A function's address is taken
by name: `&func` and bare `func` both produce a value of `fn(...) -> R` matching the
function's signature. Calling through a function-pointer value uses the regular call syntax
— `ptr(args)` — and lowers to LLVM's `call` through the pointer (`build_indirect_call`).
A function-pointer type composes with the existing array suffix using parenthesised types:
`(fn(i32) -> i32)[3] table = {&a, &b, &c}; table[i](x)`. (An `alias` over the fn-ptr type
is a fine stylistic alternative.)

```
fnptr-value ::= '&' ident          # explicit address-of a function name
              | ident              # implicit fn-to-ptr decay of a known function
indirect-call ::= postfix '(' arg-list ')'   # callee must have a fn-ptr type
```

The two call forms are **strict**: an instance method must be called as `obj.m(...)` and a
static method as `T.m(...)`. A static-form call to an instance method (UFCS-style
`T.m(&obj, ...)`) and an instance-form call to a static method (`obj.m(...)`) are both
rejected at parse time with a precise diagnostic.

**Encapsulation.** Fields *and methods* are private by default; `public` opts either into external
access. From outside the type's own methods, a private field cannot be read, assigned, or named in a
struct literal, and a private method cannot be called (in `obj.m()` or `Type.m()` form). Combined
with the "every field must be named" rule, a type-struct with any private field is unconstructable
by an external literal and must be created via one of its own `public` static methods (the factory
pattern). A method's visibility is enforced by the type checker after the parser lowers the call to
the mangled `Type$method` free function; a private method remains freely callable from any of the
type's own methods (static or instance), exactly like private-field access.

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
               | sizeof-expr
               | list-initializer
               | value-block

sizeof-expr ::= 'sizeof' '(' type ')'    # compile-time u64 byte size

list-initializer ::= '{' (expr (',' expr)*)? '}'   # array literals
value-block      ::= '{' stmt* '}'                 # block as an expression; see below

arg-list ::= /* empty */ | expr (',' expr)*
```

**Value blocks.** A `{ ... }` in *expression* position whose statements produce the
expression's value via `return`:

```tjlb
i32 clamped = {
    if x > 100 {
        return 100
    }
    return x
}
```

- A `return` inside a value block binds to the **innermost value block**, not the
  enclosing function — so inside one, you cannot early-return from the function.
  Nested value blocks each capture their own `return`s.
- **Every control path must end in `return <expr>`** (the if/else form counts when
  both arms return). Loops never satisfy the rule, conservatively — even
  `while true { return 1 }` is rejected, since the checker does not prove loop
  behaviour. A bare `return` (no value) inside a value block is an error.
- `break` and `continue` pass through to the enclosing **loop**; value blocks are
  transparent to them.
- In a checked position (initializer, argument, function `return`) the block adopts
  the target type and pushes it into every `return`; in a synthesis position
  (condition, cast operand) the first `return` fixes the type and the rest must
  coerce to it.
- **Disambiguation from list initializers**: a brace expression that parses as a
  comma-separated expression list *is* a list (`{1, 2, 3}`, `{x}`, `{}`); anything
  else is re-parsed as a value block. The grammars cannot collide — a valid value
  block must contain `return`, which can never appear in a list. A `{` in
  *statement* position is always a plain block statement, never a value block.
- Value blocks execute statements, so they are never compile-time constants:
  global initializers cannot use them.

**Notes:**

- `as` binds tighter than any binary infix operator. `x + 1 as i64`
  parses as `x + (1 as i64)`, not `(x + 1) as i64`.
- Postfix operations (calls, subscripts) chain: `arr[i][j]` and
  `f()()` are both valid.
- Unary minus has no literal form; it is sugar for `0 - expr`.
- `sizeof(T)` is a **compile-time** `u64` that lowers to a single
  constant at codegen via the target data layout. Works for every
  type (primitives, pointers, function pointers, arrays, type-structs
  with padding, parens-grouped composites). Parens are required —
  `sizeof T` is a syntax error.

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
any expression context — pass it directly, no `&` and no cast:

```
u8[256] buf
fn takes_ptr(u8 *p) { ... }

takes_ptr(buf)           # u8[256] decays to u8*
```

Same-depth pointers additionally coerce into one another implicitly, so
a decayed `i32[5]` also passes where a `u8*` is expected. The historical
`takes_ptr(&buf as u8*)` dance still works (`&buf` is `u8**`, the cast
flattens it) but is never needed.

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
    memset(buffer, 256, 0)      # array decays to u8*
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
