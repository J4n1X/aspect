use crate::lexer::{Keyword, LangType, TokenKind, TypeBase};
use crate::parser::expressions::Parser;
use crate::parser::patterns::SWITCH_ARM;
use crate::parser::{Expression, ParserError, Statement, StatementKind};
use crate::variants::VariantSpace;
use aspect_macros::parse_rule;
use crate::symbol::ids::EnumId;

impl Parser {
    /// `switch scrutinee { case … { } … default { } }`. The scrutinee's
    /// parse-time type drives pattern resolution (sum variants resolve
    /// unqualified against it, exactly like enum access); coverage
    /// completeness is computed here, dedup-aware, so the checker's
    /// termination analysis can read it without the registry.
    #[parse_rule]
    pub(crate) fn parse_switch_statement(&mut self) -> Result<Statement, ParserError> {
        use crate::parser::SwitchArm;

        let pos = pos!();
        kw!(Switch);
        let scrutinee = self.parse_expression()?;
        skip_nl!();
        token!(OpenBrace);

        let mut arms: Vec<SwitchArm> = Vec::new();
        let mut default: Option<Vec<Statement>> = None;
        loop {
            skip_nl!();
            if self.check(&TokenKind::CloseBrace) || self.is_at_end() {
                break;
            }
            // `default` covers "the rest" — reading order matches checking
            // order only if nothing follows it.
            if default.is_some() {
                return Err(ParserError::UnexpectedToken(
                    "`default` must be the last arm of a switch".to_string(),
                    self.peek().pos,
                ));
            }
            if kw_if!(Default) {
                skip_nl!();
                default = Some(block_body!(parse_block_statement));
                continue;
            }
            if !self.check_keyword(&Keyword::Case) {
                return Err(ParserError::ExpectedToken(
                    "`case`, `default` or `}` in switch body".to_string(),
                    format!("{}", self.peek().kind),
                    self.peek().pos,
                ));
            }
            let arm = self.parse_switch_arm(&scrutinee)?;
            arms.push(arm);
        }
        token!(CloseBrace);

        let complete = self.switch_coverage_complete(&scrutinee, &arms);
        Ok(Statement::new(
            StatementKind::Switch {
                scrutinee,
                arms,
                default,
                complete,
            },
            pos,
        ))
    }

    /// One `case` arm: pattern list, then the body with any bindings in
    /// scope. A pattern that binds must be the arm's only pattern (no binding
    /// unification across alternatives).
    #[parse_rule]
    fn parse_switch_arm(
        &mut self,
        scrutinee: &Expression,
    ) -> Result<crate::parser::SwitchArm, ParserError> {
        use crate::parser::{SwitchArm, SwitchPattern};

        let pos = pos!();
        kw!(Case);
        let mut patterns: Vec<SwitchPattern> = Vec::new();
        loop {
            patterns.push(self.parse_switch_pattern(scrutinee)?);
            if !self.match_token(&[TokenKind::Comma]) {
                break;
            }
        }
        let binders: Vec<(String, LangType)> = patterns
            .iter()
            .flat_map(|p| match p {
                SwitchPattern::SumVariant { binders, .. } => {
                    binders.iter().flatten().cloned().collect::<Vec<_>>()
                }
                SwitchPattern::Const(_) => Vec::new(),
            })
            .collect();
        if !binders.is_empty() && patterns.len() > 1 {
            return Err(ParserError::UnexpectedToken(
                "a pattern that binds payload fields must be the arm's only pattern".to_string(),
                pos,
            ));
        }
        skip_nl!();
        let body = scoped!({
            for (name, ty) in &binders {
                self.symbol_table_mut()
                    .add_variable(name.clone(), *ty, pos)
                    .map_err(|e| ParserError::from_symbol(e, pos))?;
            }
            match self.parse_block_statement()? {
                Statement {
                    kind: StatementKind::Block(stmts),
                    ..
                } => stmts,
                _ => unreachable!(),
            }
        });
        Ok(SwitchArm {
            patterns,
            body,
            pos,
        })
    }

    /// One pattern, resolved against the scrutinee's parse-time type: sum
    /// scrutinees (by value, or through one pointer level — matching
    /// auto-derefs like field access) take qualified `Sum.Variant` patterns
    /// with positional binders and `_` discards; anything else takes a
    /// constant expression the checker validates. Qualification is mandatory
    /// — patterns spell the type like every other variant access.
    fn parse_switch_pattern(
        &mut self,
        scrutinee: &Expression,
    ) -> Result<crate::parser::SwitchPattern, ParserError> {
        use crate::parser::SwitchPattern;

        if let Some(sum_id) = crate::variants::sum_scrutinee(&scrutinee.expr_type) {
            let pattern = self.parse_variant_pattern(sum_id, &SWITCH_ARM)?;
            return Ok(SwitchPattern::SumVariant {
                variant: pattern.variant,
                binders: pattern.binders,
            });
        }

        // A bare enum variant name gets a qualify hint rather than the
        // undefined-variable error `parse_expression` would produce; the
        // qualified form falls through and builds the `EnumValue`.
        let s_ty = scrutinee.expr_type;
        if s_ty.pointer_depth == 0
            && let TypeBase::Enum(enum_id) = s_ty.base
            && let Some(pattern) = self.parse_enum_variant_pattern(enum_id)?
        {
            return Ok(pattern);
        }

        Ok(SwitchPattern::Const(self.parse_expression()?))
    }

    /// The enum-scrutinee branch of `parse_switch_pattern`: patterns are
    /// qualified (`Color.Red`), so a bare identifier naming one of
    /// `enum_id`'s variants errors with the fix spelled out, and a bare
    /// unknown identifier errors as an unknown variant. `None` means the
    /// next token is no bare-identifier hazard (an enum name, a shadowed
    /// local, or not an identifier) — the caller falls through to
    /// `parse_expression`, which builds the qualified `EnumValue`.
    fn parse_enum_variant_pattern(
        &mut self,
        enum_id: EnumId,
    ) -> Result<Option<crate::parser::SwitchPattern>, ParserError> {
        let pos = self.peek().pos;
        let TokenKind::Identifier(name) = &self.peek().kind else {
            return Ok(None);
        };
        let name = name.clone();
        if self.module.enum_id(&name).is_some()
            || self.symbol_table.lookup_variable(&name).is_some()
        {
            return Ok(None);
        }
        let enum_name = self.module.type_def(enum_id).name.clone();
        if self.module.enum_variant_index(enum_id, &name).is_some() {
            return Err(ParserError::UnexpectedToken(
                format!("variant patterns are qualified — write `{enum_name}.{name}`"),
                pos,
            ));
        }
        Err(ParserError::UnknownVariant {
            enum_name,
            variant: name,
            pos,
        })
    }

    /// Do the arms alone cover the scrutinee? Only a closed space can be
    /// covered without a `default`.
    fn switch_coverage_complete(
        &self,
        scrutinee: &Expression,
        arms: &[crate::parser::SwitchArm],
    ) -> bool {
        VariantSpace::of(&scrutinee.expr_type, &self.module)
            .closed()
            .is_some_and(|space| space.covered_by(arms.iter().flat_map(|a| a.patterns.iter())))
    }

    pub(crate) fn case_outside_switch(&mut self) -> Result<Statement, ParserError> {
        Err(ParserError::UnexpectedToken(
            "`case` and `default` only appear inside a `switch` statement".to_string(),
            self.peek().pos,
        ))
    }
}
