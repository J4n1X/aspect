use super::errors::TypeCheckError;
use super::types::{cast_valid, literal_float_compatible, literal_int_fits, types_coercible};
use crate::lexer::{LangType, Position, TypeBase};
use crate::parser::{
    BinaryOp, ExprKind, Expression, Function, GlobalVar, LiteralValue, Program, Statement,
    StatementKind,
};
use std::collections::HashMap;

/// Function signature for type checking
#[derive(Debug, Clone)]
struct FunctionSig {
    params: Vec<LangType>,
    return_type: LangType,
}

/// Single-pass type checker for the TJLB language.
///
/// Walks the AST once and emits errors directly into `self.errors`.
/// No constraint-collection phase — errors are reported immediately upon discovery.
///
/// Use `with_source_file` to include the filename in formatted error messages.
pub struct TypeChecker {
    functions: HashMap<String, FunctionSig>,
    scopes: Vec<HashMap<String, LangType>>,
    globals: HashMap<String, LangType>,
    current_function: Option<String>,
    source_file: String,
    errors: Vec<TypeCheckError>,
}

impl TypeChecker {
    /// Create a new type checker.
    #[must_use]
    pub fn new() -> Self {
        Self {
            functions: HashMap::new(),
            scopes: vec![HashMap::new()],
            globals: HashMap::new(),
            current_function: None,
            source_file: String::new(),
            errors: Vec::new(),
        }
    }

    /// Set the source file path for diagnostic messages.
    #[must_use]
    pub fn with_source_file(mut self, path: impl Into<String>) -> Self {
        self.source_file = path.into();
        self
    }

    /// Format a single error with the source file prepended.
    ///
    /// Output format: `"path/to/file.tjlb:line:col: <error message>"`
    #[must_use]
    pub fn format_error(&self, err: &TypeCheckError) -> String {
        if self.source_file.is_empty() {
            return format!("{err}");
        }
        match err.position() {
            Some(pos) => format!("{}:{}:{}: {}", self.source_file, pos.line, pos.column, err),
            None => format!("{}: {}", self.source_file, err),
        }
    }

    /// Check a complete program.
    ///
    /// # Errors
    /// Returns `Err(Vec<TypeCheckError>)` listing every type error found.
    pub fn check_program(&mut self, program: &Program) -> Result<(), Vec<TypeCheckError>> {
        self.register_declarations(program);

        for global in &program.global_vars {
            self.check_global_var(global);
        }

        for func in &program.functions {
            if !func.proto.is_extern {
                self.check_function(func);
            }
        }

        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(self.errors.drain(..).collect())
        }
    }

    // ── Declaration registration ─────────────────────────────────────────────

    fn register_declarations(&mut self, program: &Program) {
        for global in &program.global_vars {
            self.globals.insert(global.name.clone(), global.var_type);
        }
        for func in &program.functions {
            self.functions.insert(
                func.proto.name.clone(),
                FunctionSig {
                    params: func.proto.params.iter().map(|(t, _)| *t).collect(),
                    return_type: func.proto.return_type,
                },
            );
        }
    }

    // ── Global variable checking ─────────────────────────────────────────────

    fn check_global_var(&mut self, global: &GlobalVar) {
        let var_type = &global.var_type;
        if let Some(init_expr) = &global.initializer {
            if let ExprKind::ListInitializer(elements) = &init_expr.kind {
                // Validate element count
                if let Some(expected) = var_type.array_size {
                    if elements.len() > expected as usize {
                        self.errors.push(TypeCheckError::ListInitLengthMismatch {
                            expected: expected as usize,
                            found: elements.len(),
                            position: init_expr.pos,
                        });
                    }
                }
                // Validate each element type
                let elem_type = var_type.element_type();
                for elem in elements {
                    self.check_expr_coercible(
                        elem,
                        &elem_type,
                        "global array initializer element",
                        elem.pos,
                    );
                }
            } else {
                self.check_expr_coercible(
                    init_expr,
                    var_type,
                    "global variable initializer",
                    init_expr.pos,
                );
            }
        }
    }

    // ── Function checking ────────────────────────────────────────────────────

    fn check_function(&mut self, func: &Function) {
        self.current_function = Some(func.proto.name.clone());
        self.enter_scope();

        for (param_type, param_name) in &func.proto.params {
            self.define_var(param_name.clone(), *param_type);
        }

        for stmt in &func.body {
            self.check_statement(stmt);
        }

        self.exit_scope();
        self.current_function = None;
    }

    // ── Statement checking ───────────────────────────────────────────────────

    #[allow(clippy::too_many_lines)]
    fn check_statement(&mut self, stmt: &Statement) {
        match &stmt.kind {
            StatementKind::VarDecl {
                var_type,
                name,
                initializer,
            } => {
                self.define_var(name.clone(), *var_type);
                if let Some(init_expr) = initializer {
                    if let ExprKind::ListInitializer(elements) = &init_expr.kind {
                        if let Some(expected_count) = var_type.array_size {
                            if elements.len() > expected_count as usize {
                                self.errors.push(TypeCheckError::ListInitLengthMismatch {
                                    expected: expected_count as usize,
                                    found: elements.len(),
                                    position: init_expr.pos,
                                });
                            }
                        }
                        let elem_type = var_type.element_type();
                        for elem in elements {
                            self.check_expr_coercible(
                                elem,
                                &elem_type,
                                "array initializer element",
                                elem.pos,
                            );
                        }
                    } else {
                        self.check_expr_coercible(
                            init_expr,
                            var_type,
                            "initialization",
                            init_expr.pos,
                        );
                    }
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
                    self.check_expr_coercible(value, &var_type, "assignment", value.pos);
                }
            }

            StatementKind::DerefAssign { target, value } => {
                let target_type = self.check_expression(target);
                self.check_expr_coercible(value, &target_type, "dereference assignment", value.pos);
            }

            StatementKind::Return(opt_expr) => {
                if let Some(func_name) = self.current_function.clone() {
                    if let Some(sig) = self.functions.get(&func_name).cloned() {
                        match opt_expr {
                            Some(expr) => {
                                self.check_expr_coercible(
                                    expr,
                                    &sig.return_type,
                                    "return",
                                    expr.pos,
                                );
                            }
                            None => {
                                let void = LangType::new(TypeBase::Void, 0, 0, false);
                                if sig.return_type != void {
                                    self.errors.push(TypeCheckError::ReturnTypeMismatch {
                                        expected: sig.return_type,
                                        found: void,
                                        position: stmt.pos,
                                    });
                                }
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
                let cond_type = self.check_expression(condition);
                if cond_type.base == TypeBase::Void && cond_type.pointer_depth == 0 {
                    self.errors.push(TypeCheckError::InvalidConditionType(
                        cond_type,
                        condition.pos,
                    ));
                }
                self.enter_scope();
                for s in then_block {
                    self.check_statement(s);
                }
                self.exit_scope();
                if let Some(else_stmts) = else_block {
                    self.enter_scope();
                    for s in else_stmts {
                        self.check_statement(s);
                    }
                    self.exit_scope();
                }
            }

            StatementKind::While { condition, body } => {
                let cond_type = self.check_expression(condition);
                if cond_type.base == TypeBase::Void && cond_type.pointer_depth == 0 {
                    self.errors.push(TypeCheckError::InvalidConditionType(
                        cond_type,
                        condition.pos,
                    ));
                }
                self.enter_scope();
                for s in body {
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
                    let cond_type = self.check_expression(cond_expr);
                    if cond_type.base == TypeBase::Void && cond_type.pointer_depth == 0 {
                        self.errors.push(TypeCheckError::InvalidConditionType(
                            cond_type,
                            cond_expr.pos,
                        ));
                    }
                }
                if let Some(inc_stmt) = increment {
                    self.check_statement(inc_stmt);
                }
                for s in body {
                    self.check_statement(s);
                }
                self.exit_scope();
            }

            StatementKind::Block(stmts) => {
                self.enter_scope();
                for s in stmts {
                    self.check_statement(s);
                }
                self.exit_scope();
            }

            StatementKind::Expression(expr) => {
                self.check_expression(expr);
            }

            StatementKind::Break | StatementKind::Continue => {}
        }
    }

    // ── Expression type resolution ───────────────────────────────────────────

    /// Walk an expression, emit any type errors found, and return its resolved type.
    fn check_expression(&mut self, expr: &Expression) -> LangType {
        match &expr.kind {
            ExprKind::Literal(_) => expr.expr_type,

            ExprKind::Variable(name) => {
                if let Some(ty) = self.lookup_var(name) {
                    ty
                } else {
                    self.errors
                        .push(TypeCheckError::UndefinedVariable(name.clone(), expr.pos));
                    expr.expr_type
                }
            }

            ExprKind::Binary { left, op, right } => {
                let left_type = self.check_expression(left);
                let right_type = self.check_expression(right);

                if !Self::binary_op_types_valid(&left_type, &right_type, op) {
                    self.errors.push(TypeCheckError::InvalidBinaryOperation {
                        operator: format!("{op:?}"),
                        left: left_type,
                        right: right_type,
                        position: expr.pos,
                    });
                }
                // Result type: the wider of the two operand types (or left if equal)
                Self::wider_type(&left_type, &right_type)
            }

            ExprKind::Comparison { left, op: _, right } => {
                let left_type = self.check_expression(left);
                let right_type = self.check_expression(right);

                if !Self::binary_op_types_valid(&left_type, &right_type, &BinaryOp::Add) {
                    self.errors.push(TypeCheckError::InvalidBinaryOperation {
                        operator: "comparison".to_string(),
                        left: left_type,
                        right: right_type,
                        position: expr.pos,
                    });
                }
                LangType::new(TypeBase::SInt, 32, 0, false)
            }

            ExprKind::Reference(inner) => {
                self.check_expression(inner);
                expr.expr_type
            }

            ExprKind::Dereference(inner) => {
                let inner_type = self.check_expression(inner);
                // Arrays and pointers are both valid dereference targets.
                // Array subscript `arr[i]` is lowered to `*(arr + i)` by the parser,
                // so array types (pointer_depth == 0 but is_array()) must be accepted here.
                if inner_type.pointer_depth == 0 && !inner_type.is_array() {
                    self.errors
                        .push(TypeCheckError::InvalidDereference(inner_type, expr.pos));
                }
                expr.expr_type
            }

            ExprKind::FunctionCall { name, args } => {
                // Evaluate all arg types first
                let arg_types: Vec<LangType> =
                    args.iter().map(|a| self.check_expression(a)).collect();

                if let Some(sig) = self.functions.get(name).cloned() {
                    if sig.params.len() != arg_types.len() {
                        self.errors.push(TypeCheckError::ArgumentCountMismatch {
                            name: name.clone(),
                            expected: sig.params.len(),
                            found: arg_types.len(),
                            position: expr.pos,
                        });
                    } else {
                        for (i, (param, arg_expr)) in sig.params.iter().zip(args.iter()).enumerate()
                        {
                            if !Self::expr_coercible_to(arg_expr, param) {
                                let arg_type = arg_types[i];
                                self.errors.push(TypeCheckError::ArgumentTypeMismatch {
                                    name: name.clone(),
                                    expected: *param,
                                    found: arg_type,
                                    position: arg_expr.pos,
                                });
                            }
                        }
                    }
                } else {
                    self.errors
                        .push(TypeCheckError::UndefinedFunction(name.clone(), expr.pos));
                }
                expr.expr_type
            }

            ExprKind::Cast {
                expr: inner,
                target_type,
            } => {
                let from_type = self.check_expression(inner);
                if !cast_valid(&from_type, target_type) {
                    self.errors.push(TypeCheckError::InvalidCast {
                        from: from_type,
                        to: *target_type,
                        position: expr.pos,
                    });
                }
                *target_type
            }

            ExprKind::Alloc {
                alloc_type: _,
                count,
            } => {
                let count_type = self.check_expression(count);
                if !matches!(count_type.base, TypeBase::SInt | TypeBase::UInt)
                    || count_type.pointer_depth > 0
                {
                    self.errors.push(TypeCheckError::TypeMismatch {
                        expected: LangType::new(TypeBase::UInt, 64, 0, false),
                        found: count_type,
                        position: count.pos,
                    });
                }
                expr.expr_type
            }

            ExprKind::UnaryNot(inner) => {
                let inner_type = self.check_expression(inner);
                if inner_type.base == TypeBase::Void {
                    self.errors.push(TypeCheckError::InvalidUnaryOperation {
                        operator: "!".to_string(),
                        operand: inner_type,
                        position: expr.pos,
                    });
                }
                LangType::new(TypeBase::SInt, 32, 0, false)
            }

            ExprKind::BitwiseNot(inner) => {
                let inner_type = self.check_expression(inner);
                if inner_type.base == TypeBase::Void {
                    self.errors.push(TypeCheckError::InvalidUnaryOperation {
                        operator: "~".to_string(),
                        operand: inner_type,
                        position: expr.pos,
                    });
                }
                expr.expr_type
            }

            ExprKind::ListInitializer(elements) => {
                for elem in elements {
                    self.check_expression(elem);
                }
                expr.expr_type
            }
        }
    }

    // ── Coercibility helpers ─────────────────────────────────────────────────

    /// Check if `expr` can be coerced to `target`. Emits an error on failure.
    fn check_expr_coercible(
        &mut self,
        expr: &Expression,
        target: &LangType,
        context: &str,
        pos: Position,
    ) {
        if Self::expr_coercible_to(expr, target) {
            // Visit sub-expressions for their own errors
            self.check_expression(expr);
        } else {
            let found = self.check_expression(expr);
            self.errors.push(TypeCheckError::TypeMismatch {
                expected: *target,
                found,
                position: pos,
            });
            let _ = context; // context available for richer messages in the future
        }
    }

    /// Pure predicate: can `expr` be used where `target` is expected?
    ///
    /// - Integer/float literals: value-based fit check
    /// - All other expressions: `types_coercible`
    fn expr_coercible_to(expr: &Expression, target: &LangType) -> bool {
        match &expr.kind {
            ExprKind::Literal(LiteralValue::Integer(val)) => {
                literal_int_fits(*val, target) || types_coercible(&expr.expr_type, target)
            }
            ExprKind::Literal(LiteralValue::Float(_)) => {
                literal_float_compatible(target) || types_coercible(&expr.expr_type, target)
            }
            _ => types_coercible(&expr.expr_type, target),
        }
    }

    // ── Binary op helpers ────────────────────────────────────────────────────

    /// Check if two operand types are valid for the given binary operation.
    fn binary_op_types_valid(left: &LangType, right: &LangType, op: &BinaryOp) -> bool {
        // Pointer arithmetic: ptr ± int or int ± ptr
        let left_is_ptr = left.pointer_depth > 0 || left.is_array();
        let right_is_ptr = right.pointer_depth > 0 || right.is_array();
        let left_is_int = matches!(left.base, TypeBase::SInt | TypeBase::UInt)
            && left.pointer_depth == 0
            && !left.is_array();
        let right_is_int = matches!(right.base, TypeBase::SInt | TypeBase::UInt)
            && right.pointer_depth == 0
            && !right.is_array();

        if matches!(op, BinaryOp::Add | BinaryOp::Sub)
            && ((left_is_ptr && right_is_int) || (left_is_int && right_is_ptr))
        {
            return true;
        }

        // Both same family — either side can widen to the other
        types_coercible(left, right) || types_coercible(right, left)
    }

    /// Return the "wider" of two types (for binary-op result typing).
    /// Falls back to `left` when types are incomparable.
    fn wider_type(left: &LangType, right: &LangType) -> LangType {
        if left.pointer_depth > 0 || left.is_array() {
            return *left;
        }
        if left.size_bits >= right.size_bits {
            *left
        } else {
            *right
        }
    }

    // ── Scope helpers ────────────────────────────────────────────────────────

    fn enter_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn exit_scope(&mut self) {
        self.scopes.pop();
    }

    fn define_var(&mut self, name: String, var_type: LangType) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name, var_type);
        }
    }

    fn lookup_var(&self, name: &str) -> Option<LangType> {
        for scope in self.scopes.iter().rev() {
            if let Some(t) = scope.get(name) {
                return Some(*t);
            }
        }
        self.globals.get(name).copied()
    }
}

impl Default for TypeChecker {
    fn default() -> Self {
        Self::new()
    }
}
