use crate::lexer::{Keyword, LangType, Position, TokenKind, TypeBase};
use crate::parser::expressions::Parser;
use crate::parser::{ExprKind, Expression, ParserError, Statement, StatementKind};
use aspect_macros::parse_rule;

type StatementPred = fn(&Parser) -> bool;
type StatementHandler = fn(&mut Parser) -> Result<Statement, ParserError>;

const STATEMENT_TABLE: &[(StatementPred, StatementHandler)] = &[
    (
        |p| p.check(&TokenKind::OpenBrace),
        Parser::parse_block_statement,
    ),
    (
        |p| p.check_keyword(&Keyword::Return),
        Parser::parse_return_statement,
    ),
    (
        |p| p.check_keyword(&Keyword::If),
        Parser::parse_if_statement,
    ),
    (
        |p| p.check_keyword(&Keyword::While),
        Parser::parse_while_statement,
    ),
    (
        |p| p.check_keyword(&Keyword::For),
        Parser::parse_for_statement,
    ),
    (
        |p| p.check_keyword(&Keyword::Switch),
        Parser::parse_switch_statement,
    ),
    // `case`/`default` in plain statement position: a dedicated error beats
    // the generic unexpected-token one.
    (
        |p| p.check_keyword(&Keyword::Case) || p.check_keyword(&Keyword::Default),
        Parser::case_outside_switch,
    ),
    (
        |p| p.check_keyword(&Keyword::Break),
        Parser::parse_break_statement,
    ),
    (
        |p| p.check_keyword(&Keyword::Continue),
        Parser::parse_continue_statement,
    ),
    (
        |p| matches!(p.peek().kind, TokenKind::LangType(_)),
        Parser::parse_var_decl_or_assignment,
    ),
    // A bare `const` at statement start is a const local over a named/fn-ptr/
    // grouped type; the scanner already fused `const` with builtin scalars above.
    (
        |p| p.check_keyword(&Keyword::Const),
        Parser::parse_var_decl_or_assignment,
    ),
    // Named-type local declarations: `myint x`, `Point* p`, ...
    (
        Parser::starts_named_var_decl,
        Parser::parse_var_decl_or_assignment,
    ),
    // Function-pointer local declarations: `fn(i32) -> i32 op = &double`.
    (
        Parser::starts_fnptr_var_decl,
        Parser::parse_var_decl_or_assignment,
    ),
    // Parenthesised-type local declarations: `(fn(i32) -> i32)[3] table = ...`.
    (
        Parser::starts_grouped_var_decl,
        Parser::parse_var_decl_or_assignment,
    ),
];

impl Parser {
    pub(crate) fn parse_statement(&mut self) -> Result<Statement, ParserError> {
        self.skip_newlines();
        self.dispatch_statement()
    }

    fn dispatch_statement(&mut self) -> Result<Statement, ParserError> {
        for &(pred, handler) in STATEMENT_TABLE {
            if pred(self) {
                return handler(self);
            }
        }
        self.parse_expression_or_assign_statement()
    }

    #[parse_rule]
    pub(crate) fn parse_block_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = pos!();
        token!(OpenBrace);
        let statements = scoped!({
            let mut stmts = Vec::new();
            loop {
                skip_nl!();
                if self.check(&TokenKind::CloseBrace) || self.is_at_end() {
                    break;
                }
                // `synchronize` stops *before* a statement-starting keyword so
                // the next iteration parses it — but if that keyword is what
                // failed (e.g. a top-level-only `fn` in statement position), the
                // cursor never moves and the same token fails forever until OOM.
                // Consuming the stuck token bounds the loop at one error/token.
                let before = self.current;
                if let Some(s) = sync!(parse_statement) {
                    stmts.push(s);
                } else if self.current == before {
                    self.advance();
                }
            }
            stmts
        });
        token!(CloseBrace);
        Ok(Statement::new(StatementKind::Block(statements), pos))
    }

    #[parse_rule]
    fn parse_return_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = pos!();
        kw!(Return);
        let value = opt_unless_term!(parse_expression);
        term!();
        Ok(Statement::new(StatementKind::Return(value), pos))
    }

    #[parse_rule]
    fn parse_if_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = pos!();
        kw!(If);
        // One parse-time scope spans condition + then-block: `is` bindings
        // registered while the condition parses are visible in the block and
        // die with it (the else never sees them).
        let (condition, then_block) = scoped!({
            let condition = self.parse_expression()?;
            skip_nl!();
            let then_block = block_body!(parse_block_statement);
            (condition, then_block)
        });
        skip_nl!();
        let else_block = if kw_if!(Else) {
            skip_nl!();
            Some(block_body!(parse_block_statement))
        } else if kw_if!(Elif) {
            // 'elif' already consumed — parse the rest as a nested if.
            Some(vec![self.parse_elif_body()?])
        } else {
            None
        };
        Ok(Statement::new(
            StatementKind::If {
                condition,
                then_block,
                else_block,
            },
            pos,
        ))
    }

    /// Parse the condition + blocks of an elif chain (the 'elif' keyword has
    /// already been consumed by the caller).  Handles arbitrary elif depth.
    fn parse_elif_body(&mut self) -> Result<Statement, ParserError> {
        let pos = self.peek().pos;
        // Manual scope (this fn is not a #[parse_rule], so no scoped! DSL):
        // same condition+block scoping as parse_if_statement.
        self.symbol_table_mut().enter_scope();
        let cond_and_block = (|| -> Result<_, ParserError> {
            let condition = self.parse_expression()?;
            self.skip_newlines();
            let then_block = match self.parse_block_statement()? {
                Statement {
                    kind: StatementKind::Block(stmts),
                    ..
                } => stmts,
                _ => unreachable!(),
            };
            Ok((condition, then_block))
        })();
        self.symbol_table_mut().exit_scope();
        let (condition, then_block) = cond_and_block?;
        self.skip_newlines();
        let else_block = if self.check_keyword(&Keyword::Else) {
            self.advance();
            self.skip_newlines();
            let blk = match self.parse_block_statement()? {
                Statement {
                    kind: StatementKind::Block(stmts),
                    ..
                } => stmts,
                _ => unreachable!(),
            };
            Some(blk)
        } else if self.check_keyword(&Keyword::Elif) {
            self.advance();
            Some(vec![self.parse_elif_body()?])
        } else {
            None
        };
        Ok(Statement::new(
            StatementKind::If {
                condition,
                then_block,
                else_block,
            },
            pos,
        ))
    }

    #[parse_rule]
    fn parse_while_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = pos!();
        kw!(While);
        // Condition + body share one scope, like `if` — the `while … is
        // Cons(h, t)` walk re-binds h/t each iteration into this scope.
        let (condition, body) = scoped!({
            let condition = self.parse_expression()?;
            skip_nl!();
            let body = block_body!(parse_block_statement);
            (condition, body)
        });
        Ok(Statement::new(
            StatementKind::While { condition, body },
            pos,
        ))
    }

    #[parse_rule]
    fn parse_break_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = pos!();
        kw!(Break);
        term!();
        Ok(Statement::new(StatementKind::Break, pos))
    }

    #[parse_rule]
    fn parse_continue_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = pos!();
        kw!(Continue);
        term!();
        Ok(Statement::new(StatementKind::Continue, pos))
    }

    #[parse_rule]
    fn parse_for_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = pos!();
        kw!(For);
        let (init, condition, increment, body) = scoped!({
            let init = if self.check(&TokenKind::Semicolon) {
                None
            } else if matches!(self.peek().kind, TokenKind::LangType(_)) {
                Some(Box::new(self.parse_var_decl_inner()?))
            } else {
                Some(Box::new(self.parse_expression_or_assign_inner()?))
            };
            token!(Semicolon);
            let condition = if self.check(&TokenKind::Semicolon) {
                None
            } else {
                Some(self.parse_expression()?)
            };
            token!(Semicolon);
            let increment = if self.check(&TokenKind::OpenBrace) {
                None
            } else {
                Some(Box::new(self.parse_expression_or_assign_inner()?))
            };
            skip_nl!();
            let body = block_body!(parse_block_statement);
            (init, condition, increment, body)
        });
        Ok(Statement::new(
            StatementKind::For {
                init,
                condition,
                increment,
                body,
            },
            pos,
        ))
    }

    /// No trailing terminator — the caller (or for-loop init) adds it.
    #[parse_rule]
    fn parse_var_decl_inner(&mut self) -> Result<Statement, ParserError> {
        let pos = pos!();
        let var_type = lang_type!();
        let name = ident!();
        let initializer = if self.match_token(&[TokenKind::Assign]) {
            Some(self.parse_expression()?)
        } else {
            None
        };
        self.symbol_table_mut()
            .add_variable(name.clone(), var_type, pos)
            .map_err(|e| ParserError::from_symbol(e, pos))?;
        Ok(Statement::new(
            StatementKind::VarDecl {
                var_type,
                name,
                initializer,
            },
            pos,
        ))
    }

    fn parse_var_decl_or_assignment(&mut self) -> Result<Statement, ParserError> {
        let stmt = self.parse_var_decl_inner()?;
        self.match_token(&[TokenKind::Semicolon, TokenKind::Newline]);
        Ok(stmt)
    }

    /// Map a compound-assignment token to its underlying binary operator.
    fn compound_op_for_token(kind: &TokenKind) -> Option<crate::parser::BinaryOp> {
        use crate::parser::BinaryOp;
        match kind {
            TokenKind::PlusAssign => Some(BinaryOp::Add),
            TokenKind::MinusAssign => Some(BinaryOp::Sub),
            TokenKind::MultAssign => Some(BinaryOp::Mul),
            TokenKind::DivAssign => Some(BinaryOp::Div),
            TokenKind::ModAssign => Some(BinaryOp::Mod),
            TokenKind::AndAssign => Some(BinaryOp::And),
            TokenKind::OrAssign => Some(BinaryOp::Or),
            TokenKind::XorAssign => Some(BinaryOp::Xor),
            TokenKind::LeftShiftAssign => Some(BinaryOp::LeftShift),
            TokenKind::RightShiftAssign => Some(BinaryOp::RightShift),
            _ => None,
        }
    }

    /// Desugars `x += 5` to `x = x + 5`.
    fn create_compound_assignment(
        name: &str,
        var_type: crate::lexer::LangType,
        value_expr: Expression,
        op: crate::parser::BinaryOp,
        pos: Position,
    ) -> Expression {
        let var_expr = Expression::new(ExprKind::Variable(name.to_string()), var_type, pos);
        Expression::new(
            ExprKind::Binary {
                left: Box::new(var_expr),
                op,
                right: Box::new(value_expr),
            },
            var_type,
            pos,
        )
    }

    /// No trailing terminator — the caller (or for-loop) adds it.
    fn parse_expression_or_assign_inner(&mut self) -> Result<Statement, ParserError> {
        let pos = self.peek().pos;
        let expr = self.parse_expression()?;

        if self.check(&TokenKind::Assign) {
            self.advance();
            let value = self.parse_expression()?;
            if let ExprKind::Variable(name) = expr.kind {
                Ok(Statement::new(
                    StatementKind::VarAssign { name, value },
                    pos,
                ))
            } else if matches!(expr.kind, ExprKind::Dereference(_)) {
                Ok(Statement::new(
                    StatementKind::DerefAssign {
                        target: expr,
                        value,
                    },
                    pos,
                ))
            } else if matches!(expr.kind, ExprKind::FieldAccess { .. }) {
                Ok(Statement::new(
                    StatementKind::FieldAssign {
                        target: expr,
                        value,
                    },
                    pos,
                ))
            } else {
                Err(ParserError::UnexpectedToken(
                    "cannot assign to this expression".to_string(),
                    pos,
                ))
            }
        } else {
            let compound_op = Self::compound_op_for_token(&self.peek().kind.clone());
            if let Some(op) = compound_op {
                if let ExprKind::Variable(ref name) = expr.kind {
                    let name = name.clone();
                    let var_type = expr.expr_type;
                    self.advance();
                    let value_expr = self.parse_expression()?;
                    let value =
                        Self::create_compound_assignment(&name, var_type, value_expr, op, pos);
                    Ok(Statement::new(
                        StatementKind::VarAssign { name, value },
                        pos,
                    ))
                } else {
                    Err(ParserError::UnexpectedToken(
                        "compound assignment requires a variable".to_string(),
                        pos,
                    ))
                }
            } else {
                Ok(Statement::new(StatementKind::Expression(expr), pos))
            }
        }
    }

    fn parse_expression_or_assign_statement(&mut self) -> Result<Statement, ParserError> {
        let stmt = self.parse_expression_or_assign_inner()?;
        self.match_token(&[TokenKind::Semicolon, TokenKind::Newline]);
        Ok(stmt)
    }


    /// `switch scrutinee { case … { } … default { } }`. The scrutinee's
    /// parse-time type drives pattern resolution (sum variants resolve
    /// unqualified against it, exactly like enum access); coverage
    /// completeness is computed here, dedup-aware, so the checker's
    /// termination analysis can read it without the registry.
    #[parse_rule]
    fn parse_switch_statement(&mut self) -> Result<Statement, ParserError> {
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
    /// scrutinees take (un)qualified variant patterns with positional binders
    /// and `_` discards; anything else takes a constant expression the
    /// checker validates (bare enum variant names resolve here, since the
    /// scrutinee pins the enum).
    fn parse_switch_pattern(
        &mut self,
        scrutinee: &Expression,
    ) -> Result<crate::parser::SwitchPattern, ParserError> {
        use crate::parser::SwitchPattern;

        let pos = self.peek().pos;
        let s_ty = scrutinee.expr_type;
        if s_ty.pointer_depth == 0
            && !s_ty.is_array()
            && let TypeBase::Sum(sum_id) = s_ty.base
        {
            let sum_name = self.module.sum_info(sum_id).name.clone();
            let mut variant_name = self.parse_ident("variant pattern")?;
            if variant_name == "_" {
                return Err(ParserError::UnexpectedToken(
                    format!(
                        "sum '{sum_name}' has no variant '_' — use `default` to cover the remaining variants"
                    ),
                    pos,
                ));
            }
            // Qualified form `Sum.Variant` — accepted, resolves identically.
            if variant_name == sum_name && self.match_token(&[TokenKind::Dot]) {
                variant_name = self.parse_ident("variant name")?;
            }
            let Some(idx) = self.module.sum_variant_index(sum_id, &variant_name) else {
                return Err(ParserError::UnknownSumVariant {
                    sum_name,
                    variant: variant_name,
                    pos,
                });
            };
            let field_types: Vec<LangType> = self.module.sum_info(sum_id).variants[idx]
                .fields
                .iter()
                .map(|(_, ty)| *ty)
                .collect();

            let binders: Vec<Option<(String, LangType)>> =
                if self.match_token(&[TokenKind::OpenParen]) {
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
                    names
                        .into_iter()
                        .zip(field_types)
                        .map(|(n, ty)| if n == "_" { None } else { Some((n, ty)) })
                        .collect()
                } else {
                    // Bare variant: match, ignore any payload.
                    vec![None; field_types.len()]
                };
            return Ok(SwitchPattern::SumVariant {
                variant: u32::try_from(idx).expect("variant index fits u32"),
                binders,
            });
        }

        // Bare enum variant names resolve against the scrutinee's enum — the
        // one context where a variant needs no `Enum.` prefix. The qualified
        // form falls through to `parse_expression`, which builds the same
        // `EnumValue`.
        if s_ty.pointer_depth == 0
            && let TypeBase::Enum(enum_id) = s_ty.base
            && let TokenKind::Identifier(name) = &self.peek().kind
        {
            let name = name.clone();
            // Any *enum name* (this one or another's, as in `case B.P`) falls
            // through to `parse_expression`, which builds the qualified
            // `EnumValue` — the checker then judges cross-enum mismatches.
            if self.module.enum_id(&name).is_none()
                && self.symbol_table.lookup_variable(&name).is_none()
            {
                if let Some(vidx) = self.module.enum_variant_index(enum_id, &name) {
                    self.advance();
                    return Ok(SwitchPattern::Const(Expression::new(
                        ExprKind::EnumValue {
                            enum_id,
                            value: vidx as i64,
                        },
                        LangType::enum_type(enum_id),
                        pos,
                    )));
                }
                return Err(ParserError::UnknownVariant {
                    enum_name: self.module.enum_info(enum_id).name.clone(),
                    variant: name,
                    pos,
                });
            }
        }

        Ok(SwitchPattern::Const(self.parse_expression()?))
    }

    /// Dedup-aware coverage: do the arms alone cover the scrutinee?
    fn switch_coverage_complete(
        &self,
        scrutinee: &Expression,
        arms: &[crate::parser::SwitchArm],
    ) -> bool {
        use crate::parser::SwitchPattern;
        use std::collections::HashSet;

        let s_ty = scrutinee.expr_type;
        if s_ty.pointer_depth > 0 || s_ty.is_array() {
            return false;
        }
        let patterns = arms.iter().flat_map(|a| a.patterns.iter());
        match s_ty.base {
            TypeBase::Sum(id) => {
                let seen: HashSet<u32> = patterns
                    .filter_map(|p| match p {
                        SwitchPattern::SumVariant { variant, .. } => Some(*variant),
                        SwitchPattern::Const(_) => None,
                    })
                    .collect();
                seen.len() == self.module.sum_info(id).variants.len()
            }
            TypeBase::Enum(id) => {
                let seen: HashSet<i64> = patterns
                    .filter_map(|p| match p {
                        SwitchPattern::Const(Expression {
                            kind: ExprKind::EnumValue { value, .. },
                            ..
                        }) => Some(*value),
                        _ => None,
                    })
                    .collect();
                seen.len() == self.module.enum_info(id).variants.len()
            }
            TypeBase::Bool => {
                let seen: HashSet<bool> = patterns
                    .filter_map(|p| match p {
                        SwitchPattern::Const(Expression {
                            kind: ExprKind::Literal(crate::parser::LiteralValue::Bool(b)),
                            ..
                        }) => Some(*b),
                        _ => None,
                    })
                    .collect();
                seen.len() == 2
            }
            _ => false,
        }
    }

    fn case_outside_switch(&mut self) -> Result<Statement, ParserError> {
        Err(ParserError::UnexpectedToken(
            "`case` and `default` only appear inside a `switch` statement".to_string(),
            self.peek().pos,
        ))
    }
}
