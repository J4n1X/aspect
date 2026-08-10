//! What a `switch` scrutinee can be matched against, and how its variants are
//! spelled in diagnostics.
//!
//! The one copy of that taxonomy: the parser's coverage calculation, the
//! checker's exhaustiveness stances and codegen's discriminant extraction all
//! classify through here, so the rule that a single-level pointer to a sum
//! auto-derefs is stated once. Pure data over a `LangType` plus the registry, so
//! it is usable before any LLVM target machine exists (like `target.rs`).

use std::collections::HashSet;

use crate::lexer::{LangType, TypeBase};
use crate::parser::{ExprKind, Expression, LiteralValue, SwitchPattern};
use crate::symbol::module::ModuleSymbols;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosedKind {
    Enum(u32),
    Sum(u32),
    Bool,
}

/// A scrutinee whose values can be enumerated, so listing them all is
/// exhaustive without a `default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClosedSpace {
    pub kind: ClosedKind,
    /// Variants occupy slots `0..count`; a slot index *is* the variant's
    /// value/discriminant.
    pub count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VariantSpace {
    Closed(ClosedSpace),
    /// Integers: the type cannot be enumerated, so `default` is the only route
    /// to coverage.
    Open,
    /// Floats (no exact equality), deep pointers, arrays, structs.
    Unmatchable,
}

/// The sum a scrutinee of this type matches against. A single-level pointer
/// auto-derefs like field access does; deeper pointers and arrays are not
/// scrutinees. Also the "does this switch need a payload slot" test in codegen.
#[must_use]
pub fn sum_scrutinee(ty: &LangType) -> Option<u32> {
    if ty.is_array() || ty.pointer_depth > 1 {
        return None;
    }
    match ty.base {
        TypeBase::Sum(id) => Some(id),
        _ => None,
    }
}

impl VariantSpace {
    #[must_use]
    pub fn of(ty: &LangType, symbols: &ModuleSymbols) -> Self {
        if let Some(id) = sum_scrutinee(ty) {
            return Self::Closed(ClosedSpace {
                kind: ClosedKind::Sum(id),
                count: symbols.type_def(id).as_sum().variants.len(),
            });
        }
        if ty.is_array() || ty.pointer_depth > 0 {
            return Self::Unmatchable;
        }
        match ty.base {
            TypeBase::SInt | TypeBase::UInt => Self::Open,
            TypeBase::Bool => Self::Closed(ClosedSpace {
                kind: ClosedKind::Bool,
                count: 2,
            }),
            TypeBase::Enum(id) => Self::Closed(ClosedSpace {
                kind: ClosedKind::Enum(id),
                count: symbols.type_def(id).as_enum().variants.len(),
            }),
            _ => Self::Unmatchable,
        }
    }

    #[must_use]
    pub fn closed(self) -> Option<ClosedSpace> {
        match self {
            Self::Closed(space) => Some(space),
            _ => None,
        }
    }
}

impl ClosedSpace {
    /// Which slot `pattern` covers, or `None` when it does not name one of this
    /// space's variants — a type error the checker reports separately.
    #[must_use]
    pub fn pattern_slot(&self, pattern: &SwitchPattern) -> Option<usize> {
        match (self.kind, pattern) {
            (ClosedKind::Sum(_), SwitchPattern::SumVariant { variant, .. }) => {
                Some(*variant as usize)
            }
            (
                ClosedKind::Enum(_),
                SwitchPattern::Const(Expression {
                    kind: ExprKind::EnumValue { value, .. },
                    ..
                }),
            ) => usize::try_from(*value).ok(),
            (
                ClosedKind::Bool,
                SwitchPattern::Const(Expression {
                    kind: ExprKind::Literal(LiteralValue::Bool(b)),
                    ..
                }),
            ) => Some(usize::from(*b)),
            _ => None,
        }
    }

    /// Do these patterns alone cover every slot? Dedup-aware, so repeating a
    /// variant does not fake coverage.
    pub fn covered_by<'a>(&self, patterns: impl Iterator<Item = &'a SwitchPattern>) -> bool {
        let seen: HashSet<usize> = patterns.filter_map(|p| self.pattern_slot(p)).collect();
        (0..self.count).all(|slot| seen.contains(&slot))
    }

    /// How one slot is spelled in a diagnostic: `'Circle'` for a named variant,
    /// `` `true` `` for a bool.
    #[must_use]
    pub fn slot_label(&self, slot: usize, symbols: &ModuleSymbols) -> String {
        match self.kind {
            ClosedKind::Enum(id) => symbols
                .type_def(id)
                .as_enum()
                .variants
                .get(slot)
                .map_or_else(|| "'?'".to_string(), |name| format!("'{name}'")),
            ClosedKind::Sum(id) => symbols
                .type_def(id)
                .as_sum()
                .variants
                .get(slot)
                .map_or_else(|| "'?'".to_string(), |v| format!("'{}'", v.name)),
            ClosedKind::Bool => {
                let lit = if slot == 1 { "true" } else { "false" };
                format!("`{lit}`")
            }
        }
    }

    /// The `duplicate case …` phrase.
    #[must_use]
    pub fn duplicate_label(&self, slot: usize, symbols: &ModuleSymbols) -> String {
        let label = self.slot_label(slot, symbols);
        match self.kind {
            ClosedKind::Bool => label,
            _ => format!("variant {label}"),
        }
    }

    /// The `switch does not handle …` phrase for every slot not in `seen`.
    #[must_use]
    pub fn missing_label(&self, seen: &HashSet<usize>, symbols: &ModuleSymbols) -> String {
        let missing: Vec<String> = (0..self.count)
            .filter(|slot| !seen.contains(slot))
            .map(|slot| self.slot_label(slot, symbols))
            .collect();
        match self.kind {
            ClosedKind::Bool => missing.join(", "),
            _ => format!("variants {}", missing.join(", ")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Position;
    use crate::symbol::module::{
        EnumBody, SumBody, SumVariant, TypeKind, Visibility,
    };

    /// A registry holding `enum Color { Red, Green }` and
    /// `sum Shape { Dot, Mark(i32) }`.
    fn symbols() -> (ModuleSymbols, u32, u32) {
        let mut m = ModuleSymbols::new();
        let color = m.intern_type(
            "Color",
            0,
            Visibility::Private,
            Position::new(0, 0),
            TypeKind::Enum(EnumBody::default()),
        );
        m.set_enum_variants(color, vec!["Red".to_string(), "Green".to_string()]);
        let shape = m.intern_type(
            "Shape",
            0,
            Visibility::Private,
            Position::new(0, 0),
            TypeKind::Sum(SumBody::default()),
        );
        m.set_sum_variants(
            shape,
            vec![
                SumVariant {
                    name: "Dot".to_string(),
                    fields: Vec::new(),
                },
                SumVariant {
                    name: "Mark".to_string(),
                    fields: vec![("v".to_string(), LangType::I32)],
                },
            ],
        );
        (m, color, shape)
    }

    fn variant_pattern(variant: u32) -> SwitchPattern {
        SwitchPattern::SumVariant {
            variant,
            binders: Vec::new(),
        }
    }

    /// A sum matches by value and through exactly one pointer level (which
    /// auto-derefs); deeper pointers and arrays are not scrutinees at all.
    #[test]
    fn sum_scrutinee_accepts_one_pointer_level_only() {
        let sum = LangType::sum_type(7);
        assert_eq!(sum_scrutinee(&sum), Some(7));
        assert_eq!(sum_scrutinee(&sum.with_pointer_depth(1)), Some(7));
        assert_eq!(sum_scrutinee(&sum.with_pointer_depth(2)), None);
        assert_eq!(sum_scrutinee(&sum.with_array_size(3)), None);
        assert_eq!(sum_scrutinee(&LangType::I32), None);
    }

    #[test]
    fn classification_covers_every_scrutinee_shape() {
        let (m, color, shape) = symbols();
        let closed = |ty: &LangType| VariantSpace::of(ty, &m).closed();

        assert_eq!(VariantSpace::of(&LangType::I32, &m), VariantSpace::Open);
        assert_eq!(VariantSpace::of(&LangType::U64, &m), VariantSpace::Open);
        assert_eq!(closed(&LangType::BOOL).map(|s| s.count), Some(2));
        assert_eq!(closed(&LangType::enum_type(color)).map(|s| s.count), Some(2));
        assert_eq!(closed(&LangType::sum_type(shape)).map(|s| s.count), Some(2));
        // A pointer-to-sum is still the sum's space; everything else
        // pointer-shaped, plus floats, cannot be matched.
        assert_eq!(
            closed(&LangType::sum_type(shape).with_pointer_depth(1)).map(|s| s.count),
            Some(2)
        );
        for unmatchable in [
            LangType::F64,
            LangType::U8_PTR,
            LangType::struct_type(0),
            LangType::enum_type(color).with_pointer_depth(1),
        ] {
            assert_eq!(
                VariantSpace::of(&unmatchable, &m),
                VariantSpace::Unmatchable,
                "{unmatchable} should not be matchable"
            );
        }
    }

    /// Repeating a variant must not fake coverage — the dedup is what makes
    /// `complete` trustworthy for the checker's dead-`default` warning.
    #[test]
    fn coverage_is_dedup_aware() {
        let (m, _, shape) = symbols();
        let space = VariantSpace::of(&LangType::sum_type(shape), &m)
            .closed()
            .expect("sum is closed");

        assert!(space.covered_by([variant_pattern(0), variant_pattern(1)].iter()));
        assert!(!space.covered_by([variant_pattern(0), variant_pattern(0)].iter()));
        assert!(!space.covered_by([variant_pattern(1)].iter()));
    }

    /// The three diagnostic spellings the switch errors depend on.
    #[test]
    fn slot_labels_match_the_diagnostics() {
        let (m, color, shape) = symbols();
        let enum_space = VariantSpace::of(&LangType::enum_type(color), &m)
            .closed()
            .unwrap();
        let sum_space = VariantSpace::of(&LangType::sum_type(shape), &m)
            .closed()
            .unwrap();
        let bool_space = VariantSpace::of(&LangType::BOOL, &m).closed().unwrap();

        assert_eq!(enum_space.duplicate_label(0, &m), "variant 'Red'");
        assert_eq!(sum_space.duplicate_label(1, &m), "variant 'Mark'");
        assert_eq!(bool_space.duplicate_label(1, &m), "`true`");

        let seen: HashSet<usize> = [0].into_iter().collect();
        assert_eq!(enum_space.missing_label(&seen, &m), "variants 'Green'");
        assert_eq!(bool_space.missing_label(&seen, &m), "`true`");
    }
}
