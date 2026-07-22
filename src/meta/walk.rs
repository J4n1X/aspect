//! One depth-first walk of a function body, shared by the two hook-#3 AST
//! scanners: the meta-only gate ([`super::check_meta_gate`]) and the query
//! index ([`super::query::QueryIndex`]). Each consumer implements [`Visitor`]
//! to say *what* happens at a node; the traversal shape lives here so the two
//! cannot drift as the AST grows.

use crate::parser::ast::{ExprKind, Expression, Statement, StatementKind};

/// A node visitor. Both hooks fire in source (pre-)order — a node before its
/// children. Override only the method you need; the other defaults to a no-op.
pub(super) trait Visitor {
    fn visit_stmt(&mut self, _stmt: &Statement) {}
    fn visit_expr(&mut self, _expr: &Expression) {}
}

/// Visit `stmt` and every statement and expression beneath it.
pub(super) fn walk_stmt(stmt: &Statement, v: &mut impl Visitor) {
    v.visit_stmt(stmt);
    match &stmt.kind {
        StatementKind::Expression(e) | StatementKind::Return(Some(e)) => walk_expr(e, v),
        StatementKind::Return(None) | StatementKind::Break | StatementKind::Continue => {}
        StatementKind::Block(body) => body.iter().for_each(|s| walk_stmt(s, v)),
        StatementKind::If {
            condition,
            then_block,
            else_block,
        } => {
            walk_expr(condition, v);
            then_block.iter().for_each(|s| walk_stmt(s, v));
            if let Some(eb) = else_block {
                eb.iter().for_each(|s| walk_stmt(s, v));
            }
        }
        StatementKind::While { condition, body } => {
            walk_expr(condition, v);
            body.iter().for_each(|s| walk_stmt(s, v));
        }
        StatementKind::For {
            init,
            condition,
            increment,
            body,
        } => {
            if let Some(s) = init {
                walk_stmt(s, v);
            }
            if let Some(c) = condition {
                walk_expr(c, v);
            }
            if let Some(s) = increment {
                walk_stmt(s, v);
            }
            body.iter().for_each(|s| walk_stmt(s, v));
        }
        StatementKind::VarDecl { initializer, .. } => {
            if let Some(e) = initializer {
                walk_expr(e, v);
            }
        }
        StatementKind::VarAssign { value, .. } => walk_expr(value, v),
        StatementKind::DerefAssign { target, value }
        | StatementKind::FieldAssign { target, value } => {
            walk_expr(target, v);
            walk_expr(value, v);
        }
    }
}

/// Visit `expr` and every expression (and value-block statement) beneath it.
pub(super) fn walk_expr(expr: &Expression, v: &mut impl Visitor) {
    v.visit_expr(expr);
    match &expr.kind {
        ExprKind::Binary { left, right, .. } | ExprKind::Comparison { left, right, .. } => {
            walk_expr(left, v);
            walk_expr(right, v);
        }
        ExprKind::Reference(inner)
        | ExprKind::Dereference(inner)
        | ExprKind::UnaryNot(inner)
        | ExprKind::BitwiseNot(inner)
        | ExprKind::Cast { expr: inner, .. }
        | ExprKind::FieldAccess { base: inner, .. } => walk_expr(inner, v),
        ExprKind::FunctionCall { args, .. } => args.iter().for_each(|a| walk_expr(a, v)),
        ExprKind::IndirectCall { callee, args } => {
            walk_expr(callee, v);
            args.iter().for_each(|a| walk_expr(a, v));
        }
        ExprKind::MethodCall { base, args, .. } => {
            walk_expr(base, v);
            args.iter().for_each(|a| walk_expr(a, v));
        }
        ExprKind::StructLiteral { fields, .. } => {
            fields.iter().for_each(|(_, fe)| walk_expr(fe, v));
        }
        ExprKind::Alloc { count, .. } => walk_expr(count, v),
        ExprKind::ListInitializer(items) => items.iter().for_each(|x| walk_expr(x, v)),
        ExprKind::ValueBlock(stmts) => stmts.iter().for_each(|s| walk_stmt(s, v)),
        ExprKind::Literal(_)
        | ExprKind::Variable(_)
        | ExprKind::EnumValue { .. }
        | ExprKind::FunctionRef(_)
        | ExprKind::SizeOf(_)
        | ExprKind::Null => {}
    }
}
