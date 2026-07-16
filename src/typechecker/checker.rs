use super::errors::TypeCheckError;
use super::types::{cast_valid, literal_float_compatible, literal_int_fits, types_coercible};
use crate::lexer::{LangType, TypeBase};
use crate::scope::ScopeStack;
use crate::parser::{
    BinaryOp, ExprKind, Expression, Function, GlobalVar, LiteralValue, Program, Statement,
    StatementKind,
};
use crate::symbol::module::{ModuleSymbols, Visibility};
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
    errors: Vec<TypeCheckError>,
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
            errors: Vec::new(),
        }
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

        for func in &mut program.functions {
            self.check_proto(&func.proto);
            if !func.proto.is_extern {
                self.check_function(func);
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

    fn check_function(&mut self, func: &mut Function) {
        self.current_function = Some(func.proto.name.clone());
        self.enter_scope();

        for (param_type, param_name) in &func.proto.params {
            self.define_var(param_name.clone(), *param_type);
        }

        for stmt in &mut func.body {
            self.check_statement(stmt);
        }

        self.exit_scope();
        self.current_function = None;
    }

    // ── Statement checking ───────────────────────────────────────────────────

    fn check_statement(&mut self, stmt: &mut Statement) {
        let stmt_pos = stmt.pos;
        match &mut stmt.kind {
            StatementKind::VarDecl {
                var_type,
                name,
                initializer,
            } => {
                let var_type = *var_type;
                if var_type.is_void_value() {
                    self.errors.push(TypeCheckError::InvalidVoidValue(stmt_pos));
                }
                self.define_var(name.clone(), var_type);
                if let Some(init_expr) = initializer {
                    self.check_initializer(init_expr, &var_type);
                }
            }

            StatementKind::VarAssign { name, value } => {
                if let Some(var_type) = self.lookup_var(name) {
                    if var_type.is_const {
                        self.errors.push(TypeCheckError::AssignmentToConst {
                            name: name.clone(),
                            position: value.pos,
                        });
                    }
                    self.check_expression(value, &var_type);
                }
            }

            StatementKind::DerefAssign { target, value } => {
                let target_type = self.synth_expression(target);
                self.check_expression(value, &target_type);
            }

            StatementKind::FieldAssign { target, value } => {
                let target_type = self.synth_expression(target);
                if target_type.is_const {
                    let name = if let ExprKind::FieldAccess { field, .. } = &target.kind {
                        field.clone()
                    } else {
                        "field".to_string()
                    };
                    self.errors.push(TypeCheckError::AssignmentToConst {
                        name,
                        position: target.pos,
                    });
                }
                self.check_expression(value, &target_type);
            }

            StatementKind::Return(opt_expr) => {
                // Inside a value-block, `return` yields the innermost block,
                // not the function. In a checked position the block's type is
                // known up front; in synthesis position the first `return`
                // fixes it and later ones are checked against it.
                if let Some(slot) = self.value_block_types.last().copied() {
                    match opt_expr {
                        Some(expr) => match slot {
                            Some(t) => self.check_expression(expr, &t),
                            None => {
                                let t = self.synth_expression(expr);
                                *self.value_block_types.last_mut().unwrap() = Some(t);
                            }
                        },
                        None => self
                            .errors
                            .push(TypeCheckError::ValueBlockVoidReturn(stmt_pos)),
                    }
                } else if let Some(func_name) = self.current_function.clone()
                    && let Some(sig) = self.symbols.lookup_function(&func_name).cloned()
                {
                    match opt_expr {
                        Some(expr) => {
                            self.check_expression(expr, &sig.return_type);
                        }
                        None => {
                            let void = LangType::VOID;
                            if sig.return_type != void {
                                self.errors.push(TypeCheckError::ReturnTypeMismatch {
                                    expected: sig.return_type,
                                    found: void,
                                    position: stmt_pos,
                                });
                            }
                        }
                    }
                }
            }

            StatementKind::If {
                condition,
                then_block,
                else_block,
            } => {
                self.check_condition(condition);
                self.enter_scope();
                for s in then_block.iter_mut() {
                    self.check_statement(s);
                }
                self.exit_scope();
                if let Some(else_stmts) = else_block {
                    self.enter_scope();
                    for s in else_stmts.iter_mut() {
                        self.check_statement(s);
                    }
                    self.exit_scope();
                }
            }

            StatementKind::While { condition, body } => {
                self.check_condition(condition);
                self.enter_scope();
                for s in body.iter_mut() {
                    self.check_statement(s);
                }
                self.exit_scope();
            }

            StatementKind::For {
                init,
                condition,
                increment,
                body,
            } => {
                self.enter_scope();
                if let Some(init_stmt) = init {
                    self.check_statement(init_stmt);
                }
                if let Some(cond_expr) = condition {
                    self.check_condition(cond_expr);
                }
                if let Some(inc_stmt) = increment {
                    self.check_statement(inc_stmt);
                }
                for s in body.iter_mut() {
                    self.check_statement(s);
                }
                self.exit_scope();
            }

            StatementKind::Block(stmts) => {
                self.enter_scope();
                for s in stmts.iter_mut() {
                    self.check_statement(s);
                }
                self.exit_scope();
            }

            StatementKind::Expression(expr) => {
                self.synth_expression(expr);
            }

            StatementKind::Break | StatementKind::Continue => {}
        }
    }

    /// Synthesise a condition expression and verify it is usable as a truth value.
    ///
    /// Conditions impose no target type, so they run in synthesis mode; the
    /// "must be numeric or pointer" rule then rejects `void`.
    fn check_condition(&mut self, cond: &mut Expression) {
        let cond_type = self.synth_expression(cond);
        if cond_type.is_void_value() {
            self.errors
                .push(TypeCheckError::InvalidConditionType(cond_type, cond.pos));
        }
    }

    // ── Value blocks ─────────────────────────────────────────────────────────

    /// Check a value-block's statements and resolve the block's type.
    ///
    /// `target` is `Some` in checked positions (the block must yield that
    /// type) and `None` in synthesis positions (the first `return` fixes the
    /// type; see the `Return` arm of `check_statement`). Also enforces the
    /// all-paths rule: every control path through the block must end in a
    /// `return`, conservatively (loops never count, even `while true`).
    fn check_value_block(
        &mut self,
        stmts: &mut [Statement],
        target: Option<LangType>,
        pos: crate::lexer::Position,
    ) -> LangType {
        self.value_block_types.push(target);
        self.enter_scope();
        for s in stmts.iter_mut() {
            self.check_statement(s);
        }
        self.exit_scope();
        let resolved = self.value_block_types.pop().flatten();

        if !Self::always_returns(stmts) {
            self.errors
                .push(TypeCheckError::ValueBlockMissingReturn(pos));
        }
        // `None` means the block contains no value-carrying `return` at all;
        // the all-paths error above has already fired (zero returns cannot
        // cover every path), so `void` is only a placeholder.
        resolved.unwrap_or(LangType::VOID)
    }

    /// Conservative "every path returns" analysis for value-blocks: a
    /// statement list returns iff any statement in it definitely returns
    /// (everything after that one is unreachable). Loops never count —
    /// `break` could skip their returns — and neither do `break`/`continue`
    /// themselves. Returns inside *nested* value-blocks live under an
    /// expression, which this walk deliberately does not descend into, so
    /// they never satisfy the outer block.
    fn always_returns(stmts: &[Statement]) -> bool {
        stmts.iter().any(Self::stmt_always_returns)
    }

    fn stmt_always_returns(stmt: &Statement) -> bool {
        match &stmt.kind {
            StatementKind::Return(_) => true,
            StatementKind::Block(inner) => Self::always_returns(inner),
            StatementKind::If {
                then_block,
                else_block: Some(else_stmts),
                ..
            } => Self::always_returns(then_block) && Self::always_returns(else_stmts),
            _ => false,
        }
    }

    // ── Expression type resolution (synthesis mode) ──────────────────────────

    /// Synthesise the type of `expr` with no contextual expectation.
    ///
    /// Walks the expression, emits any type errors found, and returns its
    /// resolved type. Used at sites where nothing constrains the type: callee
    /// resolution, indices, conditions, cast/dereference operands.
    fn synth_expression(&mut self, expr: &mut Expression) -> LangType {
        let pos = expr.pos;
        let default_type = expr.expr_type;
        match &mut expr.kind {
            ExprKind::Literal(_) => default_type,

            ExprKind::Variable(name) => {
                if let Some(ty) = self.lookup_var(name) {
                    ty
                } else {
                    self.errors
                        .push(TypeCheckError::UndefinedVariable(name.clone(), pos));
                    default_type
                }
            }

            ExprKind::Binary { left, op, right } => {
                let left_type = self.synth_expression(left);
                let right_type = self.synth_expression(right);

                if !Self::binary_op_types_valid(&left_type, &right_type, op) {
                    self.errors.push(TypeCheckError::InvalidBinaryOperation {
                        operator: format!("{op:?}"),
                        left: left_type,
                        right: right_type,
                        position: pos,
                    });
                }
                if matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr) {
                    // Logical `&&`/`||` yield a boolean regardless of operand type.
                    let bool_ty = LangType::BOOL;
                    expr.expr_type = bool_ty;
                    bool_ty
                } else {
                    // Result type: the wider of the two operand types (or left if equal)
                    Self::wider_type(&left_type, &right_type)
                }
            }

            ExprKind::Comparison { left, op: _, right } => {
                let left_type = self.synth_expression(left);
                let right_type = self.synth_expression(right);

                if !Self::binary_op_types_valid(&left_type, &right_type, &BinaryOp::Add) {
                    self.errors.push(TypeCheckError::InvalidBinaryOperation {
                        operator: "comparison".to_string(),
                        left: left_type,
                        right: right_type,
                        position: pos,
                    });
                }
                // A comparison never propagates its own (`i32`) result type into
                // its operands, but a literal operand may adopt its *sibling's*
                // narrower integer type so codegen compares at that width instead
                // of widening both sides to the literal's default `i32`. The
                // boolean result is unaffected because the literal fits the
                // sibling's exact type.
                Self::narrow_literal_to_sibling(left, right_type);
                Self::narrow_literal_to_sibling(right, left_type);
                let bool_ty = LangType::BOOL;
                expr.expr_type = bool_ty;
                bool_ty
            }

            ExprKind::Reference(inner) => {
                self.synth_expression(inner);
                default_type
            }

            ExprKind::Dereference(inner) => {
                let inner_type = self.synth_expression(inner);
                // Arrays and pointers are both valid dereference targets.
                // Array subscript `arr[i]` is lowered to `*(arr + i)` by the
                // parser, so array types must be accepted here.
                if !inner_type.is_pointer_like() {
                    self.errors
                        .push(TypeCheckError::InvalidDereference(inner_type, pos));
                }
                // `u0*` is opaque: its pointee is a void value, so it cannot be
                // dereferenced (or subscripted) without a cast to a sized
                // pointer first.
                if inner_type.is_opaque_ptr() {
                    self.errors.push(TypeCheckError::OpaqueDereference(pos));
                }
                default_type
            }

            ExprKind::FunctionCall { name, args } => {
                self.check_call(name, args, pos);
                default_type
            }

            ExprKind::Cast {
                expr: inner,
                target_type,
            } => {
                let from_type = self.synth_expression(inner);
                if !cast_valid(&from_type, target_type) {
                    self.errors.push(TypeCheckError::InvalidCast {
                        from: from_type,
                        to: *target_type,
                        position: pos,
                    });
                }
                *target_type
            }

            ExprKind::Alloc { alloc_type, count } => {
                if alloc_type.is_void_value() {
                    self.errors.push(TypeCheckError::InvalidVoidValue(pos));
                }
                let count_pos = count.pos;
                let count_type = self.synth_expression(count);
                if !matches!(count_type.base, TypeBase::SInt | TypeBase::UInt)
                    || count_type.pointer_depth > 0
                {
                    self.errors.push(TypeCheckError::TypeMismatch {
                        expected: LangType::U64,
                        found: count_type,
                        position: count_pos,
                    });
                }
                default_type
            }

            ExprKind::UnaryNot(inner) => {
                let inner_type = self.synth_expression(inner);
                // `!p` is a null test and works for any pointer, `u0*`
                // included; only void *values* are rejected.
                if inner_type.is_void_value() {
                    self.errors.push(TypeCheckError::InvalidUnaryOperation {
                        operator: "!".to_string(),
                        operand: inner_type,
                        position: pos,
                    });
                }
                // Logical negation yields a boolean.
                let bool_ty = LangType::BOOL;
                expr.expr_type = bool_ty;
                bool_ty
            }

            ExprKind::BitwiseNot(inner) => {
                let inner_type = self.synth_expression(inner);
                // Bit-twiddling an opaque pointer deserves an explicit cast,
                // so `u0*` stays rejected here (unlike `!` above).
                if inner_type.base == TypeBase::Void {
                    self.errors.push(TypeCheckError::InvalidUnaryOperation {
                        operator: "~".to_string(),
                        operand: inner_type,
                        position: pos,
                    });
                }
                default_type
            }

            ExprKind::ListInitializer(elements) => {
                for elem in elements.iter_mut() {
                    self.synth_expression(elem);
                }
                default_type
            }

            ExprKind::FieldAccess { base, field } => {
                let base_type = self.synth_expression(base);
                let field = field.clone();
                let field_type = self.resolve_field(&base_type, &field, pos);
                expr.expr_type = field_type;
                field_type
            }

            ExprKind::StructLiteral { struct_id, fields } => {
                let struct_id = *struct_id;
                // Snapshot declared fields to avoid holding a `self.symbols`
                // borrow across the per-field `check_expression` calls.
                let declared: Vec<(String, LangType, Visibility)> = self
                    .symbols
                    .struct_info(struct_id)
                    .fields
                    .iter()
                    .map(|f| (f.name.clone(), f.ty, f.vis))
                    .collect();
                let type_name = self.symbols.struct_info(struct_id).name.clone();
                let inside_methods = self.is_inside_struct_methods(struct_id);

                let mut named: Vec<String> = Vec::with_capacity(fields.len());
                for (fname, fexpr) in fields.iter_mut() {
                    named.push(fname.clone());
                    if let Some((_, fty, vis)) =
                        declared.iter().find(|(n, _, _)| n == fname)
                    {
                        let fty = *fty;
                        if *vis == Visibility::Private && !inside_methods {
                            self.errors.push(TypeCheckError::InaccessibleField {
                                field: fname.clone(),
                                type_name: type_name.clone(),
                                position: pos,
                            });
                        }
                        self.check_expression(fexpr, &fty);
                    } else {
                        self.errors.push(TypeCheckError::UnknownField {
                            field: fname.clone(),
                            type_name: type_name.clone(),
                            position: pos,
                        });
                        self.synth_expression(fexpr);
                    }
                }

                let missing: Vec<&str> = declared
                    .iter()
                    .map(|(n, _, _)| n.as_str())
                    .filter(|n| !named.iter().any(|m| m == n))
                    .collect();
                if !missing.is_empty() {
                    self.errors.push(TypeCheckError::MissingStructFields {
                        type_name,
                        missing: missing.join(", "),
                        position: pos,
                    });
                }

                let struct_ty = LangType::struct_type(struct_id);
                expr.expr_type = struct_ty;
                struct_ty
            }

            // A bare function name (or `&func` collapsed) — the parser stamped
            // the FnPtr type from the registry. Nothing to check; just hand it
            // back. An unknown function name would have stayed `Variable` with
            // a `void` stamp, so it never reaches this arm.
            ExprKind::FunctionRef(_) => default_type,

            // Indirect call through a function-pointer value: synth the callee,
            // validate it's a `FnPtr`, then `check` each arg against the
            // declared parameter type (mirrors `check_call`'s pattern).
            ExprKind::IndirectCall { callee, args } => {
                let callee_type = self.synth_expression(callee);
                let sig_params: Option<Vec<LangType>> = match callee_type.base {
                    TypeBase::FnPtr(id) if callee_type.pointer_depth == 0 => {
                        Some(self.symbols.fnptr_sig(id).params.clone())
                    }
                    _ => {
                        self.errors.push(TypeCheckError::TypeMismatch {
                            expected: LangType::VOID,
                            found: callee_type,
                            position: pos,
                        });
                        None
                    }
                };
                if let Some(params) = sig_params {
                    if params.len() != args.len() {
                        self.errors.push(TypeCheckError::ArgumentCountMismatch {
                            name: "<indirect call>".to_string(),
                            expected: params.len(),
                            found: args.len(),
                            position: pos,
                        });
                        for arg in args.iter_mut() {
                            self.synth_expression(arg);
                        }
                    } else {
                        for (pty, arg) in params.iter().zip(args.iter_mut()) {
                            self.check_expression(arg, pty);
                        }
                    }
                } else {
                    for arg in args.iter_mut() {
                        self.synth_expression(arg);
                    }
                }
                default_type
            }

            // `sizeof(T)` is a compile-time `u64` constant; the parser
            // already stamped the expression type at construction.
            ExprKind::SizeOf(_) => default_type,

            // `null` carries the parser-stamped `u8*` placeholder when used in
            // a context that doesn't constrain its type (e.g. `null == p`).
            // Pointer-to-pointer coercion handles the rest at the boundary.
            ExprKind::Null => default_type,

            // Value-block with no contextual target: the first `return`
            // inside synthesizes the block's type, later ones check against
            // it (see the `Return` arm of `check_statement`).
            ExprKind::ValueBlock(stmts) => {
                let ty = self.check_value_block(stmts, None, pos);
                expr.expr_type = ty;
                ty
            }
        }
    }

    /// Resolve a field access on a base type, emitting an error and returning a
    /// `void` placeholder when the base is not a type-struct or the field is
    /// unknown. A single-level pointer-to-struct auto-dereferences.
    fn resolve_field(
        &mut self,
        base_type: &LangType,
        field: &str,
        pos: crate::lexer::Position,
    ) -> LangType {
        if let TypeBase::Struct(id) = base_type.base
            && base_type.pointer_depth <= 1
        {
            if let Some((_, finfo)) = self.symbols.field(id, field) {
                let vis = finfo.vis;
                // A const struct (or `*const Struct`) propagates const-ness
                // to its fields, so assignment-through `this.field = ...` in a
                // `const fn` body lands on the existing AssignmentToConst path.
                let mut fty = finfo.ty;
                if base_type.is_const {
                    fty.is_const = true;
                }
                // Private fields are accessible only from the type's own
                // methods (M4 encapsulation).
                if vis == Visibility::Private && !self.is_inside_struct_methods(id) {
                    let type_name = self.type_name(base_type);
                    self.errors.push(TypeCheckError::InaccessibleField {
                        field: field.to_string(),
                        type_name,
                        position: pos,
                    });
                }
                return fty;
            }
            let type_name = self.type_name(base_type);
            self.errors.push(TypeCheckError::UnknownField {
                field: field.to_string(),
                type_name,
                position: pos,
            });
            return LangType::VOID;
        }
        self.errors.push(TypeCheckError::NotAStruct {
            found: *base_type,
            position: pos,
        });
        LangType::VOID
    }

    /// `true` when the function being checked is a method of the given
    /// type-struct (its mangled name begins with `"<TypeName>$"`).
    fn is_inside_struct_methods(&self, struct_id: u32) -> bool {
        let Some(current) = self.current_function.as_deref() else {
            return false;
        };
        let prefix =
            crate::symbol::module::method_owner_prefix(&self.symbols.struct_info(struct_id).name);
        current.starts_with(&prefix)
    }

    /// Human-readable name for a type, resolving type-struct ids to their
    /// declared names (which `LangType`'s `Display` cannot reach).
    fn type_name(&self, ty: &LangType) -> String {
        if let TypeBase::Struct(id) = ty.base {
            let stars = "*".repeat(ty.pointer_depth as usize);
            format!("{}{}", self.symbols.struct_info(id).name, stars)
        } else {
            format!("{ty}")
        }
    }

    /// Enforce method encapsulation: a private method is callable only from
    /// within its own type's methods. `name` is the call's mangled target
    /// (`Type$method`); a name with no `$` is an ordinary free function and is
    /// always accessible. The private-method twin of [`Self::resolve_field`]'s
    /// private-field rule — the two syntactic call forms (`obj.m()`, `T.m()`)
    /// have both already been lowered to this mangled name by the parser.
    fn check_method_access(&mut self, name: &str, pos: crate::lexer::Position) {
        let Some((type_name, method_name)) = name.split_once('$') else {
            return;
        };
        let Some(id) = self.symbols.struct_id(type_name) else {
            return;
        };
        let vis = match self.symbols.struct_info(id).methods.get(method_name) {
            Some(sig) => sig.vis,
            None => return,
        };
        if vis == Visibility::Private && !self.is_inside_struct_methods(id) {
            self.errors.push(TypeCheckError::InaccessibleMethod {
                method: method_name.to_string(),
                type_name: type_name.to_string(),
                position: pos,
            });
        }
    }

    /// Resolve a function call: validate the callee, arity, and argument types.
    ///
    /// Each argument is *checked* against its declared parameter type, which
    /// pushes the parameter type into literal arguments.
    fn check_call(
        &mut self,
        name: &str,
        args: &mut [Expression],
        pos: crate::lexer::Position,
    ) {
        self.check_method_access(name, pos);
        if let Some(sig) = self.symbols.lookup_function(name).cloned() {
            if sig.params.len() != args.len() {
                self.errors.push(TypeCheckError::ArgumentCountMismatch {
                    name: name.to_string(),
                    expected: sig.params.len(),
                    found: args.len(),
                    position: pos,
                });
                // Still synthesise the arguments so their own errors surface.
                for arg in args.iter_mut() {
                    self.synth_expression(arg);
                }
            } else {
                for ((param_ty, _), arg_expr) in sig.params.iter().zip(args.iter_mut()) {
                    self.check_expression(arg_expr, param_ty);
                }
            }
        } else {
            self.errors
                .push(TypeCheckError::UndefinedFunction(name.to_string(), pos));
            for arg in args.iter_mut() {
                self.synth_expression(arg);
            }
        }
    }

    // ── Expression type checking (checking mode) ─────────────────────────────

    /// Check `expr` against the expected `target` type.
    ///
    /// Stamps `expr.expr_type` and pushes the target into children where the
    /// child's type *is* the parent's type (arithmetic operands, bitwise-not,
    /// reference/dereference, list-initialiser elements). Emits a single
    /// `TypeMismatch` (or a more specific literal-fit error) on failure.
    fn check_expression(&mut self, expr: &mut Expression, target: &LangType) {
        let pos = expr.pos;
        match &mut expr.kind {
            // Integer literal: validate value-fit against the target and stamp it.
            ExprKind::Literal(LiteralValue::Integer(val)) => {
                let val = *val;
                if literal_int_fits(val, target) {
                    expr.expr_type = *target;
                } else if !types_coercible(&expr.expr_type, target) {
                    self.errors.push(TypeCheckError::TypeMismatch {
                        expected: *target,
                        found: expr.expr_type,
                        position: pos,
                    });
                }
            }

            // Float literal: any float target accepts it; stamp the target.
            ExprKind::Literal(LiteralValue::Float(_)) => {
                if literal_float_compatible(target) {
                    expr.expr_type = *target;
                } else if !types_coercible(&expr.expr_type, target) {
                    self.errors.push(TypeCheckError::TypeMismatch {
                        expected: *target,
                        found: expr.expr_type,
                        position: pos,
                    });
                }
            }

            // String literal: type is fixed; verify coercibility only.
            ExprKind::Literal(LiteralValue::String(_)) => {
                self.assert_coercible(expr.expr_type, target, pos);
            }

            // Binary arithmetic with a plain numeric target: propagate the
            // target into both operands; the operation shares its result type.
            // Logical `&&`/`||` are excluded — they yield a boolean, not the
            // target type, so they fall through to the synth arm below.
            ExprKind::Binary { left, op, right }
                if target.is_plain_numeric()
                    && !matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr) =>
            {
                self.check_expression(left, target);
                self.check_expression(right, target);
                let left_type = left.expr_type;
                let right_type = right.expr_type;
                if !Self::binary_op_types_valid(&left_type, &right_type, op) {
                    self.errors.push(TypeCheckError::InvalidBinaryOperation {
                        operator: format!("{op:?}"),
                        left: left_type,
                        right: right_type,
                        position: pos,
                    });
                }
                expr.expr_type = *target;
            }

            // Bitwise-not preserves its operand type: propagate the target inward.
            ExprKind::BitwiseNot(inner) => {
                self.check_expression(inner, target);
                let inner_type = inner.expr_type;
                if inner_type.base == TypeBase::Void {
                    self.errors.push(TypeCheckError::InvalidUnaryOperation {
                        operator: "~".to_string(),
                        operand: inner_type,
                        position: pos,
                    });
                }
                expr.expr_type = *target;
            }

            // Reference: the inner expression's target is the pointee type.
            // A Reference may produce a const-pointer to a non-const value
            // (C-style `const T* p = &t`), so the inner itself need not carry
            // the pointee's const-ness.
            ExprKind::Reference(inner) => {
                // Against an opaque `u0*` target the pointee is `u0` — no
                // value has that type, so nothing useful can be pushed
                // inward; synthesise instead (any `&lvalue` coerces to u0*).
                let opaque_target =
                    target.base == TypeBase::Void && target.pointer_depth == 1;
                if target.pointer_depth > 0 && !opaque_target {
                    let mut inner_target = *target;
                    inner_target.pointer_depth -= 1;
                    inner_target.is_const = false;
                    self.check_expression(inner, &inner_target);
                } else {
                    self.synth_expression(inner);
                }
                self.assert_coercible(expr.expr_type, target, pos);
            }

            // Dereference: synthesise (the operand is a pointer/array, not the
            // target type), then assert the produced type is coercible.
            ExprKind::Dereference(_) => {
                let found = self.synth_expression(expr);
                self.assert_coercible(found, target, pos);
            }

            // Value-block in a checked position: the target is pushed into
            // every `return` inside the block, and the block adopts it.
            ExprKind::ValueBlock(stmts) => {
                self.check_value_block(stmts, Some(*target), pos);
                expr.expr_type = *target;
            }

            // List initialiser: decay the target to its element type and check
            // every element against it.
            ExprKind::ListInitializer(elements) => {
                let elem_target = target.element_type();
                for elem in elements.iter_mut() {
                    self.check_expression(elem, &elem_target);
                }
            }

            // Comparison, unary-not, cast, function call, variable, alloc, and
            // binary ops with a non-numeric (pointer) target: the expression's
            // type is not the target's type, so synthesise and assert
            // coercibility at the boundary.
            _ => {
                let found = self.synth_expression(expr);
                self.assert_coercible(found, target, pos);
            }
        }
    }

    /// Emit a `TypeMismatch` unless `found` is coercible to `target`.
    fn assert_coercible(&mut self, found: LangType, target: &LangType, pos: crate::lexer::Position) {
        if !types_coercible(&found, target) {
            self.errors.push(TypeCheckError::TypeMismatch {
                expected: *target,
                found,
                position: pos,
            });
        }
    }

    /// If `operand` is an integer literal that fits the concrete integer type
    /// `sibling`, restamp the literal to that type.
    ///
    /// Used for comparison operands: `u8 i; ... i < 10` compares at `i8` rather
    /// than zero-extending `i` to `i32` to meet the literal's default width.
    /// Restricted to literals that fit `sibling`, so the comparison's result is
    /// unchanged.
    fn narrow_literal_to_sibling(operand: &mut Expression, sibling: LangType) {
        if let ExprKind::Literal(LiteralValue::Integer(val)) = operand.kind
            && sibling.is_plain_int()
            && literal_int_fits(val, &sibling)
        {
            operand.expr_type = sibling;
        }
    }

    // ── Binary op helpers ────────────────────────────────────────────────────

    /// Check if two operand types are valid for the given binary operation.
    fn binary_op_types_valid(left: &LangType, right: &LangType, op: &BinaryOp) -> bool {
        // Pointer arithmetic: `ptr ± int` and `int + ptr` (`int - ptr` has no
        // meaning). A `u0*` has an unsized pointee: no arithmetic (GEP cannot
        // scale by sizeof(u0)) — cast to `u8*` for byte offsets.
        let ptr_int = left.is_pointer_like() && right.is_plain_int();
        let int_ptr = left.is_plain_int() && right.is_pointer_like();
        if (matches!(op, BinaryOp::Add | BinaryOp::Sub) && ptr_int)
            || (matches!(op, BinaryOp::Add) && int_ptr)
        {
            return !(left.is_opaque_ptr() || right.is_opaque_ptr());
        }

        // Both same family — either side can widen to the other
        types_coercible(left, right) || types_coercible(right, left)
    }

    /// Return the "wider" of two types (for binary-op result typing).
    /// Pointer arithmetic yields the pointer side regardless of operand
    /// order; falls back to `left` when types are incomparable.
    fn wider_type(left: &LangType, right: &LangType) -> LangType {
        if left.is_pointer_like() {
            return *left;
        }
        if right.is_pointer_like() {
            return *right;
        }
        if left.size_bits >= right.size_bits {
            *left
        } else {
            *right
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::{tokenize, Position};
    use crate::parser::{ExprKind, LiteralValue, Parser, Program, StatementKind};

    /// Lex, parse, and type-check `src`, returning the (mutated) AST and the result.
    fn check(src: &str) -> (Program, Result<(), Vec<TypeCheckError>>) {
        let tokens = tokenize(src.to_string()).expect("tokenization should succeed");
        let mut parser = Parser::new(tokens);
        let mut program = parser.parse_program().expect("parsing should succeed");
        let mut checker = TypeChecker::new();
        let result = checker.check_program(&mut program);
        (program, result)
    }

    /// Find a function by name.
    fn func<'a>(program: &'a Program, name: &str) -> &'a Function {
        program
            .functions
            .iter()
            .find(|f| f.proto.name == name)
            .unwrap_or_else(|| panic!("function `{name}` not found"))
    }

    /// Initializer expression of the `idx`-th `VarDecl` in function `fname`.
    fn nth_var_init<'a>(program: &'a Program, fname: &str, idx: usize) -> &'a Expression {
        let mut count = 0;
        for stmt in &func(program, fname).body {
            if let StatementKind::VarDecl {
                initializer: Some(init),
                ..
            } = &stmt.kind
            {
                if count == idx {
                    return init;
                }
                count += 1;
            }
        }
        panic!("var decl #{idx} not found in `{fname}`");
    }

    fn assert_ty(actual: LangType, base: TypeBase, bits: u32, ptr: u32) {
        assert_eq!(actual.base, base, "base type");
        assert_eq!(actual.size_bits, bits, "size_bits");
        assert_eq!(actual.pointer_depth, ptr, "pointer_depth");
    }

    fn has_type_mismatch(errs: &[TypeCheckError], at: Position) -> bool {
        errs.iter().any(|e| {
            matches!(e, TypeCheckError::TypeMismatch { position, .. } if *position == at)
        })
    }

    // 1. Literal fits target on assignment — stamped at the target type.
    #[test]
    fn literal_fits_target() {
        let (program, res) = check("fn main() -> i32 {\n    u8 x = 200\n    return 0\n}\n");
        assert!(res.is_ok(), "expected ok, got {res:?}");
        assert_ty(nth_var_init(&program, "main", 0).expr_type, TypeBase::UInt, 8, 0);
    }

    // 2. Literal overflows target — error at the literal's position.
    #[test]
    fn literal_overflows_target() {
        let (program, res) = check("fn main() -> i32 {\n    u8 x = 300\n    return 0\n}\n");
        let lit_pos = nth_var_init(&program, "main", 0).pos;
        let errs = res.expect_err("expected overflow error");
        assert!(has_type_mismatch(&errs, lit_pos), "error should sit on the literal: {errs:?}");
    }

    // 3. Binary propagates target — both literals and the `+` stamped u8.
    #[test]
    fn binary_propagates_target() {
        let (program, res) = check("fn main() -> i32 {\n    u8 x = 1 + 2\n    return 0\n}\n");
        assert!(res.is_ok(), "expected ok, got {res:?}");
        let init = nth_var_init(&program, "main", 0);
        assert_ty(init.expr_type, TypeBase::UInt, 8, 0);
        let ExprKind::Binary { left, right, .. } = &init.kind else {
            panic!("expected binary");
        };
        assert_ty(left.expr_type, TypeBase::UInt, 8, 0);
        assert_ty(right.expr_type, TypeBase::UInt, 8, 0);
    }

    // 4. Mixed literal and variable — the literal is stamped, result is u8.
    #[test]
    fn binary_mixed_literal_and_variable() {
        let src = "fn main() -> i32 {\n    u8 y = 0\n    u8 x = y + 1\n    return 0\n}\n";
        let (program, res) = check(src);
        assert!(res.is_ok(), "expected ok, got {res:?}");
        let init = nth_var_init(&program, "main", 1);
        assert_ty(init.expr_type, TypeBase::UInt, 8, 0);
        let ExprKind::Binary { right, .. } = &init.kind else {
            panic!("expected binary");
        };
        assert_ty(right.expr_type, TypeBase::UInt, 8, 0);
    }

    // 5. Comparison yields `bool` and coerces into an integer target; the
    //    target is never propagated into the operands.
    #[test]
    fn comparison_yields_bool() {
        let src = "fn main() -> i32 {\n    i32 a = 1\n    i32 b = 2\n    i32 c = a < b\n    return 0\n}\n";
        let (program, res) = check(src);
        assert!(res.is_ok(), "expected ok, got {res:?}");
        // The comparison node itself is `bool`; it coerces to the `i32` target.
        assert_ty(nth_var_init(&program, "main", 2).expr_type, TypeBase::Bool, 8, 0);
    }

    // 6. Function-call argument fit — error at the literal argument.
    #[test]
    fn call_argument_overflow() {
        let src = "fn f(u8 b) -> i32 {\n    return 0\n}\nfn main() -> i32 {\n    return f(300)\n}\n";
        let (_program, res) = check(src);
        let errs = res.expect_err("expected argument overflow error");
        assert!(
            errs.iter().any(|e| matches!(e, TypeCheckError::TypeMismatch { expected, .. }
                if expected.base == TypeBase::UInt && expected.size_bits == 8)),
            "expected u8 type mismatch on the argument: {errs:?}"
        );
    }

    // 7. Return propagates the function's return type into the literal.
    #[test]
    fn return_literal_fits() {
        let (_p, res) = check("fn f() -> u16 {\n    return 65535\n}\n");
        assert!(res.is_ok(), "expected ok, got {res:?}");
    }

    #[test]
    fn return_literal_overflows() {
        let (_p, res) = check("fn f() -> u16 {\n    return 65536\n}\n");
        assert!(res.is_err(), "expected overflow error");
    }

    // 8. Dereference takes the synth path; coercibility holds.
    #[test]
    fn dereference_synth_path() {
        let src = "fn f(u8* p) -> u8 {\n    u8 x = *p\n    return x\n}\n";
        let (_p, res) = check(src);
        assert!(res.is_ok(), "expected ok, got {res:?}");
    }

    // 9. Reference checks its inner against the pointee type.
    #[test]
    fn reference_propagates_pointee() {
        let src = "fn main() -> i32 {\n    u8 v = 5\n    u8* p = &v\n    return 0\n}\n";
        let (_p, res) = check(src);
        assert!(res.is_ok(), "expected ok, got {res:?}");
    }

    // 10. Cast forces its type; the inner literal is left at its synth default.
    #[test]
    fn cast_does_not_propagate() {
        let (program, res) = check("fn main() -> i32 {\n    u32 x = 300 as u32\n    return 0\n}\n");
        assert!(res.is_ok(), "expected ok, got {res:?}");
        let init = nth_var_init(&program, "main", 0);
        let ExprKind::Cast { expr: inner, .. } = &init.kind else {
            panic!("expected cast");
        };
        // The literal keeps its synthesised default (i32), not the cast target.
        assert!(matches!(inner.kind, ExprKind::Literal(LiteralValue::Integer(300))));
        assert_eq!(inner.expr_type.base, TypeBase::SInt);
    }

    // 11. List initialiser propagates the element type into every element.
    #[test]
    fn list_init_propagates_element_type() {
        let (program, res) = check("fn main() -> i32 {\n    u8[3] arr = {1, 2, 3}\n    return 0\n}\n");
        assert!(res.is_ok(), "expected ok, got {res:?}");
        let ExprKind::ListInitializer(elems) = &nth_var_init(&program, "main", 0).kind else {
            panic!("expected list initializer");
        };
        for elem in elems {
            assert_ty(elem.expr_type, TypeBase::UInt, 8, 0);
        }
    }

    // 12. List initialiser element overflow — error at the offending element.
    #[test]
    fn list_init_element_overflow() {
        let (program, res) = check("fn main() -> i32 {\n    u8[3] arr = {1, 2, 300}\n    return 0\n}\n");
        let ExprKind::ListInitializer(elems) = &nth_var_init(&program, "main", 0).kind else {
            panic!("expected list initializer");
        };
        let bad_pos = elems[2].pos;
        let errs = res.expect_err("expected element overflow error");
        assert!(has_type_mismatch(&errs, bad_pos), "error should sit on the `300` element: {errs:?}");
    }

    // 13. Field access stamps the declared field type onto the AST.
    #[test]
    fn struct_field_access_stamps_field_type() {
        let src = "type P { public i32 x public u8 y }\n\
                   fn main() -> i32 {\n    P p = P { x = 1, y = 2 }\n    \
                   u8 v = p.y\n    return 0\n}\n";
        let (program, res) = check(src);
        assert!(res.is_ok(), "expected ok, got {res:?}");
        // var init #1 is `p.y` — its field type is u8.
        assert_ty(nth_var_init(&program, "main", 1).expr_type, TypeBase::UInt, 8, 0);
    }

    // 14. Accessing an undeclared field is an error.
    #[test]
    fn struct_unknown_field_errors() {
        let src = "type P { public i32 x }\n\
                   fn main() -> i32 {\n    P p = P { x = 1 }\n    return p.z\n}\n";
        let (_program, res) = check(src);
        let errs = res.expect_err("expected unknown-field error");
        assert!(
            errs.iter()
                .any(|e| matches!(e, TypeCheckError::UnknownField { .. })),
            "got {errs:?}"
        );
    }

    // 15. A struct literal must name every field.
    #[test]
    fn struct_missing_field_errors() {
        let src = "type P { public i32 x public i32 y }\n\
                   fn main() -> i32 {\n    P p = P { x = 1 }\n    return p.x\n}\n";
        let (_program, res) = check(src);
        let errs = res.expect_err("expected missing-field error");
        assert!(
            errs.iter()
                .any(|e| matches!(e, TypeCheckError::MissingStructFields { .. })),
            "got {errs:?}"
        );
    }

    // 18. Value-block in a checked position adopts the target type; its
    //     `return` binds to the block, NOT the enclosing function (1000
    //     fits the block's i32 but not the function's u8 return).
    #[test]
    fn value_block_return_binds_to_block() {
        let src = "fn f() -> u8 {\n    i32 x = { return 1000 }\n    return 0\n}\n";
        let (program, res) = check(src);
        assert!(res.is_ok(), "expected ok, got {res:?}");
        assert_ty(nth_var_init(&program, "f", 0).expr_type, TypeBase::SInt, 32, 0);
    }

    // 19. Nested value-blocks: each `return` binds to its innermost block.
    #[test]
    fn value_block_nested() {
        let src = "fn main() -> i32 {\n    i32 x = {\n    i32 y = { return 21 }\n    return y * 2\n}\n    return x\n}\n";
        let (_p, res) = check(src);
        assert!(res.is_ok(), "expected ok, got {res:?}");
    }

    // 20. Synthesis position (condition): the first `return` fixes the type.
    #[test]
    fn value_block_synth_position() {
        let src = "fn main() -> i32 {\n    if { return true } {\n    return 1\n}\n    return 0\n}\n";
        let (_p, res) = check(src);
        assert!(res.is_ok(), "expected ok, got {res:?}");
    }

    // 21. A path that falls off the end without returning is rejected.
    #[test]
    fn value_block_missing_return_errors() {
        let src = "fn main(u32 argc, u8** argv) -> i32 {\n    i32 x = {\n    if argc > 1 { return 1 }\n}\n    return x\n}\n";
        let (_p, res) = check(src);
        let errs = res.expect_err("expected all-paths error");
        assert!(
            errs.iter()
                .any(|e| matches!(e, TypeCheckError::ValueBlockMissingReturn(_))),
            "got {errs:?}"
        );
    }

    // 22. Loops never satisfy the all-paths rule (conservative: `break`
    //     could skip the return).
    #[test]
    fn value_block_loop_return_rejected() {
        let src = "fn main() -> i32 {\n    i32 x = {\n    while true { return 1 }\n}\n    return x\n}\n";
        let (_p, res) = check(src);
        let errs = res.expect_err("expected all-paths error");
        assert!(
            errs.iter()
                .any(|e| matches!(e, TypeCheckError::ValueBlockMissingReturn(_))),
            "got {errs:?}"
        );
    }

    // 23. A bare `return` inside a value-block is rejected. (`return;` —
    //     a bare `return` directly before `}` is already a parse error.)
    #[test]
    fn value_block_bare_return_errors() {
        let src = "fn main() -> i32 {\n    i32 x = { return; }\n    return x\n}\n";
        let (_p, res) = check(src);
        let errs = res.expect_err("expected void-return error");
        assert!(
            errs.iter()
                .any(|e| matches!(e, TypeCheckError::ValueBlockVoidReturn(_))),
            "got {errs:?}"
        );
    }

    // 24. Brace disambiguation regression: `{1, 2, 3}` stays a list
    //     initializer (test 11 covers the positive case; this pins the
    //     single-element form, which is a 1-element list, not a block).
    #[test]
    fn single_element_brace_stays_list() {
        let (program, res) = check("fn main() -> i32 {\n    u8[1] arr = {5}\n    return 0\n}\n");
        assert!(res.is_ok(), "expected ok, got {res:?}");
        assert!(matches!(
            nth_var_init(&program, "main", 0).kind,
            ExprKind::ListInitializer(_)
        ));
    }

    // 16. A private method (no `public`) is not callable from outside the type.
    #[test]
    fn private_method_external_call_errors() {
        let src = "type C {\n    i32 n\n    \
                   public fn make(i32 v) -> C { return C { n = v } }\n    \
                   fn secret(this) -> i32 { return this.n }\n}\n\
                   fn main() -> i32 {\n    C c = C.make(1)\n    return c.secret()\n}\n";
        let (_p, res) = check(src);
        let errs = res.expect_err("expected inaccessible-method error");
        assert!(
            errs.iter().any(|e| matches!(e,
                TypeCheckError::InaccessibleMethod { method, type_name, .. }
                if method == "secret" && type_name == "C")),
            "got {errs:?}"
        );
    }

    // 17. A private method IS callable from within the type's own methods,
    //     and a public method is callable from outside. Mirrors the private-
    //     field accessibility rule.
    #[test]
    fn private_method_internal_call_ok() {
        let src = "type C {\n    i32 n\n    \
                   public fn make(i32 v) -> C { return C { n = v } }\n    \
                   public fn doubled(this) -> i32 { return this.secret() + this.secret() }\n    \
                   fn secret(this) -> i32 { return this.n }\n}\n\
                   fn main() -> i32 {\n    C c = C.make(1)\n    return c.doubled()\n}\n";
        let (_p, res) = check(src);
        assert!(res.is_ok(), "expected ok, got {res:?}");
    }
}
