//! Qualified variant-pattern parsing, shared by `switch` arms and `is`.
//!
//! `case Shape.Circle(r)` and `s is Shape.Circle(r)` accept the same grammar and
//! enforce the same rules — qualification, positional binders, `_` discards. The
//! two differ only in what they build from the result and in how they phrase the
//! `_`-is-not-a-variant rejection, which is what [`PatternSite`] carries.

use crate::lexer::{LangType, TokenKind};
use crate::parser::expressions::Parser;
use crate::parser::ParserError;
use crate::symbol::ids::SumId;

/// The wording a pattern site needs; everything else about parsing one is
/// identical between `switch` and `is`.
pub(crate) struct PatternSite {
    /// Completes "sum 'S' has no variant '_' — …".
    pub underscore_hint: &'static str,
    /// What the head identifier is called when it is missing.
    pub head_prompt: &'static str,
}

pub(crate) const SWITCH_ARM: PatternSite = PatternSite {
    underscore_hint: "use `default` to cover the remaining variants",
    head_prompt: "variant pattern",
};

pub(crate) const IS_PROBE: PatternSite = PatternSite {
    underscore_hint: "`is` probes one named variant",
    head_prompt: "variant pattern after `is`",
};

pub(crate) struct VariantPattern {
    pub variant: u32,
    /// One entry per payload field in declaration order; `None` discards.
    pub binders: Vec<Option<(String, LangType)>>,
    /// The pattern wrote no parens at all (`Sum.Variant`), which `is` needs in
    /// order to build a non-binding probe rather than an all-discard one.
    pub bare: bool,
}

impl Parser {
    /// `Sum.Variant` or `Sum.Variant(a, _, b)`, resolved against `sum_id`.
    /// Qualification is mandatory — a pattern spells its type like every other
    /// variant access — and a binder list must name every payload field.
    pub(crate) fn parse_variant_pattern(
        &mut self,
        sum_id: SumId,
        site: &PatternSite,
    ) -> Result<VariantPattern, ParserError> {
        let pos = self.peek().pos;
        let sum_name = self.module.type_def(sum_id).name.clone();

        let head = self.parse_ident(site.head_prompt)?;
        if head == "_" {
            return Err(ParserError::UnexpectedToken(
                format!(
                    "sum '{sum_name}' has no variant '_' — {}",
                    site.underscore_hint
                ),
                pos,
            ));
        }
        if head != sum_name {
            // A bare variant name gets the fix spelled out; anything else is an
            // unknown variant of this sum.
            if self.module.sum_variant_index(sum_id, &head).is_some() {
                return Err(ParserError::UnexpectedToken(
                    format!("variant patterns are qualified — write `{sum_name}.{head}`"),
                    pos,
                ));
            }
            return Err(ParserError::UnknownSumVariant {
                sum_name,
                variant: head,
                pos,
            });
        }

        self.expect(&TokenKind::Dot, ".")?;
        let variant_name = self.parse_ident("variant name")?;
        let Some(idx) = self.module.sum_variant_index(sum_id, &variant_name) else {
            return Err(ParserError::UnknownSumVariant {
                sum_name,
                variant: variant_name,
                pos,
            });
        };
        let field_types: Vec<LangType> = self.module[sum_id].variants[idx]
            .fields
            .iter()
            .map(|(_, ty)| *ty)
            .collect();
        let variant = u32::try_from(idx).expect("variant index fits u32");

        if !self.match_token(&[TokenKind::OpenParen]) {
            return Ok(VariantPattern {
                variant,
                binders: vec![None; field_types.len()],
                bare: true,
            });
        }
        if self.check(&TokenKind::CloseParen) {
            return Err(ParserError::UnexpectedToken(
                format!(
                    "empty pattern parens on '{variant_name}' — a bare `{variant_name}` ignores the payload"
                ),
                self.peek().pos,
            ));
        }
        let names = self.parse_comma_separated(&TokenKind::CloseParen, |p| {
            p.parse_ident("binding name or `_`")
        })?;
        if names.len() != field_types.len() {
            return Err(ParserError::UnexpectedToken(
                format!(
                    "pattern '{variant_name}' binds {} of {} payload fields — bind every field positionally (use `_` to discard)",
                    names.len(),
                    field_types.len()
                ),
                pos,
            ));
        }
        Ok(VariantPattern {
            variant,
            binders: names
                .into_iter()
                .zip(field_types)
                .map(|(n, ty)| if n == "_" { None } else { Some((n, ty)) })
                .collect(),
            bare: false,
        })
    }
}
