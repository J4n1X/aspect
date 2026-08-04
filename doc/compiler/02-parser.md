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
    /// Transient per-function variable scopes (discarded after parsing).
    symbol_table: SymbolTable,
    /// Cross-phase global symbols (functions, type-structs, aliases); moved
    /// into the `Program` at the end of `parse_program`.
    module: ModuleSymbols,
    /// Deduplicating and insertion-ordered — hence not a `Vec`.
    string_literals: IndexSet<String>,
    context_stack: Vec<&'static str>,
    /// Multi-error accumulator; see Error Handling below.
    errors: Vec<ParserError>,
    /// Bodies skipped in pass 1, parsed in pass 2 once every prototype is
    /// registered — this is what makes forward references work.
    pending_bodies: Vec<PendingBody>,
    alias_prescan_sites: HashSet<usize>,
    /// File registry indexed by `Position::file_id`.
    source_files: Vec<PathBuf>,
    /// Module of each file, parallel to `source_files`.
    file_modules: Vec<String>,
    /// Module → its *direct* imports; drives the import-visibility check.
    module_imports: HashMap<String, Vec<String>>,
    /// Declaration positions per type-struct/sum id — the registry stores no
    /// positions, so the by-value-containment cycle check reports here.
    struct_decl_pos: HashMap<u32, Position>,
    sum_decl_pos: HashMap<u32, Position>,
}
```

Type-shape declarations are handled by `parse_struct_def`, `parse_enum_def` and
`parse_sum_def` (all in `declarations.rs`), each backed by a prescan
(`prescan_type_names` / `prescan_enum_names` / `prescan_sum_names`) that
reserves the name before the main parse so forward references resolve. A sum
(`sum Shape { Circle(f64 radius) … }`) parses like an enum with
parameter-style payload fields per variant — one variant per line — and, like
an enum, produces no AST node, only a `SumInfo` registry entry. After pass 1,
`check_byvalue_containment_cycles` walks the by-value containment graph over
all type-structs and sums (arrays included; any pointer depth breaks an edge)
and reports `RecursiveByValue` for each cycle — forward references mean a
cycle may only close once the whole top level is parsed, which is why this is
a whole-program pass and not a per-declaration check.

The parser owns the token vec and tracks position via `current: usize`.
`symbol_table` holds only **transient per-function variable scopes** and is
discarded once parsing completes; the durable global symbols accumulate in
`module` and move into `Program::symbols` for the type checker and codegen.
The string literal table collects all string constants for later codegen.

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
Parser::new(tokens).parse_program() -> Result<Program, Vec<ParserError>>
```

`parse_program()` loops until EOF, parsing top-level items:
- Functions (`fn`, `extern fn`, or `asm fn`)
- Global variables (type token followed by identifier)

Each iteration first calls `parse_kind_modifier()`, which consumes the leading
`extern`/`asm` and reports naming two of them as one error regardless of
order. Keeping that scan separate from the dispatch is what stops the
diagnostic depending on which keyword happened to come first.

## Expression Parsing (Precedence Climbing)

Binary and comparison operators are parsed by a single precedence-climbing loop,
`parse_expr_prec(min_prec)` (`expressions.rs`), over the static `INFIX_OPS` table.
That table is the **single source of truth** for binding strength, and it includes
the comparison operators, which `BinaryOp` does not model.

### Precedence Chain (lowest → highest)

| Precedence | Operators |
|-----------|-----------|
| 1 (lowest) | `\|\|` |
| 2 | `&&` |
| 3 | `==`, `!=`, `<`, `>`, `<=`, `>=` |
| 4 | `\|` |
| 5 | `^` |
| 6 | `&` |
| 7 | `<<`, `>>` |
| 10 | `+`, `-` |
| 20 | `*`, `/`, `%` |

> **Trap — this is not C.** Comparisons bind **looser** than every bitwise
> operator (3 vs `\|`=4, `^`=5, `&`=6, shifts=7). In C, `==`/`!=` bind *tighter*
> than `&`/`^`/`\|`. So `a | b == c` parses as `(a | b) == c` here, not
> `a | (b == c)`: `1 | 0 == 0` evaluates to `0` in Aspect and `1` in C.
> Parenthesise any mixed bitwise/comparison expression.

The tighter-than-binary levels are ordinary methods, not table entries:

| Level | Method | Operators |
|-------|--------|-----------|
| — | `parse_cast_or_alloc()` | Tries `parse_alloc()`, falls back to `parse_cast()` |
| — | `parse_cast()` | `as` keyword (left-associative, chains) |
| — | `parse_unary()` | `-`, `!`, `&`, `*`, `~` (right-associative, recursive) |
| — | `parse_postfix()` | `()` (function call), `[]` (array index) |
| — | `parse_primary()` | Literals, identifiers, `(expr)` |

### Desugaring at Parse Time

Several constructs are lowered during parsing:

| Construct | Desugared Form |
|-----------|---------------|
| `-x` | `0 - x` (Binary Sub) — **except** when `x` is an integer or float literal, which folds into a negative `Literal` instead (so `-128` can narrow to `i8`; `0 - 128` cannot) |
| `x += 5` | `x = x + 5` |
| `arr[i]` | `*(arr + i)` (pointer arithmetic + dereference) |
| `elif cond { ... }` | `else { if cond { ... } }` |

### Type Parsing

Types arrive from the lexer **already complete** — the scanner folds `const`, the
`[N]` array suffix, and repeated `*` into a single `LangType` token (see
[01-lexer](01-lexer.md)). `parse_type()` (in `expressions.rs`) reads that token; it
does not apply the modifiers itself.

The array suffix must be an integer **literal**, since it is stored as
`array_size: Option<u32>` on the token. `i32[n]` for a variable `n` is not a type
and does not lex as one — it becomes four tokens (`LangType(i32)`, `[`, `n`, `]`)
and fails to parse in type position.

A runtime-sized allocation is a different construct: the *alloc expression*
`i32[n]` in expression position, handled by `parse_alloc()` into
`ExprKind::Alloc { alloc_type, count }`, where `count` is an arbitrary expression.

## Statement Parsing

`parse_statement()` loops over the static `STATEMENT_TABLE` of
`(predicate, handler)` pairs (`statements.rs`), taking the first predicate that
matches, and falls through to a catch-all when none does:

| Predicate | Handler |
|-----------|---------|
| `{` | `parse_block_statement()` |
| `return` | `parse_return_statement()` |
| `if` | `parse_if_statement()` |
| `while` | `parse_while_statement()` |
| `for` | `parse_for_statement()` |
| `break` | `parse_break_statement()` |
| `continue` | `parse_continue_statement()` |
| `LangType` token | `parse_var_decl_or_assignment()` |
| `starts_named_var_decl` — `myint x`, `Point* p` | `parse_var_decl_or_assignment()` |
| `starts_fnptr_var_decl` — `fn(i32) -> i32 op = &double` | `parse_var_decl_or_assignment()` |
| `starts_grouped_var_decl` — `(fn(i32) -> i32)[3] table = ...` | `parse_var_decl_or_assignment()` |
| Anything else (fallthrough) | `parse_expression_or_assign_statement()` |

There is no `*`-specific row: `*ptr = x` reaches the catch-all like any other
expression or assignment.

### Block Statements

`parse_block_statement()` enters a new scope in the symbol table, parses statements until `}`, then exits scope.

### For Loops

### `is` and condition scoping

`is` parses as an infix at the comparison tier (`parse_is_suffix`), resolving
its variant against the scrutinee's parse-time type. A parenthesized pattern
builds the binding form and registers its binders into the current scope
immediately — textual order, which is what lets later `&&`-conjuncts and the
block reference them. To make binders die with the block,
`parse_if_statement`/`parse_elif_body`/`parse_while_statement` wrap
condition + body in **one** parse-time scope (the `for`-header pattern; the
else-branch is outside it). Placement policing at parse time: a parenthesized
binding form errors at the paren primary, and folding a `||` whose operands
contain a binding `is` errors at the fold — everything else (expression
positions, `for` headers) is caught by the checker's `IsBinding` synthesis
arm.

### The tri-scope invariant

Variable references stay **name strings** in the AST — no resolved symbol id —
so three phases re-resolve names lexically and each keeps its own scope stack:
the parser's transient `SymbolTable` (type stamping for `.`-resolution and
pattern resolution; redeclaration errors; discarded after parse), the
checker's scopes (the authority on undefined-variable errors), and codegen's
`ScopeStack` (allocas). **Every scoped construct must implement its scoping in
all three walks** — `for` headers, value blocks, and switch arms each do — and
nothing enforces this mechanically: omitting one shows up as stale types or
wrong-variable resolution, not a crash. (A future refactor could resolve names
once and stamp slot ids on `Variable` nodes, collapsing the other two.)

### Switch

`switch scrutinee { case … { } … default { } }` — `parse_switch_statement` /
`parse_switch_arm` / `parse_switch_pattern` (all driven by the scrutinee's
parse-time `expr_type`): sum scrutinees take variant patterns with positional
binders (`_` discards; bare variant = ignore payload), everything else takes
constant expressions, with bare enum variant names resolved against the
scrutinee's enum. Arm bindings are registered into the parse-time symbol
table (a fresh scope) *before* the body parses — that is what lets the body
reference them. The parser also computes `complete` (dedup-aware variant/
literal coverage) so the checker's registry-less termination analysis can
read it, and enforces `default`-last plus the binding-pattern-stands-alone
rule.

### For Loops

`for init; condition; increment { body }` — has its own scope. The `;` is a section
delimiter consumed by the for-loop parser itself, so the sections must be parsed
*without* consuming a terminator. That is what the `_inner` pair is for:
`parse_var_decl_inner()` and `parse_expression_or_assign_inner()` parse the bare
construct, while the public wrappers (`parse_var_decl_or_assignment`,
`parse_expression_or_assign_statement`) add `term!()` on top. `parse_for_statement`
calls the `_inner` forms directly.

### Compound Assignments

All compound assignment operators (`+=`, `-=`, `*=`, `/=`, `%=`, `&=`, `|=`, `^=`, `<<=`, `>>=`) are desugared at parse time into `name = name op value`.

## Symbol Table Integration

The parser builds the symbol table as it encounters declarations:

- **Variable declarations**: Added to the current scope via `symbol_table.add_variable()`
- **Function declarations**: Added to the module's global symbols via `ModuleSymbols::add_function()` (`src/symbol/module.rs`) — *not* `symbol_table`, which holds variables only
- **Expression parsing**: Looks up variable types and function signatures to stamp `expr_type`. An identifier that resolves to nothing is **not** a parse error — it is stamped `u0` and left for the type checker, which raises `TypeCheckError::UndefinedVariable`. Function *calls* are still validated here (`ParserError::UndefinedFunction`)

So the parser has *partial* semantic awareness: it resolves types eagerly, but defers variable-existence diagnosis to the type checker.

## Error Handling

Uses `thiserror`-derived `ParserError`. Individual rules return `Result<T, ParserError>`
and propagate via `?`, but `parse_program` returns `Result<Program, Vec<ParserError>>` —
the parser **recovers and reports every error it can**. `parse_block_statement` wraps each
statement in `sync!`, which on failure pushes the error onto `Parser::errors` and calls
`synchronize()` to skip to the next statement/declaration boundary (it stops *before* a
statement-starting keyword or `}`; the block loop consumes a token itself when the cursor
has not advanced, bounding recovery at one error per token).

| Variant | Trigger |
|---------|---------|
| `UnexpectedToken(String, Position)` | Unexpected token in context |
| `ExpectedToken(expected, found, Position)` | Missing expected token |
| `TypeMismatch(expected, found, Position)` | Type mismatch |
| `UndefinedVariable(name, Position)` | **Declared but never constructed** — the parser stamps unknown names `u0`; `TypeCheckError::UndefinedVariable` is what users actually see |
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

4. **Function calls as postfix**: a call whose callee is a bare identifier naming a function becomes `ExprKind::FunctionCall` (direct call by name). A call through any other expression of function-pointer type becomes `ExprKind::IndirectCall { callee, args }`, which codegen lowers via `build_indirect_call` after resolving the signature through the callee's `TypeBase::FnPtr(id)`. A bare function name in value position, and `&func`, both produce `ExprKind::FunctionRef`.

5. **`parse_cast_or_alloc` disambiguation**: Tries `parse_alloc()` first (type[expr]), falling back to `parse_cast()` (expr as type) via `.or_else()`.
