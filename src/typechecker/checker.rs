use super::errors::TypeCheckError;
use super::types::{ConstraintContext, TypeConstraint};
use crate::lexer::{LangType, TypeBase};
use crate::parser::{ExprKind, Expression, Function, Program, Statement, StatementKind};
use std::collections::HashMap;

/// Function signature for type checking
#[derive(Debug, Clone)]
struct FunctionSig {
    params: Vec<LangType>,
    return_type: LangType,
}

/// The type checker for the TJLB language.
///
/// Works in two phases:
/// 1. Collect phase: Walk the AST and collect type constraints
/// 2. Check & Fix phase: Verify all constraints and report fatal errors.
///    Where possible, apply implicit conversions (TODO).
/// 
/// TODO: notes on what needs to be done next:
/// - Refine arithmetic rules (e.g., pointer arithmetic MUST work correctly)
/// - Implement implicit conversions where applicable.
///   Do note that array variables will have to decay and thus the TODO
///   for better scope handling must be completed first.
pub struct TypeChecker {
    /// Known function signatures
    functions: HashMap<String, FunctionSig>,
    /// Variable types in current scope stack
    scopes: Vec<HashMap<String, LangType>>,
    /// Global variable types
    globals: HashMap<String, LangType>,
    /// Collected type constraints
    constraints: Vec<TypeConstraint>,
    /// Current function name (for return type checking)
    current_function: Option<String>,
}

impl TypeChecker {
    /// Create a new type checker
    #[must_use]
    pub fn new() -> Self {
        Self {
            functions: HashMap::new(),
            scopes: vec![HashMap::new()],
            globals: HashMap::new(),
            constraints: Vec::new(),
            current_function: None,
        }
    }

    /// Check a complete program and return a list of results.
    /// Each result is either Ok(()) or an error describing the problem.
    /// # Errors
    /// If any type errors are found, returns a vector of ``TypeCheckError`` which explain which restraint checks failed.
    pub fn check_program(&mut self, program: &Program) -> Result<(), Vec<TypeCheckError>> {
        // Phase 1: Register all function signatures and globals first
        self.register_declarations(program);

        // Phase 2: Collect constraints from function bodies
        for func in &program.functions {
            self.collect_function_constraints(func);
        }

        // Phase 3: Verify all constraints
        let constraint_results = self.verify_constraints();
        if constraint_results.is_empty() {
            Ok(())
        } else {
            Err(constraint_results)
        }
    }

    /// Register all function signatures and global variables
    fn register_declarations(&mut self, program: &Program) {
        // Register global variables
        for global in &program.global_vars {
            self.globals.insert(global.name.clone(), global.var_type);
        }

        // Register function signatures
        for func in &program.functions {
            let sig = FunctionSig {
                params: func.proto.params.iter().map(|(t, _)| *t).collect(),
                return_type: func.proto.return_type,
            };
            self.functions.insert(func.proto.name.clone(), sig);
        }
    }

    /// Collect constraints from a function
    fn collect_function_constraints(&mut self, func: &Function) {
        self.current_function = Some(func.proto.name.clone());
        self.enter_scope();

        // Add parameters to scope
        for (param_type, param_name) in &func.proto.params {
            self.define_var(param_name.clone(), *param_type);
        }

        // Process function body
        for stmt in &func.body {
            self.collect_statement_constraints(stmt);
        }

        self.exit_scope();
        self.current_function = None;
    }

    /// Collect constraints from a statement
    #[allow(clippy::too_many_lines)]
    fn collect_statement_constraints(&mut self, stmt: &Statement) {
        match &stmt.kind {
            StatementKind::VarDecl {
                var_type,
                name,
                initializer,
            } => {
                self.define_var(name.clone(), *var_type);
                if let Some(init_expr) = initializer {
                    let init_type = self.resolve_expression_type(init_expr);
                    self.constraints.push(TypeConstraint::Compatible {
                        expected: *var_type,
                        found: init_type,
                        pos: init_expr.pos,
                        context: ConstraintContext::Initialization,
                    });
                }
            }

            StatementKind::VarAssign { name, value } => {
                if let Some(var_type) = self.lookup_var(name) {
                    if var_type.is_const {
                        self.constraints.push(TypeConstraint::AssignmentToConst {
                            name: name.clone(),
                            pos: value.pos,
                        });
                    }
                    let value_type = self.resolve_expression_type(value);
                    self.constraints.push(TypeConstraint::Compatible {
                        expected: var_type,
                        found: value_type,
                        pos: value.pos,
                        context: ConstraintContext::Assignment,
                    });
                }
                // Note: Undefined variable errors are handled by the parser
            }

            StatementKind::DerefAssign { target, value } => {
                let target_type = self.resolve_expression_type(target);
                let value_type = self.resolve_expression_type(value);

                // target_type is already the pointee type (dereferenced).
                // Pointer validity is checked by resolve_expression_type's Dereference constraint.
                self.constraints.push(TypeConstraint::Compatible {
                    expected: target_type,
                    found: value_type,
                    pos: value.pos,
                    context: ConstraintContext::Assignment,
                });
            }

            StatementKind::Return(opt_expr) => {
                let func_name = self.current_function.clone();
                if let Some(func_name) = func_name {
                    if let Some(sig) = self.functions.get(&func_name) {
                        let expected = sig.return_type;
                        let (found, pos) = match opt_expr {
                            Some(expr) => (self.resolve_expression_type(expr), expr.pos),
                            None => (LangType::new(TypeBase::Void, 0, 0, false), stmt.pos),
                        };
                        self.constraints.push(TypeConstraint::Return {
                            expected,
                            found,
                            func_name: func_name.clone(),
                            pos,
                        });
                    }
                }
            }

            StatementKind::If {
                condition,
                then_block,
                else_block,
            } => {
                let cond_type = self.resolve_expression_type(condition);
                self.constraints.push(TypeConstraint::Condition {
                    found: cond_type,
                    pos: condition.pos,
                });

                self.enter_scope();
                for s in then_block {
                    self.collect_statement_constraints(s);
                }
                self.exit_scope();

                if let Some(else_stmts) = else_block {
                    self.enter_scope();
                    for s in else_stmts {
                        self.collect_statement_constraints(s);
                    }
                    self.exit_scope();
                }
            }

            StatementKind::While { condition, body } => {
                let cond_type = self.resolve_expression_type(condition);
                self.constraints.push(TypeConstraint::Condition {
                    found: cond_type,
                    pos: condition.pos,
                });

                self.enter_scope();
                for s in body {
                    self.collect_statement_constraints(s);
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
                    self.collect_statement_constraints(init_stmt);
                }

                if let Some(cond_expr) = condition {
                    let cond_type = self.resolve_expression_type(cond_expr);
                    self.constraints.push(TypeConstraint::Condition {
                        found: cond_type,
                        pos: cond_expr.pos,
                    });
                }

                if let Some(inc_stmt) = increment {
                    self.collect_statement_constraints(inc_stmt);
                }

                for s in body {
                    self.collect_statement_constraints(s);
                }

                self.exit_scope();
            }

            StatementKind::Block(stmts) => {
                self.enter_scope();
                for s in stmts {
                    self.collect_statement_constraints(s);
                }
                self.exit_scope();
            }

            StatementKind::Expression(expr) => {
                // Just resolve the type to collect any constraints within
                self.resolve_expression_type(expr);
            }

            StatementKind::Break | StatementKind::Continue => {
                // No type constraints to check for break/continue
            }
        }
    }

    /// Resolve the type of an expression and collect constraints
    fn resolve_expression_type(&mut self, expr: &Expression) -> LangType {
        match &expr.kind {
            ExprKind::Literal(_) => expr.expr_type,

            ExprKind::Variable(name) => self.lookup_var(name).unwrap_or(expr.expr_type),

            ExprKind::Binary { left, op, right } => {
                let left_type = self.resolve_expression_type(left);
                let right_type = self.resolve_expression_type(right);

                self.constraints.push(TypeConstraint::BinaryOp {
                    left: left_type,
                    right: right_type,
                    operator: format!("{op:?}"),
                    pos: expr.pos,
                });

                expr.expr_type
            }

            ExprKind::Comparison { left, op: _, right } => {
                let left_type = self.resolve_expression_type(left);
                let right_type = self.resolve_expression_type(right);

                self.constraints.push(TypeConstraint::Compatible {
                    expected: left_type,
                    found: right_type,
                    pos: expr.pos,
                    context: ConstraintContext::Comparison,
                });

                // Comparisons always return i32 (as boolean)
                LangType::new(TypeBase::SInt, 32, 0, false)
            }

            ExprKind::Reference(inner) => {
                let inner_type = self.resolve_expression_type(inner);
                self.constraints.push(TypeConstraint::Reference {
                    operand: inner_type,
                    pos: expr.pos,
                });
                expr.expr_type
            }

            ExprKind::Dereference(inner) => {
                let inner_type = self.resolve_expression_type(inner);
                self.constraints.push(TypeConstraint::Dereference {
                    operand: inner_type,
                    pos: expr.pos,
                });
                expr.expr_type
            }

            ExprKind::FunctionCall { name, args } => {
                let arg_types: Vec<LangType> = args
                    .iter()
                    .map(|a| self.resolve_expression_type(a))
                    .collect();

                if let Some(sig) = self.functions.get(name).cloned() {
                    self.constraints.push(TypeConstraint::FunctionCall {
                        func_name: name.clone(),
                        expected_args: sig.params,
                        found_args: arg_types,
                        pos: expr.pos,
                    });
                }

                expr.expr_type
            }

            ExprKind::Cast {
                expr: inner,
                target_type,
            } => {
                let from_type = self.resolve_expression_type(inner);
                self.constraints.push(TypeConstraint::Cast {
                    from: from_type,
                    to: *target_type,
                    pos: expr.pos,
                });
                *target_type
            }

            ExprKind::Alloc {
                alloc_type: _,
                count,
            } => {
                let count_type = self.resolve_expression_type(count);
                // Count should be an integer type
                self.constraints.push(TypeConstraint::Compatible {
                    expected: LangType::new(TypeBase::SInt, 64, 0, false),
                    found: count_type,
                    pos: count.pos,
                    context: ConstraintContext::Other("allocation count".to_string()),
                });
                expr.expr_type
            }

            ExprKind::UnaryNot(inner) => {
                let inner_type = self.resolve_expression_type(inner);
                self.constraints.push(TypeConstraint::UnaryOp {
                    operand: inner_type,
                    operator: "!".to_string(),
                    pos: expr.pos,
                });
                // Logical not returns i32
                LangType::new(TypeBase::SInt, 32, 0, false)
            }

            ExprKind::BitwiseNot(inner) => {
                let inner_type = self.resolve_expression_type(inner);
                self.constraints.push(TypeConstraint::UnaryOp {
                    operand: inner_type,
                    operator: "~".to_string(),
                    pos: expr.pos,
                });
                expr.expr_type
            }
        }
    }

    /// Verify all collected constraints and return results
    fn verify_constraints(&self) -> Vec<TypeCheckError> {
        self.constraints
            .iter()
            .map(Self::verify_constraint)
            .filter_map(Result::err)
            .collect()
    }

    /// Verify a single constraint
    #[allow(clippy::too_many_lines)]
    fn verify_constraint(constraint: &TypeConstraint) -> Result<(), TypeCheckError> {
        match constraint {
            TypeConstraint::Equal {
                expected,
                found,
                pos,
                context: _,
            } => {
                if expected != found {
                    return Err(TypeCheckError::TypeMismatch {
                        expected: *expected,
                        found: *found,
                        position: *pos,
                    });
                }
                Ok(())
            }

            TypeConstraint::Compatible {
                expected,
                found,
                pos,
                context: _,
            } => {
                if !Self::types_compatible(expected, found) {
                    return Err(TypeCheckError::TypeMismatch {
                        expected: *expected,
                        found: *found,
                        position: *pos,
                    });
                }
                Ok(())
            }

            TypeConstraint::BinaryOp {
                left,
                right,
                operator,
                pos,
            } => {
                // Allow pointer + integer and pointer - integer arithmetic
                let left_is_ptr = left.pointer_depth > 0 || left.is_array();
                let right_is_ptr = right.pointer_depth > 0 || right.is_array();
                let left_is_int = matches!(left.base, TypeBase::SInt | TypeBase::UInt) && left.pointer_depth == 0 && !left.is_array();
                let right_is_int = matches!(right.base, TypeBase::SInt | TypeBase::UInt) && right.pointer_depth == 0 && !right.is_array();

                let is_ptr_arith = (left_is_ptr && right_is_int) || (left_is_int && right_is_ptr);
                let is_valid_op = matches!(operator.as_str(), "Add" | "Sub");

                if !(Self::types_compatible(left, right) || (is_ptr_arith && is_valid_op)) {
                    return Err(TypeCheckError::InvalidBinaryOperation {
                        operator: operator.clone(),
                        left: *left,
                        right: *right,
                        position: *pos,
                    });
                }
                Ok(())
            }

            TypeConstraint::UnaryOp {
                operand,
                operator,
                pos,
            } => {
                // Most unary ops work on numeric types
                if operand.base == TypeBase::Void {
                    return Err(TypeCheckError::InvalidUnaryOperation {
                        operator: operator.clone(),
                        operand: *operand,
                        position: *pos,
                    });
                }
                Ok(())
            }

            TypeConstraint::Dereference { operand, pos } => {
                if operand.pointer_depth == 0 {
                    return Err(TypeCheckError::InvalidDereference(*operand, *pos));
                }
                Ok(())
            }

            TypeConstraint::Reference { operand: _, pos: _ } => {
                // References are generally valid; specific cases might be handled elsewhere
                Ok(())
            }

            TypeConstraint::FunctionCall {
                func_name,
                expected_args,
                found_args,
                pos,
            } => {
                if expected_args.len() != found_args.len() {
                    return Err(TypeCheckError::ArgumentCountMismatch {
                        name: func_name.clone(),
                        expected: expected_args.len(),
                        found: found_args.len(),
                        position: *pos,
                    });
                }

                for (expected, found) in expected_args.iter().zip(found_args.iter()) {
                    if !Self::types_compatible(expected, found) {
                        return Err(TypeCheckError::ArgumentTypeMismatch {
                            name: func_name.clone(),
                            expected: *expected,
                            found: *found,
                            position: *pos,
                        });
                    }
                }
                Ok(())
            }

            TypeConstraint::Return {
                expected,
                found,
                func_name: _,
                pos,
            } => {
                if !Self::types_compatible(expected, found) {
                    return Err(TypeCheckError::ReturnTypeMismatch {
                        expected: *expected,
                        found: *found,
                        position: *pos,
                    });
                }
                Ok(())
            }

            TypeConstraint::Cast { from, to, pos } => {
                if !Self::cast_valid(from, to) {
                    return Err(TypeCheckError::InvalidCast {
                        from: *from,
                        to: *to,
                        position: *pos,
                    });
                }
                Ok(())
            }

            TypeConstraint::Condition { found, pos } => {
                // Conditions must be numeric or pointer types
                if found.base == TypeBase::Void && found.pointer_depth == 0 {
                    return Err(TypeCheckError::InvalidConditionType(*found, *pos));
                }
                Ok(())
            }

            TypeConstraint::AssignmentToConst { name, pos } => {
                Err(TypeCheckError::AssignmentToConst {
                    name: name.clone(),
                    position: *pos,
                })
            }
        }
    }

    /// Check if two types are compatible (for assignment, comparison, etc.)
    fn types_compatible(expected: &LangType, actual: &LangType) -> bool {
        // Exact match
        if expected == actual {
            return true;
        }

        // Array-to-pointer decay: an array T[N] is compatible with T*
        let decayed_actual = if actual.is_array() {
            actual.decay_to_pointer()
        } else {
            *actual
        };
        let decayed_expected = if expected.is_array() {
            expected.decay_to_pointer()
        } else {
            *expected
        };
        if decayed_expected == *actual || decayed_actual == *expected || decayed_expected == decayed_actual {
            return true;
        }

        // Void is only compatible with void
        if expected.base == TypeBase::Void || actual.base == TypeBase::Void {
            return expected.base == TypeBase::Void && actual.base == TypeBase::Void;
        }

        // Pointer depth must match for pointer types
        if expected.pointer_depth != actual.pointer_depth {
            return false;
        }

        // Numeric types can be implicitly converted between same category
        matches!(
            (&expected.base, &actual.base),
            (
                TypeBase::SInt | TypeBase::UInt,
                TypeBase::SInt | TypeBase::UInt
            ) | (TypeBase::SFloat, TypeBase::SFloat)
        )
    }

    /// Check if a cast is valid
    fn cast_valid(from: &LangType, to: &LangType) -> bool {
        // Most casts between numeric types are valid
        // Pointer casts require same pointer depth or casting to/from integers
        if from.pointer_depth > 0 || to.pointer_depth > 0 {
            // Pointer to integer or integer to pointer
            if (from.pointer_depth > 0 && to.pointer_depth == 0)
                || (from.pointer_depth == 0 && to.pointer_depth > 0)
            {
                return matches!(to.base, TypeBase::SInt | TypeBase::UInt)
                    || matches!(from.base, TypeBase::SInt | TypeBase::UInt);
            }
            // Pointer to pointer
            return true;
        }

        // Numeric to numeric casts are always valid
        true
    }

    // Scope management helpers

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
        // Search scopes from innermost to outermost
        for scope in self.scopes.iter().rev() {
            if let Some(t) = scope.get(name) {
                return Some(*t);
            }
        }
        // Check globals
        self.globals.get(name).copied()
    }
}

impl Default for TypeChecker {
    fn default() -> Self {
        Self::new()
    }
}
