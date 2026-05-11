# Parser Macro Rewrite — Implementation Reference

> **Status: Complete.** All nine phases have been implemented.

This document describes the parser architecture after the rewrite. It is a
living reference for anyone writing or modifying parse rules.

---

## Architecture Overview

```
src/parser/
  expressions.rs   — Parser struct, Pratt expression engine, top-level rules
  statements.rs    — Statement dispatch table, all statement rules
  ast.rs           — AST node types
  errors.rs        — ParserError enum + position() helper
  types.rs         — type-parsing helpers

tjlb-macros/
  src/lib.rs       — #[parse_rule] attribute macro
  src/expand.rs    — DSL macro expansions (DslRewriter)
```

---

## Writing a Parse Rule

Annotate any `impl Parser` method with `#[parse_rule]`. The attribute does two things:

1. **DSL expansion** — every recognised `macro_name!(...)` call in the body is
   rewritten to plain Rust at compile time (see the table below).
2. **Context tracking** — the body is automatically wrapped in a
   `self.context_stack.push/pop` pair so the parser always knows which rule is
   currently executing.

```rust
#[parse_rule]
fn parse_return_statement(&mut self) -> Result<Statement, ParserError> {
    let pos = pos!();
    kw!(Return);
    let value = opt_unless_term!(parse_expression);
    term!();
    Ok(Statement::new(StatementKind::Return(value), pos))
}
```

### DSL Primitive Reference

| Macro | Expands to |
|---|---|
| `pos!()` | `self.peek().pos` |
| `kw!(Return)` | `self.expect_keyword(&Keyword::Return, "return")?` |
| `token!(OpenParen)` | `self.expect(&TokenKind::OpenParen, "(")?` |
| `token_if!(Semicolon, Newline)` | `self.match_token(&[TokenKind::Semicolon, TokenKind::Newline])` |
| `kw_if!(Else)` | `if self.check_keyword(&Keyword::Else) { self.advance(); true } else { false }` |
| `skip_nl!()` | `self.skip_newlines()` |
| `term!()` | `self.match_token(&[TokenKind::Semicolon, TokenKind::Newline])` |
| `opt_unless_term!(f)` | `if self.check_terminator() { None } else { Some(self.f()?) }` |
| `block_body!(f)` | Calls `self.f()` and unwraps `Block(stmts)` → `Vec<Statement>` |
| `ident!()` | Expects `Identifier`, clones name, advances |
| `lang_type!()` | `self.parse_type()?` |
| `opt!(f)` | Backtracking optional — saves/restores `current` + `string_literals` |
| `alt!(f, g, …)` | Tries each with backtracking, returns first `Ok` |
| `many!(f)` | Calls `self.f()` in a loop until error, returns `Vec<T>` (silent backtrack on error) |
| `sync!(f)` | Calls `self.f()`; on error records to `self.errors`, calls `synchronize()`, returns `None` |
| `scoped!({ body })` | Wraps `body` in `enter_scope()` / `exit_scope()`, exits on error too |

### Backtracking vs. Error Recovery

`opt!`, `alt!`, and `many!` use **backtracking**: on failure they restore
`self.current` and `self.string_literals` silently. Use these for speculative
parsing where failure is a normal branch (e.g., optional type annotations).

`sync!` uses **error recovery**: on failure it records the error in
`self.errors`, calls `synchronize()` to advance past the bad tokens, and
returns `None`. Use this in loops where every iteration *should* produce a node
(e.g., statements inside a block).

---

## Expression Parser — Pratt Table

All binary and comparison operators are driven by `INFIX_OPS` in
`expressions.rs`. To add a new binary operator:

1. Add one `InfixEntry` line to `INFIX_OPS`.
2. Add the corresponding `BinaryOp` or `ComparisonOp` variant (if new).
3. Handle it in the codegen.

No other parsing code changes.

Precedence values (higher = tighter binding):

| Level | Operators |
|---|---|
| 1 | `\|\|` |
| 2 | `&&` |
| 3 | `==` `!=` `<` `>` `<=` `>=` |
| 4 | `\|` |
| 5 | `^` |
| 6 | `&` |
| 7 | `<<` `>>` |
| 10 | `+` `-` |
| 20 | `*` `/` `%` |

---

## Statement Dispatch Table

`parse_statement` delegates via `STATEMENT_TABLE` in `statements.rs`. To add a
new statement form:

1. Add `(predicate_closure, Parser::parse_my_statement)` to `STATEMENT_TABLE`.
2. Implement `parse_my_statement` with `#[parse_rule]`.

---

## Error Handling

`parse_program` returns `Result<Program, Vec<ParserError>>`. The vector is
sorted by source position.

Each `ParserError` variant carries a `Position`. Call `err.position()` to
extract it, or use `parser.format_error(&err)` to get a
`"file:line:col: message"` string suitable for user display.

Construct a `Parser` with source file information:

```rust
let mut parser = Parser::new(tokens).with_source_file(path.display().to_string());
match parser.parse_program() {
    Ok(program) => { /* proceed */ }
    Err(errors) => {
        for e in &errors {
            eprintln!("error: {}", parser.format_error(e));
        }
    }
}
```

### Error Recovery Inside Blocks

`parse_block_statement` uses a `sync!` loop rather than `many!`. When a
statement parse fails, the error is recorded and `synchronize()` advances to
the next safe token (`}`, `;`/`\n`, or a statement-starting keyword). Parsing
of subsequent statements continues. All errors are collected and returned
together from `parse_program`.

### Context Stack

`Parser.context_stack: Vec<&'static str>` is maintained automatically by
`#[parse_rule]`. Each rule pushes its name on entry and pops on exit (including
on error paths, via the closure IIFE pattern). The stack is available for
inspection if richer error messages are desired in the future.

---

## Scope Management

`scoped!({ body })` expands to:

```rust
self.symbol_table_mut().enter_scope();
let __scope_result = (|| -> Result<_, ParserError> { Ok({ body }) })();
self.symbol_table_mut().exit_scope();
__scope_result?
```

`exit_scope()` is always called, even if `body` returns an error early.
**Do not** use `opt!`/`alt!` around rules that register variables or functions —
symbol-table mutations are not rolled back on backtrack.

---

## Implementation History

| Phase | What changed |
|---|---|
| 1 | `tjlb-macros` crate scaffolding; no-op `#[parse_rule]` |
| 2 | `parse_postfix` loop fix; `parse_cast_or_alloc` backtracking fix; `compound_op_for_token` extraction |
| 3 | Stateless DSL primitives: `pos!`, `kw!`, `token!`, `token_if!`, `kw_if!`, `skip_nl!`, `term!`, `ident!`, `lang_type!`, `opt_unless_term!`, `block_body!` |
| 4 | Backtracking combinators: `opt!`, `alt!`, `many!`, `scoped!` |
| 5 | `STATEMENT_TABLE` dispatch; `parse_var_decl_inner` / `parse_expression_or_assign_inner` factored out; `*_for_loop` duplicates deleted |
| 6 | Pratt expression parser (`INFIX_OPS`, `parse_expr_prec`); 12 hand-crafted precedence functions deleted |
| 7 | All parse functions migrated to `#[parse_rule]`; `Parser.current` and `string_literals` made `pub(crate)` for macro access |
| 8 | `context_stack`, `errors`, `source_file` added to `Parser`; `#[parse_rule]` emits context push/pop; `sync!` primitive; `parse_block_statement` uses recovery loop; `parse_program` returns `Result<Program, Vec<ParserError>>`; `format_error` prefixes filename |
| 9 | Doc rewrite (this file); stale inline comments removed |
