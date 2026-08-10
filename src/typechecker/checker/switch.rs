use super::TypeChecker;
use crate::lexer::{LangType, Position, TypeBase};
use crate::parser::{ExprKind, Expression, LiteralValue, Statement, SwitchArm, SwitchPattern};
use crate::typechecker::errors::TypeCheckError;
use crate::variants::{ClosedSpace, VariantSpace};
use std::collections::HashSet;

impl TypeChecker {
    /// Type-check a `switch`: scrutinee validity, per-pattern validity and
    /// duplicates, arm bodies (bindings in scope), and the exhaustiveness
    /// stances — one rule, "a switch must cover its scrutinee; `default`
    /// covers the rest".
    pub(crate) fn check_switch(
        &mut self,
        scrutinee: &mut Expression,
        arms: &mut [SwitchArm],
        default: &mut Option<Vec<Statement>>,
        complete: bool,
        stmt_pos: Position,
    ) {
        let s_ty = self.synth_expression(scrutinee);
        let s_pos = scrutinee.pos;

        let space = VariantSpace::of(&s_ty, &self.symbols);
        if space == VariantSpace::Unmatchable {
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

        // One `seen` set for both pattern shapes: a scrutinee admits only one of
        // them, so a slot index and a case value can never collide. Keyed by
        // value rather than slot so negative case values stay distinct.
        let mut seen: HashSet<i64> = HashSet::new();
        for arm in arms.iter_mut() {
            for pattern in &mut arm.patterns {
                match pattern {
                    SwitchPattern::Const(e) => {
                        self.check_const_pattern(e, &s_ty, space, &mut seen);
                    }
                    SwitchPattern::SumVariant { variant, .. } => {
                        if !seen.insert(i64::from(*variant)) {
                            self.report_duplicate_case(space, i64::from(*variant), arm.pos);
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

        self.check_switch_exhaustiveness(
            space,
            &s_ty,
            complete,
            default.is_some(),
            &seen,
            stmt_pos,
        );
    }

    fn check_const_pattern(
        &mut self,
        e: &mut Expression,
        s_ty: &LangType,
        space: VariantSpace,
        seen: &mut HashSet<i64>,
    ) {
        self.check_expression(e, s_ty);
        // Values are compared after evaluation, so `0x10` duplicates `16`.
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
            && !seen.insert(key)
        {
            self.report_duplicate_case(space, key, e.pos);
        }
    }

    /// A closed space names the slot (`variant 'A'`, `` `true` ``); an open one
    /// can only quote the value.
    fn report_duplicate_case(&mut self, space: VariantSpace, key: i64, position: Position) {
        let slot = usize::try_from(key).ok();
        let what = match (space.closed(), slot) {
            (Some(s), Some(slot)) if slot < s.count => s.duplicate_label(slot, &self.symbols),
            _ => format!("value {key}"),
        };
        self.errors
            .push(TypeCheckError::SwitchDuplicateCase { what, position });
    }

    fn check_switch_exhaustiveness(
        &mut self,
        space: VariantSpace,
        s_ty: &LangType,
        complete: bool,
        has_default: bool,
        seen: &HashSet<i64>,
        stmt_pos: Position,
    ) {
        match space {
            VariantSpace::Open => {
                if !has_default {
                    self.errors.push(TypeCheckError::SwitchMissingDefault {
                        ty: self.type_name(s_ty),
                        position: stmt_pos,
                    });
                }
            }
            VariantSpace::Closed(space) => {
                let slots: HashSet<usize> =
                    seen.iter().filter_map(|k| usize::try_from(*k).ok()).collect();
                self.check_closed_stances(space, complete, has_default, &slots, stmt_pos);
            }
            VariantSpace::Unmatchable => {}
        }
    }

    /// Stances 3–4, shared by enums, sums and bool: fully listed + `default` is
    /// a dead arm (warning — it would silently swallow future variants); missing
    /// variants without `default` is an error naming them.
    fn check_closed_stances(
        &mut self,
        space: ClosedSpace,
        complete: bool,
        has_default: bool,
        slots: &HashSet<usize>,
        pos: Position,
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
                missing: space.missing_label(slots, &self.symbols),
                position: pos,
            });
        }
    }
}
