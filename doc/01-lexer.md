# Lexer

The lexer (`src/lexer/`) tokenizes TJLB source text into a flat `Vec<Token>`.

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
- `input: String` — the full source text
- `current: usize` — byte offset
- `line: usize`, `column: usize` — 1-based position tracking
- `tokens: VecDeque<Token>` — output buffer

`scan_all()` loops: `skip_whitespace()` → `scan_token()` until EOF, then appends an `Eof` token.

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
| `const` | Const modifier (also part of `const <type>` LangType) |
| `type` | Type alias (reserved) |
| `struct` | Struct definition (reserved) |
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
| Void | `u0` |

Types can be modified with:
- `const` prefix: `const i32`
- Array suffix `[N]`: `i32[10]`
- Pointer suffix `*` (repeatable): `i32*`, `i32**`

The scanner handles the `const` keyword specially: when `const` is followed by a type name, it produces a single `LangType` token with `is_const = true` rather than separate `Keyword` + `LangType` tokens.

## Comment Handling

Comments are consumed inside `skip_whitespace()` and are completely invisible to the token stream.

- **Line comments** (`#`): consume until `\n` or EOF
- **Block comments** (`#-` ... `-#`): consume until the closing `-#` sequence; not nestable; unterminated block comments produce `LexerError::UnterminatedBlockComment`

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
