# Parser

The parser (`src/parser/`) converts a flat `Vec<Token>` into a `Program` AST. It is a **recursive descent parser** with **precedence climbing** for binary expressions.

## Files

| File | Purpose |
|------|---------|
| `ast.rs` | AST node types: `ExprKind`, `StatementKind`, `Expression`, `Statement`, `Function`, `GlobalVar`, `Program` |
| `expressions.rs` | `Parser` struct, expression parsing, program-level parsing (`parse_program`, `parse_function`, `parse_global_var`), type parsing |
| `statements.rs` | Statement parsing methods |
| `types.rs` | Re-export of `LangType` and `TypeBase` from the lexer |
| `errors.rs` | `ParserError` enum |

## Parser Struct

```rust
pub struct Parser {
    tokens: Vec<Token>,
    current: usize,
    symbol_table: SymbolTable,
    string_literals: Vec<String>,
}
```

The parser owns the token vec, tracks position via `current: usize`, and **builds the symbol table during parsing** (not as a separate phase). The string literal table collects all string constants for later codegen.

## Infrastructure Methods

| Method | Behavior |
|--------|----------|
| `is_at_end()` | Check for `TokenKind::Eof` |
| `peek()` / `peek_ahead(n)` / `previous()` | Token inspection |
| `advance()` | Consume and return previous token |
| `check(kind)` | Compare discriminant (ignores payload) using `std::mem::discriminant` |
| `check_keyword(keyword)` | Exact keyword match |
| `match_token(kinds)` | Consume if any kind matches; returns `bool` |
| `expect(kind, message)` | Consume or error with `ExpectedToken` |
| `skip_newlines()` | Consume all consecutive `Newline` tokens |

## Entry Point

```rust
Parser::new(tokens).parse_program() -> Result<Program, ParserError>
```

`parse_program()` loops until EOF, parsing top-level items:
- Functions (`fn` keyword or `extern fn` keyword)
- Global variables (type token followed by identifier)

## Expression Parsing (Precedence Climbing)

Expressions are parsed via a chain of methods ordered by precedence. Each level calls the next-higher-precedence level. Binary expressions use a generic `parse_binary_expr()` helper that loops while operators at the current level match (left-associative).

### Precedence Chain (lowest → highest)

| Precedence | Method | Operators |
|-----------|--------|-----------|
| 0 (lowest) | `parse_logical_or()` | `\|\|` |
| 1 | `parse_logical_and()` | `&&` |
| 2 | `parse_bitwise_or()` | `\|` |
| 3 | `parse_bitwise_xor()` | `^` |
| 4 | `parse_bitwise_and()` | `&` |
| 5 | `parse_shift()` | `<<`, `>>` |
| 10 | `parse_additive()` | `+`, `-` |
| 20 | `parse_multiplicative()` | `*`, `/`, `%` |
| — | `parse_other()` | Tries `parse_alloc()`, falls back to `parse_cast()` |
| — | `parse_cast()` | `as` keyword (left-associative, chains) |
| — | `parse_unary()` | `-`, `!`, `&`, `*`, `~` (right-associative, recursive) |
| — | `parse_postfix()` | `()` (function call), `[]` (array index) |
| — | `parse_primary()` | Literals, identifiers, `(expr)` |

### Desugaring at Parse Time

Several constructs are lowered during parsing:

| Construct | Desugared Form |
|-----------|---------------|
| `-x` | `0 - x` (Binary Sub) |
| `x += 5` | `x = x + 5` |
| `arr[i]` | `*(arr + i)` (pointer arithmetic + dereference) |
| `elif cond { ... }` | `else { if cond { ... } }` |

### Type Parsing

Types are parsed from `LangType` tokens with optional modifiers:
- `const` prefix
- Array suffix `[expr]`
- Pointer suffix `*` (repeatable)

## Statement Parsing

`parse_statement()` dispatches on the current token:

| Token | Handler |
|-------|---------|
| `{` | `parse_block_statement()` |
| `return` | `parse_return_statement()` |
| `if` | `parse_if_statement()` |
| `while` | `parse_while_statement()` |
| `for` | `parse_for_statement()` |
| `break` | `parse_break_statement()` |
| `continue` | `parse_continue_statement()` |
| `LangType` | `parse_var_decl_or_assignment()` |
| `Identifier` + assignment op | `parse_assignment_statement()` |
| `Identifier` (no assignment) | `parse_expression_or_indexed_assignment()` |
| `*` (asterisk) | Deref assignment or expression |
| Anything else | `parse_expression_statement()` |

### Block Statements

`parse_block_statement()` enters a new scope in the symbol table, parses statements until `}`, then exits scope.

### For Loops

`for (init; condition; increment) { body }` — has its own scope. Special `*_for_loop` variants of expression/declaration/assignment statements exist that do **not** consume terminators, since `;` is used as a section delimiter consumed by the for-loop parser itself.

### Compound Assignments

All compound assignment operators (`+=`, `-=`, `*=`, `/=`, `%=`, `&=`, `|=`, `^=`, `<<=`, `>>=`) are desugared at parse time into `name = name op value`.

## Symbol Table Integration

The parser builds the symbol table as it encounters declarations:

- **Variable declarations**: Added to the current scope via `symbol_table.add_variable()`
- **Function declarations**: Added to the flat function table via `symbol_table.add_function()`
- **Expression parsing**: Looks up variable types and function signatures from the symbol table

This means the parser has semantic awareness — it resolves types and validates existence during parsing rather than as a separate pass.

## Error Handling

Uses `thiserror`-derived `ParserError`. All methods return `Result<T, ParserError>`. Errors propagate via `?` — no recovery/resynchronization.

| Variant | Trigger |
|---------|---------|
| `UnexpectedToken(String, Position)` | Unexpected token in context |
| `ExpectedToken(expected, found, Position)` | Missing expected token |
| `TypeMismatch(expected, found, Position)` | Type mismatch |
| `UndefinedVariable(name, Position)` | Variable not in symbol table |
| `UndefinedFunction(name, Position)` | Function not in symbol table |
| `ArgumentCountMismatch(func, expected, got, Position)` | Wrong arg count |
| `InvalidDereference(Position)` | Dereferencing non-pointer |
| `FunctionRedefinition(name, Position)` | Duplicate function body |
| `InvalidBinaryOperation(Position)` | Invalid binary op |
| `ExpectedExpression(Position)` | Expression required |
| `ExpectedStatement(Position)` | Statement required |
| `UnexpectedEof` | End of input |

## Notable Techniques

1. **`std::mem::discriminant` for token matching**: `check()` uses discriminant comparison, so `TokenKind::LangType(...)` matches any `LangType` variant regardless of payload.

2. **Newline-aware but newline-tolerant**: Newlines are explicit tokens and are consumed as optional statement terminators alongside `;`.

3. **Array-to-pointer decay**: When an array variable is referenced, its type decays from array to pointer (`decay_to_pointer()`).

4. **Function calls as postfix**: Only valid when the callee is a `Variable` (identifier). Indirect calls through function pointers are not supported.

5. **`parse_other` disambiguation**: Tries `parse_alloc()` first (type[expr]), falling back to `parse_cast()` (expr as type) via `.or_else()`.
