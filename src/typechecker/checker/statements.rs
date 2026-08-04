use super::TypeChecker;
use crate::lexer::{LangType, TypeBase};
use crate::parser::{ExprKind, Expression, Statement, StatementKind};
use crate::typechecker::errors::TypeCheckError;

impl TypeChecker {
    /// Type-check `stmts` in a fresh lexical scope (enter, check each, exit).
    fn check_scoped(&mut self, stmts: &mut [Statement]) {
        self.enter_scope();
        for s in stmts.iter_mut() {
            self.check_statement(s);
        }
        self.exit_scope();
    }

    pub(crate) fn check_statement(&mut self, stmt: &mut Statement) {
        let stmt_pos = stmt.pos;
        match &mut stmt.kind {
            StatementKind::Switch {
                scrutinee,
                arms,
                default,
                complete,
            } => {
                let complete = *complete;
                self.check_switch(scrutinee, arms, default, complete, stmt_pos);
            }
            StatementKind::VarDecl {
                var_type,
                name,
                initializer,
            } => {
                let var_type = *var_type;
                self.reject_void_value(var_type, stmt_pos);
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
                // `*p = x` can't write through a const pointer's pointee. The
                // `Dereference` synth arm propagates const downward, so writes
                // through a const chain (`**pp = x`) are caught here too.
                if target_type.is_const {
                    self.errors
                        .push(TypeCheckError::WriteThroughConst { position: target.pos });
                }
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
                // Inside a value-block, `return` yields the innermost block, not
                // the function. In synthesis position the first `return` fixes
                // the type and later ones are checked against it.
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
                // One scope spans condition + then-block (mirroring the
                // parser): `is` bindings defined while walking the condition
                // are visible exactly there. The else never sees them.
                self.enter_scope();
                self.check_condition_with_bindings(condition);
                for s in then_block.iter_mut() {
                    self.check_statement(s);
                }
                self.exit_scope();
                if let Some(else_stmts) = else_block {
                    self.check_scoped(else_stmts);
                }
            }

            StatementKind::While { condition, body } => {
                self.enter_scope();
                self.check_condition_with_bindings(condition);
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
                self.check_scoped(stmts);
            }

            StatementKind::Expression(expr) => {
                self.synth_expression(expr);
            }

            StatementKind::Break | StatementKind::Continue => {}
        }
    }

    /// An `if`/`while` condition: leaves of the root `&&` spine may be
    /// binding `is` conjuncts — validate each leaf, stamp `bool` on the
    /// spine, and define binders (textual order, i.e. left to right) into
    /// the scope the caller opened around condition + block. Everything
    /// that is not a spine `&&` or a binding `is` checks as an ordinary
    /// condition — a binding `is` nested anywhere deeper is caught by the
    /// `synth_expression` arm's not-an-expression error.
    fn check_condition_with_bindings(&mut self, cond: &mut Expression) {
        match &mut cond.kind {
            ExprKind::Binary {
                op: crate::parser::BinaryOp::LogicalAnd,
                left,
                right,
            } => {
                self.check_condition_with_bindings(left);
                self.check_condition_with_bindings(right);
                cond.expr_type = LangType::BOOL;
            }
            ExprKind::IsBinding {
                scrutinee, binders, ..
            } => {
                self.synth_expression(scrutinee);
                let binders = binders.clone();
                for (name, ty) in binders.into_iter().flatten() {
                    self.define_var(name, ty);
                }
                cond.expr_type = LangType::BOOL;
            }
            _ => self.check_condition(cond),
        }
    }

    /// Conditions impose no target type, so they run in synthesis mode; `void`
    /// is then rejected as not a truth value.
    fn check_condition(&mut self, cond: &mut Expression) {
        let cond_type = self.synth_expression(cond);
        if cond_type.is_void_value() {
            self.errors
                .push(TypeCheckError::InvalidConditionType(cond_type, cond.pos));
        }
    }

    /// `target` is `Some` in checked positions (the block must yield that type)
    /// and `None` in synthesis positions (the first `return` fixes the type).
    /// Also enforces the all-paths rule: every control path must end in a
    /// `return`, conservatively (loops never count, even `while true`).
    pub(crate) fn check_value_block(
        &mut self,
        stmts: &mut [Statement],
        target: Option<LangType>,
        pos: crate::lexer::Position,
    ) -> LangType {
        self.value_block_types.push(target);
        self.check_scoped(stmts);
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

    /// Conservative "every path returns": a list returns iff any statement in
    /// it definitely returns. Loops never count (`break` could skip their
    /// returns), and returns inside *nested* value-blocks live under an
    /// expression this walk doesn't descend into, so they don't satisfy the
    /// outer block.
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
            // A switch always returns iff it is coverage-complete (arms alone,
            // or a `default`) and every reachable body always returns. The
            // parser computed `complete` dedup-aware, so no registry is needed.
            StatementKind::Switch {
                arms,
                default,
                complete,
                ..
            } => {
                (*complete || default.is_some())
                    && arms.iter().all(|arm| Self::always_returns(&arm.body))
                    && default.as_ref().map_or(true, |d| Self::always_returns(d))
            }
            _ => false,
        }
    }

    /// Type-check a `switch`: scrutinee class, per-pattern validity and
    /// duplicates, arm bodies (bindings in scope), and the exhaustiveness
    /// stances — one rule, "a switch must cover its scrutinee; `default`
    /// covers the rest".
    #[allow(clippy::too_many_lines)]
    fn check_switch(
        &mut self,
        scrutinee: &mut Expression,
        arms: &mut [crate::parser::SwitchArm],
        default: &mut Option<Vec<Statement>>,
        complete: bool,
        stmt_pos: crate::lexer::Position,
    ) {
        use crate::parser::{LiteralValue, SwitchPattern};
        use std::collections::HashSet;

        let s_ty = self.synth_expression(scrutinee);
        let s_pos = scrutinee.pos;

        enum Class {
            Int,
            Bool,
            Enum(u32),
            Sum(u32),
            Bad,
        }
        let class = if s_ty.pointer_depth > 0 || s_ty.is_array() {
            Class::Bad
        } else {
            match s_ty.base {
                TypeBase::SInt | TypeBase::UInt => Class::Int,
                TypeBase::Bool => Class::Bool,
                TypeBase::Enum(id) => Class::Enum(id),
                TypeBase::Sum(id) => Class::Sum(id),
                _ => Class::Bad,
            }
        };
        if matches!(class, Class::Bad) {
            let hint = if s_ty.base == TypeBase::SFloat && s_ty.pointer_depth == 0 {
                " — floats have no exact equality; use if/elif"
            } else {
                ""
            };
            self.errors.push(TypeCheckError::InvalidSwitchScrutinee {
                ty: self.type_name(&s_ty),
                hint,
                position: s_pos,
            });
        }

        // Pattern validity + duplicate detection (values are compared after
        // evaluation, so `0x10` duplicates `16`).
        let mut seen_consts: HashSet<i64> = HashSet::new();
        let mut seen_variants: HashSet<u32> = HashSet::new();
        for arm in arms.iter_mut() {
            for pattern in &mut arm.patterns {
                match pattern {
                    SwitchPattern::Const(e) => {
                        self.check_expression(e, &s_ty);
                        let key = match &e.kind {
                            ExprKind::Literal(LiteralValue::Integer(v)) => Some(*v),
                            ExprKind::Literal(LiteralValue::Bool(b)) => Some(i64::from(*b)),
                            ExprKind::EnumValue { value, .. } => Some(*value),
                            _ => {
                                self.errors.push(TypeCheckError::NonConstantPattern(e.pos));
                                None
                            }
                        };
                        if let Some(key) = key
                            && !seen_consts.insert(key)
                        {
                            let what = match (&class, &e.kind) {
                                (Class::Enum(id), ExprKind::EnumValue { value, .. }) => {
                                    let info = self.symbols.enum_info(*id);
                                    format!(
                                        "variant '{}'",
                                        info.variants
                                            .get(*value as usize)
                                            .map_or("?", String::as_str)
                                    )
                                }
                                (Class::Bool, ExprKind::Literal(LiteralValue::Bool(b))) => {
                                    format!("`{b}`")
                                }
                                _ => format!("value {key}"),
                            };
                            self.errors.push(TypeCheckError::SwitchDuplicateCase {
                                what,
                                position: e.pos,
                            });
                        }
                    }
                    SwitchPattern::SumVariant { variant, .. } => {
                        if !seen_variants.insert(*variant) {
                            let what = if let Class::Sum(id) = class {
                                format!(
                                    "variant '{}'",
                                    self.symbols.sum_info(id).variants[*variant as usize].name
                                )
                            } else {
                                format!("variant #{variant}")
                            };
                            self.errors.push(TypeCheckError::SwitchDuplicateCase {
                                what,
                                position: arm.pos,
                            });
                        }
                    }
                }
            }

            // Body with the pattern's bindings in scope (copies — ordinary
            // locals of the payload field types).
            self.enter_scope();
            for pattern in &arm.patterns {
                if let SwitchPattern::SumVariant { binders, .. } = pattern {
                    for (name, ty) in binders.iter().flatten() {
                        self.define_var(name.clone(), *ty);
                    }
                }
            }
            for stmt in &mut arm.body {
                self.check_statement(stmt);
            }
            self.exit_scope();
        }
        if let Some(d) = default {
            self.check_scoped(d);
        }

        // Exhaustiveness stances.
        match class {
            Class::Int => {
                if default.is_none() {
                    self.errors.push(TypeCheckError::SwitchMissingDefault {
                        ty: self.type_name(&s_ty),
                        position: stmt_pos,
                    });
                }
            }
            Class::Bool => {
                if !complete && default.is_none() {
                    let missing = if seen_consts.contains(&1) { "`false`" } else { "`true`" };
                    self.errors.push(TypeCheckError::SwitchNonExhaustive {
                        missing: missing.to_string(),
                        position: stmt_pos,
                    });
                }
            }
            Class::Enum(id) => {
                let names: Vec<String> = self
                    .symbols
                    .enum_info(id)
                    .variants
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !seen_consts.contains(&(*i as i64)))
                    .map(|(_, n)| format!("'{n}'"))
                    .collect();
                self.finish_variant_stances(&names, complete, default.is_some(), stmt_pos);
            }
            Class::Sum(id) => {
                let names: Vec<String> = self
                    .symbols
                    .sum_info(id)
                    .variants
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !seen_variants.contains(&(*i as u32)))
                    .map(|(_, v)| format!("'{}'", v.name))
                    .collect();
                self.finish_variant_stances(&names, complete, default.is_some(), stmt_pos);
            }
            Class::Bad => {}
        }
    }

    /// Stances 3–4, shared by enums and sums: fully listed + `default` is a
    /// dead arm (warning — it would silently swallow future variants); missing
    /// variants without `default` is an error naming them.
    fn finish_variant_stances(
        &mut self,
        missing: &[String],
        complete: bool,
        has_default: bool,
        pos: crate::lexer::Position,
    ) {
        if complete && has_default {
            self.warnings.push(crate::typechecker::errors::TypeWarning {
                message: "`default` arm is dead — every variant is already handled; \
                          it would silently swallow variants added later"
                    .to_string(),
                position: pos,
            });
        } else if !complete && !has_default {
            self.errors.push(TypeCheckError::SwitchNonExhaustive {
                missing: format!("variants {}", missing.join(", ")),
                position: pos,
            });
        }
    }
}
