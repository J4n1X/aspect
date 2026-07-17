use super::TypeChecker;
use crate::lexer::LangType;
use crate::parser::{ExprKind, Expression, Statement, StatementKind};
use crate::typechecker::errors::TypeCheckError;

impl TypeChecker {
    // ── Statement checking ───────────────────────────────────────────────────

    pub(crate) fn check_statement(&mut self, stmt: &mut Statement) {
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
    pub(crate) fn check_value_block(
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
}
