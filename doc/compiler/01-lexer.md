# Lexer

The lexer (`src/lexer/`) tokenizes Aspect source text into a flat `Vec<Token>`.

## Files

| File | Purpose |
|------|---------|
| `scanner.rs` | `Scanner` struct — the lexer implementation |
| `tokens.rs` | `Token`, `TokenKind`, `Keyword`, `LangType`, `TypeBase` definitions |
| `errors.rs` | `Position` (line:column) and `LexerError` enum |

## Entry Point

```rust
pub fn tokenize(input: String) -> Result<Vec<Token>, LexerError>
```

This is the top-level convenience function. Internally it creates a `Scanner` and calls `scan_all()`.

## Scanner

The `Scanner` maintains:
- `input: Vec<char>` — the source decoded to characters
- `current: usize` — character index into `input` (not a byte offset)
- `line: usize`, `column: usize` — 1-based position tracking. These count
  characters, so a multi-byte UTF-8 character advances the column by 1,
  not by its byte length
- `file_id: u32` — stamped onto every emitted token's `Position`; set by the
  preprocessor so multi-file diagnostics can name the right file, and 0
  (the entry file) for the bare `tokenize` API

`scan_all()` loops: `skip_whitespace()` → `scan_token()` until EOF, then appends an `Eof` token.
It builds and returns a fresh `Vec<Token>`; the scanner holds no output buffer of its own.

A sibling entry point, `tokenize_with_file_id(input: String, file_id: u32)`, sets `file_id`
explicitly for imported files.

### Navigation Methods

| Method | Behavior |
|--------|----------|
| `peek()` | Look at current character without consuming |
| `peek_ahead(offset)` | Look ahead N characters |
| `advance()` | Consume and return one character, updating line/column |
| `match_char(expected)` | Consume if next char matches; returns `bool` |

## Token Types (`TokenKind`)

### Punctuation
`(`, `)`, `{`, `}`, `[`, `]`, `;`, `:`, `,`, `.`, `->`, `?`

### Arithmetic
`+`, `-`, `*`, `/`, `%`

### Comparison
`==`, `!=`, `<`, `>`, `<=`, `>=`

### Logical
`&&`, `||`, `!`

### Bitwise
`&`, `|`, `^`, `~`, `<<`, `>>`

### Assignment
`=`, `+=`, `-=`, `*=`, `/=`, `%=`, `&=`, `|=`, `^=`, `<<=`, `>>=`

### Literals and Identifiers
- `Integer(i64)` — decimal, hex (`0x`/`0X`), binary (`0b`/`0B`)
- `Float(f64)` — decimal floating-point
- `StringLiteral(String)` — double-quoted with escape sequences (`\n`, `\r`, `\t`, `\\`, `\"`)
- `Identifier(String)` — user-defined names
- `Keyword(Keyword)` — language keywords
- `LangType(LangType)` — built-in type tokens (includes array/pointer/const modifiers)

### Special
- `Newline` — acts as statement terminator
- `Eof` — end of file sentinel

## Keywords

| Keyword | Purpose |
|---------|---------|
| `fn` | Function declaration |
| `extern` | External (C) function declaration |
| `asm` | Inline-assembly function declaration (`asm fn`) |
| `const` | Const modifier (also part of `const <type>` LangType) |
| `type` | Type-struct / alias definition — struct definitions are spelled `type Name { ... }` |
| `struct` | Reserved, but unused — see `type` |
| `naked` | Naked function declaration (`naked fn`) — no prologue/epilogue |
| `alias` | Type alias (`alias myint i32`) |
| `public` | Module visibility — makes a function/global/type nameable from importing modules (private by default); also opts a field/method out of the private default |
| `export` | External linkage on a function or global — the object-file symbol is visible to foreign code (see [06-codegen](06-codegen.md#function-linkage-functionsrs)) |
| `sizeof` | Compile-time size of a type; yields `u64` |
| `null` | The null pointer constant |
| `true` / `false` | Boolean literals |
| `while` | While loop |
| `if` / `else` / `elif` | Conditional branching |
| `for` | For loop |
| `switch` | Switch statement (reserved) |
| `break` / `continue` | Loop control |
| `as` | Type cast operator |
| `return` | Return from function |

## Built-in Types

Parsed as `LangType` tokens (not keywords):

| Category | Types |
|----------|-------|
| Signed integers | `i8`, `i16`, `i32`, `i64` |
| Unsigned integers | `u8`, `u16`, `u32`, `u64` |
| Floats | `f32`, `f64` |
| Boolean | `bool` — an i1 logical value stored as i8 (`size_bits` is the storage width, 8). The type of comparisons and `!` |
| Void | `u0` — the special case mapping to `TypeBase::Void` |

The integer and float rows list the widths in common use, not the accepted set:
`langtype_from_str` admits `i`/`u`/`f` followed by any positive multiple of 8, so
`i128` lexes as a valid LangType token even where later phases cannot lower it.

Types can be modified with:
- `const` prefix: `const i32`
- Array suffix `[N]`: `i32[10]`
- Pointer suffix `*` (repeatable): `i32*`, `i32**`

The scanner handles the `const` keyword specially: when `const` is followed by a type name, it produces a single `LangType` token with `is_const = true` rather than separate `Keyword` + `LangType` tokens.

## Comment Handling

Comments are consumed inside `skip_whitespace()` and are completely invisible to the token stream.

- **Line comments** (`#`): consume until `\n` or EOF
- **Block comments** (`#-` ... `-#`): consume until the closing `-#` sequence; not nestable; unterminated block comments produce `LexerError::UnterminatedBlockComment`

> **Known bug**: `UnterminatedBlockComment` is constructed by `skip_block_comment()` but
> never escapes the scanner. Its only caller, `skip_whitespace()`, returns unit and
> discards it (`if self.skip_comment().is_err() { break; }`), so an unterminated `#-`
> lexes *successfully* — the rest of the file is silently eaten as comment text. The
> user sees either a positionless downstream `Unexpected end of input` or, if the
> truncation leaves balanced braces, no error at all. Fix is to make `skip_whitespace`
> return `Result` and propagate; the variant is unreachable until then.

## Operator Disambiguation

The scanner uses `match_char()` lookahead to disambiguate multi-character operators:

| First char | Lookahead chain |
|-----------|----------------|
| `=` | `==` or `=` |
| `!` | `!=` or `!` |
| `<` | `<<=` or `<<` or `<=` or `<` |
| `>` | `>>=` or `>>` or `>=` or `>` |
| `-` | `->` or `-=` or `-` |
| `&` | `&&` or `&=` or `&` |
| `\|` | `\|\|` or `\|=` or `\|` |

## Error Handling

Uses `thiserror`-derived `LexerError`. The scanner does **not** attempt error recovery — the first error terminates lexing.

| Variant | Trigger |
|---------|---------|
| `UnexpectedChar(char, Position)` | Character that doesn't start any valid token |
| `UnterminatedString(Position)` | Missing closing `"` or raw newline in string |
| `UnterminatedBlockComment(Position)` | `#-` without matching `-#` before EOF |
| `InvalidNumber(String, Position)` | Numeric literal that fails to parse |
| `InvalidEscape(char, Position)` | Unrecognized escape sequence in string |
| `UnexpectedEof` | `advance()` called at end of input |
