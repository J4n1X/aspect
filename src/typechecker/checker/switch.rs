use super::TypeChecker;
use crate::lexer::{LangType, Position, TypeBase};
use crate::parser::{ExprKind, Expression, LiteralValue, Statement, SwitchArm, SwitchPattern};
use crate::typechecker::errors::TypeCheckError;
use std::collections::HashSet;

enum Class {
    Int,
    Bool,
    Enum(u32),
    Sum(u32),
    Bad,
}

fn classify_scrutinee(s_ty: LangType) -> Class {
    if s_ty.is_array() {
        return Class::Bad;
    }
    // A single-level pointer to a sum auto-derefs (like field access);
    // deeper pointers and pointers to anything else stay invalid.
    if s_ty.pointer_depth > 0 {
        return if s_ty.pointer_depth == 1
            && let TypeBase::Sum(id) = s_ty.base
        {
            Class::Sum(id)
        } else {
            Class::Bad
        };
    }
    match s_ty.base {
        TypeBase::SInt | TypeBase::UInt => Class::Int,
        TypeBase::Bool => Class::Bool,
        TypeBase::Enum(id) => Class::Enum(id),
        TypeBase::Sum(id) => Class::Sum(id),
        _ => Class::Bad,
    }
}

/// Bundles the two duplicate-detection sets so `check_switch_exhaustiveness`
/// stays under `clippy::too_many_arguments`.
struct SeenPatterns<'a> {
    consts: &'a HashSet<i64>,
    variants: &'a HashSet<u32>,
}

impl TypeChecker {
    /// Type-check a `switch`: scrutinee class, per-pattern validity and
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

        let class = classify_scrutinee(s_ty);
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
                        self.check_const_pattern(e, &s_ty, &class, &mut seen_consts);
                    }
                    SwitchPattern::SumVariant { variant, .. } => {
                        self.check_sum_variant_pattern(
                            *variant,
                            arm.pos,
                            &class,
                            &mut seen_variants,
                        );
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
            &class,
            &s_ty,
            complete,
            default,
            SeenPatterns {
                consts: &seen_consts,
                variants: &seen_variants,
            },
            stmt_pos,
        );
    }

    fn check_const_pattern(
        &mut self,
        e: &mut Expression,
        s_ty: &LangType,
        class: &Class,
        seen_consts: &mut HashSet<i64>,
    ) {
        self.check_expression(e, s_ty);
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
            let what = match (class, &e.kind) {
                (Class::Enum(id), ExprKind::EnumValue { value, .. }) => {
                    let info = self.symbols.enum_info(*id);
                    format!(
                        "variant '{}'",
                        info.variants.get(*value as usize).map_or("?", String::as_str)
                    )
                }
                (Class::Bool, ExprKind::Literal(LiteralValue::Bool(b))) => format!("`{b}`"),
                _ => format!("value {key}"),
            };
            self.errors.push(TypeCheckError::SwitchDuplicateCase {
                what,
                position: e.pos,
            });
        }
    }

    fn check_sum_variant_pattern(
        &mut self,
        variant: u32,
        arm_pos: Position,
        class: &Class,
        seen_variants: &mut HashSet<u32>,
    ) {
        if !seen_variants.insert(variant) {
            let what = if let Class::Sum(id) = class {
                format!(
                    "variant '{}'",
                    self.symbols.sum_info(*id).variants[variant as usize].name
                )
            } else {
                format!("variant #{variant}")
            };
            self.errors.push(TypeCheckError::SwitchDuplicateCase {
                what,
                position: arm_pos,
            });
        }
    }

    fn check_switch_exhaustiveness(
        &mut self,
        class: &Class,
        s_ty: &LangType,
        complete: bool,
        default: &Option<Vec<Statement>>,
        seen: SeenPatterns<'_>,
        stmt_pos: Position,
    ) {
        match class {
            Class::Int => {
                if default.is_none() {
                    self.errors.push(TypeCheckError::SwitchMissingDefault {
                        ty: self.type_name(s_ty),
                        position: stmt_pos,
                    });
                }
            }
            Class::Bool => {
                if !complete && default.is_none() {
                    let missing = if seen.consts.contains(&1) {
                        "`false`"
                    } else {
                        "`true`"
                    };
                    self.errors.push(TypeCheckError::SwitchNonExhaustive {
                        missing: missing.to_string(),
                        position: stmt_pos,
                    });
                }
            }
            Class::Enum(id) => {
                let names: Vec<String> = self
                    .symbols
                    .enum_info(*id)
                    .variants
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !seen.consts.contains(&(*i as i64)))
                    .map(|(_, n)| format!("'{n}'"))
                    .collect();
                self.finish_variant_stances(&names, complete, default.is_some(), stmt_pos);
            }
            Class::Sum(id) => {
                let names: Vec<String> = self
                    .symbols
                    .sum_info(*id)
                    .variants
                    .iter()
                    .enumerate()
                    .filter(|(i, _)| !seen.variants.contains(&(*i as u32)))
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
                missing: format!("variants {}", missing.join(", ")),
                position: pos,
            });
        }
    }
}
