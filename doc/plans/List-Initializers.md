# List Initializer Plan

## Overview

Add brace-list syntax for initializing fixed-size array variables at declaration time.
Currently `u8[8] buf` allocates an array but leaves every element zero-initialized with
no way to set values inline. The new syntax lets the programmer write:

```tjlb
u8[5] vowels = {'a' as u8, 'e' as u8, 'i' as u8, 'o' as u8, 'u' as u8}
i32[3] coords = {10, 20, 30}
```

The feature is restricted to `VarDecl` initializer position for local variables.
Global arrays with list initializers are **out of scope** for this plan (LLVM requires
constant global initializers, which would need a separate constant-folding pass).

## Syntax

```tjlb
<array-type> <name> = { <expr>, <expr>, ... }
```

- The element count in the initializer must exactly match the array's declared size.
- Each element expression must be compatible with the array's element type.
- Elements are evaluated left-to-right in source order.
- Trailing commas are not allowed (consistent with function call argument syntax).

## Changes Required

### 1. AST — `src/parser/ast.rs`

Add a new variant to `ExprKind`:

```rust
ListInit {
    elements: Vec<Expression>,
}
```

The `Expression` wrapper's `expr_type` field carries the full array type (set by the
parser, which already knows the target type when parsing a `VarDecl` initializer).

### 2. Parser — `src/parser/expressions.rs`

**In `parse_primary`**: add an `OpenBrace` arm that parses a comma-separated list of
expressions, then closes with `CloseBrace`.

```rust
TokenKind::OpenBrace => {
    self.advance(); // consume '{'
    let mut elements = Vec::new();
    if !self.check(&TokenKind::CloseBrace) {
        loop {
            elements.push(self.parse_expression()?);
            if !self.match_token(&[TokenKind::Comma]) { break; }
        }
    }
    self.expect(&TokenKind::CloseBrace, "}")?;
    // Type is unknown at this point; set to void as placeholder.
    // generate_var_decl will use the declared var_type directly.
    Ok(Expression::new(
        ExprKind::ListInit { elements },
        LangType::new(TypeBase::Void, 0, 0, false),
        pos,
    ))
}
```

The placeholder `void` type is intentional — `ListInit` is only valid as a `VarDecl`
initializer, so the typechecker and codegen always have the target array type from the
declaration, not from the expression itself.

### 3. Typechecker — `src/typechecker/checker.rs`

Add a `ListInit` arm in `resolve_expression_type`:

```rust
ExprKind::ListInit { elements } => {
    // Push element-level Compatible constraints.
    // The element type is the array base type with pointer_depth 0 and array_size None.
    // The declared array type is available through the VarDecl constraint pushed by
    // the statement handler; here we just recurse into each element.
    for elem in elements {
        self.resolve_expression_type(elem);
    }
    expr.expr_type // void placeholder — VarDecl handler owns the array-level constraint
}
```

**In the `VarDecl` branch of `check_statement`**, add a guard before pushing the
`Compatible` constraint: when the initializer is a `ListInit`, skip the top-level
`Compatible` constraint (void cannot be compared to an array type) and instead push
per-element constraints:

```rust
StatementKind::VarDecl { var_type, name, initializer } => {
    self.define_var(name.clone(), *var_type);
    if let Some(init_expr) = initializer {
        if let ExprKind::ListInit { elements } = &init_expr.kind {
            // Validate element count
            if let Some(expected_count) = var_type.array_size {
                if elements.len() != expected_count as usize {
                    self.constraints.push(TypeConstraint::ListInitLengthMismatch {
                        expected: expected_count as usize,
                        found: elements.len(),
                        pos: init_expr.pos,
                    });
                }
            }
            // Validate each element type against the array element type
            let elem_type = LangType {
                array_size: None,
                pointer_depth: 0,
                ..*var_type
            };
            for elem in elements {
                let elem_found = self.resolve_expression_type(elem);
                self.constraints.push(TypeConstraint::Compatible {
                    expected: elem_type,
                    found: elem_found,
                    pos: elem.pos,
                    context: ConstraintContext::Initialization,
                });
            }
        } else {
            // Existing scalar initializer path
            let init_type = self.resolve_expression_type(init_expr);
            self.constraints.push(TypeConstraint::Compatible {
                expected: *var_type,
                found: init_type,
                pos: init_expr.pos,
                context: ConstraintContext::Initialization,
            });
        }
    }
}
```

Add the new constraint variant to the `TypeConstraint` enum in `src/typechecker/types.rs`
(or wherever constraints are defined):

```rust
ListInitLengthMismatch { expected: usize, found: usize, pos: Position },
```

And handle it in the constraint solver / error formatter.

### 4. Codegen — `src/codegen/generator.rs`

**In `generate_var_decl`**, change the early-return guard for arrays so it only skips
when there is no `ListInit` initializer:

```rust
// Before (skips ALL array initializers):
if var_type.is_array() {
    return Ok(());
}

// After:
if var_type.is_array() {
    if let Some(Expression { kind: ExprKind::ListInit { elements }, .. }) = initializer {
        return self.generate_list_init(alloca, var_type, elements, pos);
    }
    return Ok(()); // uninitialized array — zero-fill already done by alloca
}
```

Add a new helper method `generate_list_init`:

```rust
fn generate_list_init(
    &mut self,
    array_ptr: PointerValue<'ctx>,
    var_type: &LangType,
    elements: &[Expression],
    pos: crate::lexer::Position,
) -> Result<(), CodegenError> {
    let elem_lang_type = LangType { array_size: None, pointer_depth: 0, ..*var_type };
    let elem_llvm_type = lang_type_to_llvm(self.context, &elem_lang_type)?;

    for (i, elem_expr) in elements.iter().enumerate() {
        let index = self.context.i64_type().const_int(i as u64, false);
        // GEP: compute pointer to array_ptr[i]
        let elem_ptr = unsafe {
            self.builder.build_gep(
                elem_llvm_type,
                array_ptr,
                &[index],
                &format!("list_init.{i}"),
            )?
        };
        // Generate the element value, casting literals to the target element type
        let value = match &elem_expr.kind {
            ExprKind::Literal(lit @ LiteralValue::Integer(_))
                if elem_lang_type.pointer_depth == 0 =>
            {
                self.generate_literal_typed(lit, &elem_lang_type, elem_expr.pos)?
            }
            ExprKind::Literal(lit @ LiteralValue::Float(_))
                if elem_lang_type.pointer_depth == 0 =>
            {
                self.generate_literal_typed(lit, &elem_lang_type, elem_expr.pos)?
            }
            _ => {
                let mut val = self.generate_expression(elem_expr)?;
                if val.get_type() != elem_llvm_type.into() {
                    val = self.cast_value(
                        val,
                        elem_llvm_type.into(),
                        &elem_expr.expr_type,
                        &elem_lang_type,
                    )?;
                }
                val
            }
        };
        self.builder.build_store(elem_ptr, value)?;
    }
    Ok(())
}
```

> **GEP note**: `build_gep` takes the element type (not the array type), the pointer to
> the first element, and a list of indices. With a flat array and a single integer index
> this directly yields a pointer to `array_ptr[i]`.

### 5. Syntax Highlighting — `vscode-tjlb/syntaxes/tjlb.tmLanguage.json`

No changes needed: `{` and `}` are already tokenised as punctuation and the grammar
does not need to distinguish expression braces from block braces.

## Files to Modify

| File | Change |
|---|---|
| `src/parser/ast.rs` | Add `ListInit` variant to `ExprKind` |
| `src/parser/expressions.rs` | Parse `{ ... }` in `parse_primary` |
| `src/typechecker/checker.rs` | Handle `ListInit` in `VarDecl` branch and `resolve_expression_type` |
| `src/typechecker/types.rs` (or errors) | Add `ListInitLengthMismatch` constraint/error variant |
| `src/codegen/generator.rs` | Patch early-return guard; add `generate_list_init` |

## Test Plan

### Positive cases

1. **Basic integer array** — `i32[3] a = {1, 2, 3}`, read back each element and verify.
2. **u8 array with casts** — `u8[4] b = {72 as u8, 105 as u8, 33 as u8, 0 as u8}`, pass to `puts`.
3. **f64 array** — `f64[2] c = {3.14, 2.71}`, verify elements via arithmetic.
4. **Single-element array** — `i32[1] d = {42}`.
5. **Mixed expressions** — elements that are not plain literals, e.g. `{x + 1, foo(y)}`.
6. **Immediately after declaration** — use the array in the same statement block.
7. **Uninitialized array still works** — `u8[8] buf` without an initializer compiles and behaves as before.

### Negative cases (error reporting)

8. **Count mismatch (too few)** — `i32[3] a = {1, 2}` → error naming expected/found counts.
9. **Count mismatch (too many)** — `i32[3] a = {1, 2, 3, 4}` → same.
10. **Type mismatch in element** — `i32[2] a = {1, 3.14}` → type error on the float element.
11. **ListInit on non-array** — `i32 x = {1}` → meaningful error (not a crash).
12. **ListInit on global** — `i32[3] g = {1, 2, 3}` at global scope → clear "not supported" error.

### Integration test

Add `demos/list_init.tjlb` that exercises several array types and returns a checkable
exit code, following the pattern of `demos/types.tjlb`.

## Implementation Order

1. `src/parser/ast.rs` — add `ListInit` variant
2. `src/parser/expressions.rs` — parse `{ ... }` in `parse_primary`
3. `src/typechecker/checker.rs` + error types — handle `ListInit` in `VarDecl`, add `ListInitLengthMismatch`
4. `src/codegen/generator.rs` — patch guard + implement `generate_list_init`
5. Write unit / integration tests and `demos/list_init.tjlb`
