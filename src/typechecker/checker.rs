use super::errors::TypeCheckError;
use crate::lexer::LangType;
use crate::scope::ScopeStack;
use crate::parser::{
    ExprKind, Expression, Function, FunctionBody, FunctionProto, GlobalVar, Program, Statement,
};
use crate::symbol::module::ModuleSymbols;
use crate::target::TargetSpec;
use std::collections::HashMap;

/// Single-pass type checker for the Aspect language.
///
/// Walks the AST once and emits errors directly into `self.errors`.
/// No constraint-collection phase — errors are reported immediately upon discovery.
///
/// The checker is **bidirectional**: every expression is visited in one of two
/// modes.
/// - [`TypeChecker::synth_expression`] *synthesises* a type bottom-up when no
///   surrounding context constrains it (conditions, callees, indices, cast and
///   dereference operands).
/// - [`TypeChecker::check_expression`] *checks* an expression against a target
///   type supplied by its context (assignment RHS, `return` value, call
///   arguments, declaration initialisers). It pushes the target down into the
///   children where the child's type *is* the parent's type, and **stamps
///   `expr_type` on the AST in place** so codegen reads the final width directly.
///
/// Use `with_source_file` to include the filename in formatted error messages.
pub struct TypeChecker {
    /// The program's shared symbol table, taken from `Program` for the duration
    /// of `check_program` and restored on exit (so any registry refinement the
    /// checker performs is preserved, without a divergent copy).
    symbols: ModuleSymbols,
    scopes: ScopeStack<LangType>,
    globals: HashMap<String, LangType>,
    current_function: Option<String>,
    /// File registry inherited from the parsed `Program` so error messages
    /// can name the file the error actually came from.
    source_files: Vec<std::path::PathBuf>,
    /// Stack of enclosing value-block result types, innermost last. A
    /// `return` statement binds to the top entry instead of the function.
    /// `Some(t)` once the type is known (checked position, or the first
    /// `return` in synthesis position); `None` while still undetermined.
    value_block_types: Vec<Option<LangType>>,
    /// The target being compiled for. Only `asm fn` consults it: register
    /// names are validated against this target's register model, so `rax`
    /// under an `aarch64-*` target is a clean error. Defaults to the host;
    /// override with [`TypeChecker::with_target`].
    target: TargetSpec,
    errors: Vec<TypeCheckError>,
    /// Non-fatal diagnostics accumulated during checking (e.g. an implicit
    /// signedness-changing conversion). Read by the driver *after* a successful
    /// `check_program` — they never appear in the `Err` path, and never change
    /// the exit code (v1). Both `main.rs` and the test harness must read this,
    /// since each builds its own `TypeChecker`.
    warnings: Vec<super::errors::TypeWarning>,
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

    /// Set a single-entry source-file registry. Convenience for the simple
    /// single-file case; multi-file consumers should let `check_program`
    /// pull the registry from `Program::source_files` directly.
    #[must_use]
    pub fn with_source_file(mut self, path: impl Into<String>) -> Self {
        self.source_files = vec![std::path::PathBuf::from(path.into())];
        self
    }

    /// Format a single error with the originating source file prepended.
    /// Looks up the file via the error's `pos.file_id` so errors inside an
    /// imported file are attributed to that file, not the entry one.
    #[must_use]
    pub fn format_error(&self, err: &TypeCheckError) -> String {
        let Some(pos) = err.position() else {
            return format!("{err}");
        };
        match self.source_files.get(pos.file_id as usize) {
            Some(path) => format!("{}:{}:{}: {}", path.display(), pos.line, pos.column, err),
            None => format!("{err}"),
        }
    }

    /// Warnings accumulated during the last `check_program`. Read after a
    /// successful check — `main.rs` prints these to stderr, the test harness
    /// asserts on them (`# expected_warning:`).
    #[must_use]
    pub fn warnings(&self) -> &[super::errors::TypeWarning] {
        &self.warnings
    }

    /// Format one warning as `file:line:col: warning: <message>`, mirroring
    /// [`Self::format_error`] and the `aspc: warning:` precedent in `main.rs`.
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

    /// Check a complete program.
    ///
    /// The AST is taken by mutable reference: the checker stamps the resolved
    /// `expr_type` onto literal and arithmetic nodes as it pushes target types
    /// down into expressions.
    ///
    /// # Errors
    /// Returns `Err(Vec<TypeCheckError>)` listing every type error found.
    pub fn check_program(&mut self, program: &mut Program) -> Result<(), Vec<TypeCheckError>> {
        // Take the shared symbol table for the duration of checking; restore it
        // before returning so codegen sees it (plus any refinement we make).
        self.symbols = std::mem::take(&mut program.symbols);
        // Inherit the parser's file registry — unless caller pre-set one via
        // `with_source_file` (single-file convenience) — so error messages
        // can name the originating file for each `Position`.
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

    fn check_global_var(&mut self, global: &mut GlobalVar) {
        let var_type = global.var_type;
        if var_type.is_void_value() {
            self.errors
                .push(TypeCheckError::InvalidVoidValue(global.pos));
        }
        if let Some(init_expr) = &mut global.initializer {
            self.check_initializer(init_expr, &var_type);
        }
    }

    /// Check a declaration initializer against the declared type.
    ///
    /// A `ListInitializer` validates its element count and each element
    /// against the declared element type; any other expression is checked
    /// directly against `var_type`. Shared by global and local declarations.
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
            if param_type.is_void_value() {
                self.errors
                    .push(TypeCheckError::InvalidVoidValue(proto.pos));
            }
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
