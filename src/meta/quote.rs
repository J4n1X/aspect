//! Transforms Slice 2b, Slices B–E: desugars `quote { ... }` templates into
//! ordinary Aspect AST — calls to the `Ast.*` builders — before typecheck
//! ever sees a meta function's body. See `doc/plans/Quote-Plan.md` §2 for the
//! design and the full lowering table. A quote's body is always a statement
//! sequence (§1); this pass currently implements only a body ending in
//! `return <expr>` (a value quote) whose statements are `Return`/expression-
//! statements — `VarDecl` (§3's hygiene) and void quotes (§4) land next.

use std::collections::HashMap;

use crate::lexer::{LangType, Position, TypeBase};
use crate::parser::ast::{ExprKind, Expression, FunctionBody, LiteralValue, Statement, StatementKind};
use crate::parser::{ParserError, Program};
use crate::symbol::module::mangle_method;

/// Desugar every `quote { ... }` in a `meta_kind.is_some()` function body into
/// `Ast.*` builder calls. Run once, after parsing and before
/// `elaborate_program`, in both entry points (`build_program`, the
/// integration-test harness) — right after the meta-scope gate, mirroring
/// where that gate runs. Ordinary functions are untouched: a `Quote`
/// surviving there is the gate's error, not this pass's job.
///
/// # Errors
/// One [`ParserError::UnsupportedQuoteShape`] per template construct `lower`
/// doesn't implement yet, collected across the whole program — mirroring
/// [`crate::parser::expressions::Parser::parse_program`]'s own
/// accumulate-then-report model — rather than stopping at the first.
pub fn desugar_quotes(program: &mut Program) -> Result<(), Vec<ParserError>> {
    let mut errors = Vec::new();
    for func in &mut program.functions {
        if func.proto.meta_kind.is_none() {
            continue;
        }
        if let FunctionBody::Aspect(body) = &mut func.body {
            for stmt in body {
                desugar_stmt(stmt, &mut program.string_literals, &mut errors);
            }
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Find every `Quote` inside `stmt` (at any nesting depth) and replace it
/// in place. Mirrors `meta::walk::walk_stmt`'s traversal shape, but mutably —
/// that shared walker is read-only (built for the two hook-#3 AST scanners),
/// so a rewrite needs its own.
fn desugar_stmt(stmt: &mut Statement, string_literals: &mut Vec<String>, errors: &mut Vec<ParserError>) {
    match &mut stmt.kind {
        StatementKind::Expression(e) | StatementKind::Return(Some(e)) => {
            desugar_expr(e, string_literals, errors);
        }
        StatementKind::Return(None) | StatementKind::Break | StatementKind::Continue => {}
        StatementKind::Block(body) => body.iter_mut().for_each(|s| desugar_stmt(s, string_literals, errors)),
        StatementKind::If {
            condition,
            then_block,
            else_block,
        } => {
            desugar_expr(condition, string_literals, errors);
            then_block.iter_mut().for_each(|s| desugar_stmt(s, string_literals, errors));
            if let Some(eb) = else_block {
                eb.iter_mut().for_each(|s| desugar_stmt(s, string_literals, errors));
            }
        }
        StatementKind::While { condition, body } => {
            desugar_expr(condition, string_literals, errors);
            body.iter_mut().for_each(|s| desugar_stmt(s, string_literals, errors));
        }
        StatementKind::For {
            init,
            condition,
            increment,
            body,
        } => {
            if let Some(s) = init {
                desugar_stmt(s, string_literals, errors);
            }
            if let Some(c) = condition {
                desugar_expr(c, string_literals, errors);
            }
            if let Some(s) = increment {
                desugar_stmt(s, string_literals, errors);
            }
            body.iter_mut().for_each(|s| desugar_stmt(s, string_literals, errors));
        }
        StatementKind::VarDecl { initializer, .. } => {
            if let Some(e) = initializer {
                desugar_expr(e, string_literals, errors);
            }
        }
        StatementKind::VarAssign { value, .. } => desugar_expr(value, string_literals, errors),
        StatementKind::DerefAssign { target, value } | StatementKind::FieldAssign { target, value } => {
            desugar_expr(target, string_literals, errors);
            desugar_expr(value, string_literals, errors);
        }
    }
}

/// Find every `Quote` inside `expr` (at any nesting depth) and replace it in
/// place with `lower_body`'s output. A node that is itself a `Quote` is
/// handled directly; everything else recurses into its children looking for
/// one nested deeper (e.g. as a function-call argument).
fn desugar_expr(expr: &mut Expression, string_literals: &mut Vec<String>, errors: &mut Vec<ParserError>) {
    if matches!(expr.kind, ExprKind::Quote { .. }) {
        let pos = expr.pos;
        let ExprKind::Quote { body } = std::mem::replace(&mut expr.kind, ExprKind::Null) else {
            unreachable!("just matched Quote above");
        };
        match lower_body(body, pos, string_literals) {
            Ok(lowered) => *expr = lowered,
            Err(e) => errors.push(e), // expr stays the ExprKind::Null placeholder; the collected error aborts the build regardless.
        }
        return;
    }
    match &mut expr.kind {
        ExprKind::Binary { left, right, .. } | ExprKind::Comparison { left, right, .. } => {
            desugar_expr(left, string_literals, errors);
            desugar_expr(right, string_literals, errors);
        }
        ExprKind::Reference(inner)
        | ExprKind::Dereference(inner)
        | ExprKind::UnaryNot(inner)
        | ExprKind::BitwiseNot(inner)
        | ExprKind::Cast { expr: inner, .. }
        | ExprKind::FieldAccess { base: inner, .. }
        | ExprKind::Splice(inner) => desugar_expr(inner, string_literals, errors),
        ExprKind::FunctionCall { args, .. } => {
            args.iter_mut().for_each(|a| desugar_expr(a, string_literals, errors));
        }
        ExprKind::IndirectCall { callee, args } => {
            desugar_expr(callee, string_literals, errors);
            args.iter_mut().for_each(|a| desugar_expr(a, string_literals, errors));
        }
        ExprKind::MethodCall { base, args, .. } => {
            desugar_expr(base, string_literals, errors);
            args.iter_mut().for_each(|a| desugar_expr(a, string_literals, errors));
        }
        ExprKind::StructLiteral { fields, .. } => {
            fields.iter_mut().for_each(|(_, fe)| desugar_expr(fe, string_literals, errors));
        }
        ExprKind::Alloc { count, .. } => desugar_expr(count, string_literals, errors),
        ExprKind::ListInitializer(items) => items.iter_mut().for_each(|x| desugar_expr(x, string_literals, errors)),
        ExprKind::ValueBlock(stmts) => stmts.iter_mut().for_each(|s| desugar_stmt(s, string_literals, errors)),
        ExprKind::Literal(_)
        | ExprKind::Variable(_)
        | ExprKind::EnumValue { .. }
        | ExprKind::FunctionRef(_)
        | ExprKind::SizeOf(_)
        | ExprKind::Null => {}
        ExprKind::Quote { .. } => unreachable!("handled by the early return above"),
    }
}

/// Lower a `Quote`'s whole body (`doc/plans/Quote-Plan.md` §2's `lower_body`).
/// `quote_pos` is the `quote { ... }` expression's own position, used only
/// for diagnostics with no more specific statement to point at (an empty
/// body). Builds `StmtList.new().push(...).push(...)` then wraps with
/// `Ast.value_block(...)` (value-producing — the body's last statement is
/// `return <expr>`, `Expr`-typed) or `Ast.block(...)` (void — `Stmt`-typed;
/// **not** another `ValueBlock`, which is unconditionally value-producing —
/// `Ast.block` targets `StatementKind::Block` instead, the same node an
/// ordinary `{ }` block *statement* produces).
fn lower_body(
    body: Vec<Statement>,
    quote_pos: Position,
    string_literals: &mut Vec<String>,
) -> Result<Expression, ParserError> {
    let is_value = matches!(
        body.last(),
        Some(Statement {
            kind: StatementKind::Return(Some(_)),
            ..
        })
    );
    for (i, stmt) in body.iter().enumerate() {
        if matches!(stmt.kind, StatementKind::Return(_)) && i + 1 != body.len() {
            return Err(ParserError::UnsupportedQuoteShape(
                "a `return` that isn't the template's last statement".to_string(),
                stmt.pos,
            ));
        }
    }

    // Hygiene (§3): a `VarDecl` is v1's only binder. Rename map is a fixed,
    // compile-time substitution — `orig` -> `orig + "$hyg"` — not a runtime
    // gensym: `$` can't appear in a real Aspect identifier, so the renamed
    // form can never collide with anything real source (or another
    // independently-hygiened template) could write. This fixes *capture*
    // (a binder's own declaration is visible, unbound, while its initializer
    // is checked — `define_var` runs before `check_initializer` — so it can
    // shadow a same-named spliced reference to an outer variable); it does
    // not need to be unique *across firings*, since sibling constructed
    // `ValueBlock`s already get their own checker scope.
    let rename: HashMap<String, String> = body
        .iter()
        .filter_map(|s| match &s.kind {
            StatementKind::VarDecl { name, .. } => Some((name.clone(), format!("{name}$hyg"))),
            _ => None,
        })
        .collect();
    // `StmtList.new()` becomes `.push(...)`'s *receiver* below, which needs a
    // real resolved type (a plain `FunctionCall` here would leave it
    // `UNRESOLVED` — `synth_expression`'s `FunctionCall` arm trusts a node's
    // pre-stamped `expr_type` rather than deriving one from `check_call`, an
    // invariant only the parser normally upholds). A deferred static
    // `MethodCall` — `StmtList.new()` exactly as the parser would represent
    // it — routes through `build_static_method_call`, which looks the return
    // type up for real.
    let mut list = Expression::new(
        ExprKind::MethodCall {
            base: Box::new(Expression::new(
                ExprKind::Variable("StmtList".to_string()),
                LangType::UNRESOLVED,
                quote_pos,
            )),
            name: "new".to_string(),
            args: Vec::new(),
        },
        LangType::UNRESOLVED,
        quote_pos,
    );
    for stmt in body {
        let stmt_pos = stmt.pos;
        let built = lower_stmt(stmt, &rename, string_literals)?;
        list = Expression::new(
            ExprKind::MethodCall {
                base: Box::new(list),
                name: "push".to_string(),
                args: vec![built],
            },
            LangType::UNRESOLVED,
            stmt_pos,
        );
    }
    let wrapper = if is_value { "value_block" } else { "block" };
    Ok(Expression::new(
        ExprKind::FunctionCall {
            name: mangle_method("Ast", wrapper),
            args: vec![list],
        },
        LangType::UNRESOLVED,
        quote_pos,
    ))
}

/// Lower one template statement into an expression that, when evaluated in
/// the handler, produces a `Stmt` handle. `rename` maps an original binder
/// name to its hygiene-renamed form (§3).
fn lower_stmt(
    stmt: Statement,
    rename: &HashMap<String, String>,
    string_literals: &mut Vec<String>,
) -> Result<Expression, ParserError> {
    let pos = stmt.pos;
    match stmt.kind {
        StatementKind::Return(Some(e)) => {
            let e = lower(e, rename, string_literals)?;
            Ok(Expression::new(
                ExprKind::FunctionCall {
                    name: mangle_method("Ast", "return_stmt"),
                    args: vec![e],
                },
                LangType::UNRESOLVED,
                pos,
            ))
        }
        StatementKind::Expression(e) => {
            let e = lower(e, rename, string_literals)?;
            Ok(Expression::new(
                ExprKind::FunctionCall {
                    name: mangle_method("Ast", "expr_stmt"),
                    args: vec![e],
                },
                LangType::UNRESOLVED,
                pos,
            ))
        }
        StatementKind::VarDecl {
            var_type,
            name,
            initializer: Some(init),
        } => {
            let Some((kind_tag, size_bits, pointer_depth)) = quote_vardecl_type_tag(&var_type) else {
                return Err(ParserError::UnsupportedQuoteShape(
                    format!("a `{var_type}` local (only integer, bool, or a single pointer to one, are supported)"),
                    pos,
                ));
            };
            let renamed = rename
                .get(&name)
                .expect("every VarDecl name in this body was collected into `rename`");
            let name_arg = string_literal_expr(string_literals, renamed, pos);
            let init = lower(init, rename, string_literals)?;
            Ok(Expression::new(
                ExprKind::FunctionCall {
                    name: mangle_method("Ast", "vardecl"),
                    args: vec![
                        int_literal_expr(kind_tag, pos),
                        int_literal_expr(size_bits, pos),
                        int_literal_expr(pointer_depth, pos),
                        name_arg,
                        init,
                    ],
                },
                LangType::UNRESOLVED,
                pos,
            ))
        }
        StatementKind::VarDecl { initializer: None, .. } => Err(ParserError::UnsupportedQuoteShape(
            "an uninitialized variable declaration".to_string(),
            pos,
        )),
        other => Err(ParserError::UnsupportedQuoteShape(describe_stmt_shape(&other), pos)),
    }
}

/// `Ast.vardecl`'s type encoding for v1: an integer, bool, or a single
/// pointer to one — mirrors (and is slightly broader than, allowing one
/// level of pointer) `src/parser/meta.rs`'s `is_meta_scalar` restriction on
/// `meta` globals. Returns `(kind_tag, size_bits, pointer_depth)`, `kind_tag`
/// the frozen `TypeKind` ABI position (`Meta-Module-JIT-Interface.md` §7).
fn quote_vardecl_type_tag(ty: &LangType) -> Option<(i32, i32, i32)> {
    if ty.is_array() || ty.pointer_depth > 1 {
        return None;
    }
    let kind_tag = match ty.base {
        TypeBase::SInt => 0,
        TypeBase::UInt => 1,
        TypeBase::Bool => 4,
        _ => return None,
    };
    Some((kind_tag, ty.size_bits as i32, ty.pointer_depth as i32))
}

/// An `i32` literal expression, for `Ast.vardecl`'s type-encoding arguments.
fn int_literal_expr(value: i32, pos: Position) -> Expression {
    Expression::new(ExprKind::Literal(LiteralValue::Integer(i64::from(value))), LangType::I32, pos)
}

/// Lower one expression from inside a `Quote`'s body into ordinary Aspect
/// AST that calls the `Ast.*` builders (`doc/plans/Quote-Plan.md` §2's
/// `lower(t)`). Implements `Splice`, a zero-arg `MethodCall`, and a bare
/// `Variable` (quote-mode's deferred bare-identifier node, §1) — the rest of
/// the table is additive, filled in as later template shapes need it; every
/// other shape is a positioned error, not a panic.
fn lower(
    template: Expression,
    rename: &HashMap<String, String>,
    string_literals: &mut Vec<String>,
) -> Result<Expression, ParserError> {
    let pos = template.pos;
    match template.kind {
        // `$(expr)` unwraps to `expr`, but `expr` may itself contain an
        // undesugared nested `Quote` — quote-mode is suspended, not raised,
        // while parsing a splice's interior, so `$(quote { ... })` parses.
        // Left un-desugared, a stray `Quote` reaching the checker/codegen's
        // exhaustive matches is an `unreachable!()` panic, not a diagnostic —
        // recurse through the same find-and-lower-any-`Quote` pass ordinary
        // code gets before returning it.
        ExprKind::Splice(inner) => {
            let mut inner = *inner;
            let mut nested_errors = Vec::new();
            desugar_expr(&mut inner, string_literals, &mut nested_errors);
            if let Some(e) = nested_errors.into_iter().next() {
                return Err(e);
            }
            Ok(inner)
        }
        // `base.name()` (no args, quote-mode's deferred node — see
        // `Parser::parse_dot_postfix`) → `Ast.method(lower(base), "name")`.
        ExprKind::MethodCall { base, name, args } if args.is_empty() => {
            let base = lower(*base, rename, string_literals)?;
            let name_arg = string_literal_expr(string_literals, &name, pos);
            Ok(Expression::new(
                ExprKind::FunctionCall {
                    name: mangle_method("Ast", "method"),
                    args: vec![base, name_arg],
                },
                LangType::UNRESOLVED,
                pos,
            ))
        }
        // A bare identifier inside the template — quote-mode's deferred
        // bare-identifier node (§1). A binder (renamed) or a free identifier
        // (unhygienic by design, §3) either way become `Ast.var(<name>)`.
        ExprKind::Variable(name) => {
            let resolved = rename.get(&name).cloned().unwrap_or(name);
            let name_arg = string_literal_expr(string_literals, &resolved, pos);
            Ok(Expression::new(
                ExprKind::FunctionCall {
                    name: mangle_method("Ast", "var"),
                    args: vec![name_arg],
                },
                LangType::UNRESOLVED,
                pos,
            ))
        }
        other => Err(ParserError::UnsupportedQuoteShape(describe_shape(&other), pos)),
    }
}

/// Intern `s` into the program's string-literal table, deduping like the
/// parser's own `string_literal()` so repeated method names across templates
/// don't grow the table pointlessly.
fn string_literal_expr(string_literals: &mut Vec<String>, s: &str, pos: Position) -> Expression {
    let index = string_literals
        .iter()
        .position(|existing| existing == s)
        .unwrap_or_else(|| {
            string_literals.push(s.to_string());
            string_literals.len() - 1
        });
    Expression::new(ExprKind::Literal(LiteralValue::String(index)), LangType::U8_PTR, pos)
}

/// A short, human-readable name for a template expression shape `lower`
/// doesn't implement yet, for the `UnsupportedQuoteShape` diagnostic.
fn describe_shape(kind: &ExprKind) -> String {
    match kind {
        ExprKind::Binary { .. } | ExprKind::Comparison { .. } => "a binary operator".to_string(),
        ExprKind::MethodCall { .. } => "a method call with arguments".to_string(),
        ExprKind::FieldAccess { .. } => "a field access".to_string(),
        ExprKind::FunctionCall { .. } => "a function call".to_string(),
        ExprKind::Literal(_) => "a literal".to_string(),
        _ => "this expression form".to_string(),
    }
}

/// A short, human-readable name for a template statement shape `lower_stmt`
/// doesn't implement yet, for the `UnsupportedQuoteShape` diagnostic.
fn describe_stmt_shape(kind: &StatementKind) -> String {
    match kind {
        StatementKind::VarDecl { .. } => "a variable declaration".to_string(),
        StatementKind::If { .. } => "an `if`".to_string(),
        StatementKind::While { .. } => "a `while` loop".to_string(),
        StatementKind::For { .. } => "a `for` loop".to_string(),
        StatementKind::Block(_) => "a nested block".to_string(),
        StatementKind::VarAssign { .. } | StatementKind::DerefAssign { .. } | StatementKind::FieldAssign { .. } => {
            "an assignment".to_string()
        }
        _ => "this statement form".to_string(),
    }
}
