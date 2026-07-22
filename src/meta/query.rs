//! Tier-1 query index: cheap dictionaries built in a single post-typecheck AST
//! walk. Phase 2a ships only what the shipped builtins and rule-anchor
//! validation need — per-struct construction sites, and attribute carriers.
//! The layer is designed to grow (call sites, module-of, …) without changing
//! this shape (§7 of `doc/plans/Three-Hook-Metasystem.md`).

use std::collections::HashMap;

use crate::lexer::{Position, TypeBase};
use crate::parser::ast::{Attribute, ExprKind, Expression, FunctionBody, Statement};
use crate::parser::Program;

use super::walk;

/// Post-typecheck dictionaries over a program. Borrows the program for the
/// lifetime of a rule run.
pub struct QueryIndex<'a> {
    program: &'a Program,
    /// struct id → every **construction** site of that struct.
    instantiations: HashMap<u32, Vec<Position>>,
    /// struct id → construction sites inside a **function body** (a subset of
    /// `instantiations`). The complement — sites in a global initializer — runs
    /// exactly once, so this is what a bombproof singleton must be empty.
    local_instantiations: HashMap<u32, Vec<Position>>,
    /// attribute name → the position of every carrier.
    attr_carriers: HashMap<String, Vec<Position>>,
    /// callee name (mangled `Type$method` for methods) → every direct call site.
    call_sites: HashMap<String, Vec<Position>>,
    /// Transient walk state: true while walking a function body, false while
    /// walking a global initializer. Routes construction sites to the right bucket.
    in_function: bool,
}

impl<'a> QueryIndex<'a> {
    /// Build the index in one walk of the typed AST.
    #[must_use]
    pub fn build(program: &'a Program) -> Self {
        let mut idx = QueryIndex {
            program,
            instantiations: HashMap::new(),
            local_instantiations: HashMap::new(),
            attr_carriers: HashMap::new(),
            call_sites: HashMap::new(),
            in_function: false,
        };
        idx.in_function = true;
        for func in &program.functions {
            idx.record_attrs(&func.proto.attrs);
            if let FunctionBody::Aspect(body) = &func.body {
                for stmt in body {
                    walk::walk_stmt(stmt, &mut idx);
                }
            }
        }
        idx.in_function = false;
        for global in &program.global_vars {
            idx.record_attrs(&global.attrs);
            if let Some(init) = &global.initializer {
                walk::walk_expr(init, &mut idx);
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

    /// Construction sites of struct `id` that sit inside a **function body**
    /// (not a global initializer). A global initializer runs exactly once, so a
    /// singleton whose only construction is a global init is provably single;
    /// any construction here could execute repeatedly (a loop, or a helper
    /// called more than once) and defeats that guarantee.
    #[must_use]
    pub fn local_instantiations_of(&self, id: u32) -> &[Position] {
        self.local_instantiations.get(&id).map_or(&[], Vec::as_slice)
    }

    /// The position of every site carrying attribute `name` (`@name` → `name`).
    #[must_use]
    pub fn attr_carriers(&self, name: &str) -> Vec<Position> {
        self.attr_carriers.get(name).cloned().unwrap_or_default()
    }

    /// Every direct call site of `name`. Methods are keyed by their mangled
    /// `Type$method` name, so `call_sites_of("Config$new")` finds `Config.new(..)`
    /// calls. Indirect (fn-pointer) calls are not counted.
    #[must_use]
    pub fn call_sites_of(&self, name: &str) -> &[Position] {
        self.call_sites.get(name).map_or(&[], Vec::as_slice)
    }

    /// The whole callee → call-sites map, for snapshotting into a JIT run.
    #[must_use]
    pub fn call_sites(&self) -> &HashMap<String, Vec<Position>> {
        &self.call_sites
    }

    /// Declared name of struct `id`, for judgment messages.
    #[must_use]
    pub fn struct_name(&self, id: u32) -> &str {
        &self.program.symbols.struct_info(id).name
    }

    /// The module a position belongs to (`""` for the anonymous root module).
    #[must_use]
    pub fn module_of(&self, pos: Position) -> &str {
        self.program
            .file_modules
            .get(pos.file_id as usize)
            .map_or("", String::as_str)
    }

    /// Restrict `positions` to those in `module`, or return all when `module` is
    /// `None` (a `public` rule, which is whole-program). This is how a rule's
    /// visibility scopes what it judges.
    #[must_use]
    pub fn in_module(&self, positions: &[Position], module: Option<&str>) -> Vec<Position> {
        match module {
            None => positions.to_vec(),
            Some(m) => positions
                .iter()
                .copied()
                .filter(|p| self.module_of(*p) == m)
                .collect(),
        }
    }

    fn record_attrs(&mut self, attrs: &[Attribute]) {
        for attr in attrs {
            self.attr_carriers
                .entry(attr.name.clone())
                .or_default()
                .push(attr.pos);
        }
    }

    /// Record a construction of struct `id` at `pos` into the all-sites bucket,
    /// and additionally into the function-body bucket when the walk is inside a
    /// function (not a global initializer).
    fn record_construction(&mut self, id: u32, pos: Position) {
        self.instantiations.entry(id).or_default().push(pos);
        if self.in_function {
            self.local_instantiations.entry(id).or_default().push(pos);
        }
    }
}

impl walk::Visitor for QueryIndex<'_> {
    fn visit_stmt(&mut self, stmt: &Statement) {
        self.record_attrs(&stmt.attrs);
    }

    fn visit_expr(&mut self, expr: &Expression) {
        match &expr.kind {
            ExprKind::StructLiteral { struct_id, .. } => {
                self.record_construction(*struct_id, expr.pos);
            }
            ExprKind::Alloc { alloc_type, .. } => {
                if alloc_type.pointer_depth == 0
                    && let TypeBase::Struct(id) = alloc_type.base
                {
                    self.record_construction(id, expr.pos);
                }
            }
            ExprKind::FunctionCall { name, .. } => {
                self.call_sites
                    .entry(name.clone())
                    .or_default()
                    .push(expr.pos);
            }
            _ => {}
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

    /// A global initializer's construction is counted overall but NOT as a
    /// function-body construction; a construction inside a function is both.
    #[test]
    fn local_instantiations_excludes_global_init() {
        let src = "type Config {\n    public i32 x\n}\n\
                   Config g = Config { x = 1 }\n\
                   fn f() -> i32 {\n    Config c = Config { x = 2 }\n    return c.x\n}";
        let tokens = crate::lexer::tokenize(src.to_string()).expect("lex");
        let program = Parser::new(tokens).parse_program().expect("parse");
        let id = program.symbols.struct_id("Config").expect("struct interned");
        let idx = QueryIndex::build(&program);
        assert_eq!(idx.instantiations_of(id).len(), 2, "global + local");
        assert_eq!(idx.local_instantiations_of(id).len(), 1, "only the one in f()");
    }

    /// Direct calls are recorded under the callee name; an uncalled name has none.
    #[test]
    fn call_sites_counts_direct_calls() {
        let src = "fn helper() -> i32 { return 1 }\n\
                   fn f() -> i32 {\n    helper()\n    helper()\n    return 0\n}";
        let tokens = crate::lexer::tokenize(src.to_string()).expect("lex");
        let program = Parser::new(tokens).parse_program().expect("parse");
        let idx = QueryIndex::build(&program);
        assert_eq!(idx.call_sites_of("helper").len(), 2);
        assert_eq!(idx.call_sites_of("absent").len(), 0);
    }
}
