use super::errors::TypeCheckError;
use crate::lexer::LangType;
use crate::scope::ScopeStack;
use crate::parser::{
    ExprKind, Expression, Function, FunctionBody, FunctionProto, GlobalVar, Program, Statement,
};
use crate::symbol::module::ModuleSymbols;
use crate::target::TargetSpec;
use std::collections::HashMap;

/// Single-pass, **bidirectional** type checker: errors are reported immediately,
/// with no constraint-collection phase. Every expression is visited in one of
/// two modes.
/// - `synth_expression` *synthesises* a type bottom-up when no surrounding
///   context constrains it (conditions, callees, indices, cast/deref operands).
/// - `check_expression` *checks* an expression against a target type supplied
///   by its context (assignment RHS, `return` value, call arguments,
///   initialisers). It pushes the target down where the child's type *is* the
///   parent's, and **stamps `expr_type` on the AST in place** so codegen reads
///   the final width directly.
pub struct TypeChecker {
    /// Taken from `Program` for the duration of `check_program` and restored
    /// on exit, so any registry refinement is preserved without a divergent copy.
    symbols: ModuleSymbols,
    scopes: ScopeStack<LangType>,
    globals: HashMap<String, LangType>,
    current_function: Option<String>,
    source_files: Vec<std::path::PathBuf>,
    /// Enclosing value-block result types, innermost last: a `return` binds to
    /// the top entry instead of the function. `None` while still undetermined.
    value_block_types: Vec<Option<LangType>>,
    /// Only `asm fn` consults it: register names are validated against this
    /// target's register model, so `rax` under `aarch64-*` is a clean error.
    target: TargetSpec,
    errors: Vec<TypeCheckError>,
    /// Read by the driver *after* a successful `check_program` — they never
    /// appear in the `Err` path and never change the exit code (v1). Both
    /// `main.rs` and the test harness read this, each building its own checker.
    warnings: Vec<super::errors::TypeWarning>,
    /// Transform handlers consulted at demand sites during a round.
    handlers: super::elaborate::HandlerRegistry,
    /// Handler rewrites applied this round; the elaboration driver reads it to
    /// detect quiescence. Only a transform rewrite bumps it — the one-shot
    /// `MethodCall` lowering is core lowering and must not.
    rewrites: usize,
}

impl TypeChecker {
    #[must_use]
    pub fn new() -> Self {
        Self {
            symbols: ModuleSymbols::new(),
            scopes: ScopeStack::new(),
            globals: HashMap::new(),
            current_function: None,
            source_files: Vec::new(),
            value_block_types: Vec::new(),
            target: TargetSpec::host(),
            errors: Vec::new(),
            warnings: Vec::new(),
            handlers: super::elaborate::HandlerRegistry::new(),
            rewrites: 0,
        }
    }

    /// Check against `target` rather than the host. Mirrors
    /// `Preprocessor::for_target`: the two must agree, or an `$ifdef
    /// ARCH_X86_64` guard would admit code the checker then rejects.
    #[must_use]
    pub fn with_target(mut self, target: TargetSpec) -> Self {
        self.target = target;
        self
    }

    /// Format a single error with the originating source file prepended.
    /// Looks up the file via the error's `pos.file_id` so errors inside an
    /// imported file are attributed to that file, not the entry one.
    #[must_use]
    pub fn format_error(&self, err: &TypeCheckError) -> String {
        crate::lexer::format_diagnostic(&self.source_files, err, err.position())
    }

    /// Warnings accumulated during the last `check_program`. Read after a
    /// successful check — `main.rs` prints these to stderr, the test harness
    /// asserts on them (`# expected_warning:`).
    #[must_use]
    pub fn warnings(&self) -> &[super::errors::TypeWarning] {
        &self.warnings
    }

    /// Handler rewrites applied during the last `check_program`; the elaboration
    /// driver reads this to detect the fixpoint (a round with zero is quiescent).
    #[must_use]
    pub fn rewrites(&self) -> usize {
        self.rewrites
    }

    /// Mirrors [`Self::format_error`], as `file:line:col: warning: <message>`.
    #[must_use]
    pub fn format_warning(&self, warn: &super::errors::TypeWarning) -> String {
        let pos = warn.position;
        match self.source_files.get(pos.file_id as usize) {
            Some(path) => format!(
                "{}:{}:{}: warning: {}",
                path.display(),
                pos.line,
                pos.column,
                warn.message
            ),
            None => format!("warning: {}", warn.message),
        }
    }

    /// The AST is taken by mutable reference: the checker stamps the resolved
    /// `expr_type` onto nodes as it pushes target types down into expressions.
    ///
    /// # Errors
    /// Returns `Err(Vec<TypeCheckError>)` listing every type error found.
    pub fn check_program(&mut self, program: &mut Program) -> Result<(), Vec<TypeCheckError>> {
        self.symbols = std::mem::take(&mut program.symbols);
        // Adopt the file registry that rides on `Program` (unless one was
        // already set), so diagnostics resolve `pos.file_id` to a filename.
        if self.source_files.is_empty() {
            self.source_files = program.source_files.clone();
        }

        self.register_declarations(program);

        for global in &mut program.global_vars {
            self.check_global_var(global);
        }

        for Function { proto, body } in &mut program.functions {
            self.check_proto(proto);
            match body {
                // An asm fn has no statements to walk; it has a register
                // contract to validate instead.
                FunctionBody::Asm(asm) => self.check_asm_function(proto, asm),
                // A naked fn's body is opaque assembly with no register
                // contract to validate; its params/return follow the raw ABI.
                FunctionBody::Naked(_) => {}
                FunctionBody::Aspect(stmts) => self.check_function(proto, stmts),
                FunctionBody::Extern => {}
            }
        }

        program.symbols = std::mem::take(&mut self.symbols);

        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(self.errors.drain(..).collect())
        }
    }

    // ── Declaration registration ─────────────────────────────────────────────

    fn register_declarations(&mut self, program: &Program) {
        // Function signatures already live in `self.symbols` (built by the
        // parser); only globals need a checker-local index for fast lookup.
        for global in &program.global_vars {
            self.globals.insert(global.name.clone(), global.var_type);
        }
    }

    // ── Global variable checking ─────────────────────────────────────────────

    /// Push an `InvalidVoidValue` error if `ty` is a bare `void` value; a
    /// `void` pointer (`u0*`) is fine. Shared by the decl/param/alloc checks.
    fn reject_void_value(&mut self, ty: LangType, pos: crate::lexer::Position) {
        if ty.is_void_value() {
            self.errors.push(TypeCheckError::InvalidVoidValue(pos));
        }
    }

    fn check_global_var(&mut self, global: &mut GlobalVar) {
        let var_type = global.var_type;
        self.reject_void_value(var_type, global.pos);
        if let Some(init_expr) = &mut global.initializer {
            self.check_initializer(init_expr, &var_type);
        }
    }

    /// Shared by global and local declarations.
    fn check_initializer(&mut self, init_expr: &mut Expression, var_type: &LangType) {
        let init_pos = init_expr.pos;
        if let ExprKind::ListInitializer(elements) = &mut init_expr.kind {
            if let Some(expected) = var_type.array_size
                && elements.len() > expected as usize
            {
                self.errors.push(TypeCheckError::ListInitLengthMismatch {
                    expected: expected as usize,
                    found: elements.len(),
                    position: init_pos,
                });
            }
            let elem_type = var_type.element_type();
            for elem in elements.iter_mut() {
                self.check_expression(elem, &elem_type);
            }
        } else {
            self.check_expression(init_expr, var_type);
        }
    }

    // ── Function checking ────────────────────────────────────────────────────

    /// Validate a function prototype: no `u0`-valued parameters. Runs for
    /// extern declarations too (they never reach `check_function`).
    fn check_proto(&mut self, proto: &crate::parser::FunctionProto) {
        for (param_type, _) in &proto.params {
            self.reject_void_value(*param_type, proto.pos);
        }
    }

    fn check_function(&mut self, proto: &FunctionProto, stmts: &mut [Statement]) {
        self.current_function = Some(proto.name.clone());
        self.enter_scope();

        for (param_type, param_name) in &proto.params {
            self.define_var(param_name.clone(), *param_type);
        }

        for stmt in stmts {
            self.check_statement(stmt);
        }

        self.exit_scope();
        self.current_function = None;
    }

    // ── Scope helpers ────────────────────────────────────────────────────────

    fn enter_scope(&mut self) {
        self.scopes.enter();
    }

    fn exit_scope(&mut self) {
        self.scopes.exit();
    }

    fn define_var(&mut self, name: String, var_type: LangType) {
        self.scopes.insert(name, var_type);
    }

    fn lookup_var(&self, name: &str) -> Option<LangType> {
        self.scopes
            .lookup(name)
            .copied()
            .or_else(|| self.globals.get(name).copied())
    }
}

impl Default for TypeChecker {
    fn default() -> Self {
        Self::new()
    }
}

mod asm;
mod expressions;
mod statements;

#[cfg(test)]
mod tests;
