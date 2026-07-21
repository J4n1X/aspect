//! Tier-1 query index: cheap dictionaries built in a single post-typecheck AST
//! walk. Phase 2a ships only what the shipped builtins and rule-anchor
//! validation need — per-struct construction sites, and attribute carriers.
//! The layer is designed to grow (call sites, module-of, …) without changing
//! this shape (§7 of `doc/plans/Three-Hook-Metasystem.md`).

use std::collections::HashMap;

use crate::lexer::{Position, TypeBase};
use crate::parser::ast::{Attribute, ExprKind, Expression, FunctionBody, Statement, StatementKind};
use crate::parser::Program;

/// Post-typecheck dictionaries over a program. Borrows the program for the
/// lifetime of a rule run.
pub struct QueryIndex<'a> {
    program: &'a Program,
    /// struct id → every **construction** site of that struct.
    instantiations: HashMap<u32, Vec<Position>>,
    /// attribute name → the position of every carrier.
    attr_carriers: HashMap<String, Vec<Position>>,
}

impl<'a> QueryIndex<'a> {
    /// Build the index in one walk of the typed AST.
    #[must_use]
    pub fn build(program: &'a Program) -> Self {
        let mut idx = QueryIndex {
            program,
            instantiations: HashMap::new(),
            attr_carriers: HashMap::new(),
        };
        for func in &program.functions {
            idx.record_attrs(&func.proto.attrs);
            if let FunctionBody::Aspect(body) = &func.body {
                for stmt in body {
                    idx.walk_stmt(stmt);
                }
            }
        }
        for global in &program.global_vars {
            idx.record_attrs(&global.attrs);
            if let Some(init) = &global.initializer {
                idx.walk_expr(init);
            }
        }
        for s in program.symbols.structs() {
            idx.record_attrs(&s.attrs);
            for field in &s.fields {
                idx.record_attrs(&field.attrs);
            }
        }
        idx
    }

    /// Every construction site of struct `id`. "Construction" is a struct
    /// literal or an `alloc` of the value type; source order is not promised.
    /// Deliberate v1 blind spots (not counted): value copies (`T b = a`),
    /// uninitialized declarations, arrays, by-value parameters, struct-returning
    /// calls, and embedded struct-typed fields.
    #[must_use]
    pub fn instantiations_of(&self, id: u32) -> &[Position] {
        self.instantiations.get(&id).map_or(&[], Vec::as_slice)
    }

    /// The position of every site carrying attribute `name` (`@name` → `name`).
    #[must_use]
    pub fn attr_carriers(&self, name: &str) -> Vec<Position> {
        self.attr_carriers.get(name).cloned().unwrap_or_default()
    }

    /// Declared name of struct `id`, for judgment messages.
    #[must_use]
    pub fn struct_name(&self, id: u32) -> &str {
        &self.program.symbols.struct_info(id).name
    }

    fn record_attrs(&mut self, attrs: &[Attribute]) {
        for attr in attrs {
            self.attr_carriers
                .entry(attr.name.clone())
                .or_default()
                .push(attr.pos);
        }
    }

    fn walk_stmt(&mut self, stmt: &Statement) {
        self.record_attrs(&stmt.attrs);
        match &stmt.kind {
            StatementKind::Expression(e) => self.walk_expr(e),
            StatementKind::Block(body) => body.iter().for_each(|s| self.walk_stmt(s)),
            StatementKind::Return(Some(e)) => self.walk_expr(e),
            StatementKind::Return(None) | StatementKind::Break | StatementKind::Continue => {}
            StatementKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.walk_expr(condition);
                then_block.iter().for_each(|s| self.walk_stmt(s));
                if let Some(eb) = else_block {
                    eb.iter().for_each(|s| self.walk_stmt(s));
                }
            }
            StatementKind::While { condition, body } => {
                self.walk_expr(condition);
                body.iter().for_each(|s| self.walk_stmt(s));
            }
            StatementKind::For {
                init,
                condition,
                increment,
                body,
            } => {
                if let Some(s) = init {
                    self.walk_stmt(s);
                }
                if let Some(c) = condition {
                    self.walk_expr(c);
                }
                if let Some(s) = increment {
                    self.walk_stmt(s);
                }
                body.iter().for_each(|s| self.walk_stmt(s));
            }
            StatementKind::VarDecl { initializer, .. } => {
                if let Some(e) = initializer {
                    self.walk_expr(e);
                }
            }
            StatementKind::VarAssign { value, .. } => self.walk_expr(value),
            StatementKind::DerefAssign { target, value }
            | StatementKind::FieldAssign { target, value } => {
                self.walk_expr(target);
                self.walk_expr(value);
            }
        }
    }

    fn walk_expr(&mut self, expr: &Expression) {
        match &expr.kind {
            ExprKind::StructLiteral { struct_id, fields } => {
                self.instantiations.entry(*struct_id).or_default().push(expr.pos);
                for (_, value) in fields {
                    self.walk_expr(value);
                }
            }
            ExprKind::Alloc { alloc_type, count } => {
                if alloc_type.pointer_depth == 0 {
                    if let TypeBase::Struct(id) = alloc_type.base {
                        self.instantiations.entry(id).or_default().push(expr.pos);
                    }
                }
                self.walk_expr(count);
            }
            ExprKind::Binary { left, right, .. } | ExprKind::Comparison { left, right, .. } => {
                self.walk_expr(left);
                self.walk_expr(right);
            }
            ExprKind::Reference(inner)
            | ExprKind::Dereference(inner)
            | ExprKind::UnaryNot(inner)
            | ExprKind::BitwiseNot(inner)
            | ExprKind::Cast { expr: inner, .. }
            | ExprKind::FieldAccess { base: inner, .. } => self.walk_expr(inner),
            ExprKind::FunctionCall { args, .. } => args.iter().for_each(|a| self.walk_expr(a)),
            ExprKind::IndirectCall { callee, args } => {
                self.walk_expr(callee);
                args.iter().for_each(|a| self.walk_expr(a));
            }
            ExprKind::MethodCall { base, args, .. } => {
                self.walk_expr(base);
                args.iter().for_each(|a| self.walk_expr(a));
            }
            ExprKind::ListInitializer(items) => items.iter().for_each(|e| self.walk_expr(e)),
            ExprKind::ValueBlock(stmts) => stmts.iter().for_each(|s| self.walk_stmt(s)),
            ExprKind::Literal(_)
            | ExprKind::Variable(_)
            | ExprKind::EnumValue { .. }
            | ExprKind::FunctionRef(_)
            | ExprKind::SizeOf(_)
            | ExprKind::Null => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::QueryIndex;
    use crate::parser::Parser;

    fn instantiation_count(source: &str, struct_name: &str) -> usize {
        let tokens = crate::lexer::tokenize(source.to_string()).expect("lex");
        let program = Parser::new(tokens).parse_program().expect("parse");
        let id = program.symbols.struct_id(struct_name).expect("struct interned");
        QueryIndex::build(&program).instantiations_of(id).len()
    }

    /// `T v = T { … }` is a *single* construction — the literal is the site, the
    /// declaration is not counted again. This is the blocking correctness
    /// property for the `singleton` rule (no literal + decl double-count).
    #[test]
    fn value_decl_with_literal_counts_once() {
        let count = instantiation_count(
            "type Config {\n    public i32 x\n}\nfn f() -> i32 {\n    Config c = Config { x = 1 }\n    return c.x\n}",
            "Config",
        );
        assert_eq!(count, 1);
    }

    /// A copy (`T b = a`) is not a construction — a documented v1 blind spot.
    #[test]
    fn copy_is_not_a_construction() {
        let count = instantiation_count(
            "type Config {\n    public i32 x\n}\nfn f() -> i32 {\n    Config a = Config { x = 1 }\n    Config b = a\n    return b.x\n}",
            "Config",
        );
        assert_eq!(count, 1);
    }

    /// Two distinct literals are two construction sites.
    #[test]
    fn two_literals_count_twice() {
        let count = instantiation_count(
            "type Config {\n    public i32 x\n}\nfn f() -> i32 {\n    Config a = Config { x = 1 }\n    Config b = Config { x = 2 }\n    return a.x + b.x\n}",
            "Config",
        );
        assert_eq!(count, 2);
    }
}
