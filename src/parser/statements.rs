use crate::lexer::{Keyword, Position, TokenKind};
use crate::parser::expressions::Parser;
use crate::parser::{ExprKind, Expression, ParserError, Statement, StatementKind};
use tjlb_macros::parse_rule;

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
    // Named-type local declarations: `myint x`, `Point* p`, ...
    (
        Parser::starts_named_var_decl,
        Parser::parse_var_decl_or_assignment,
    ),
];

impl Parser {
    /// Parse a statement
    pub(crate) fn parse_statement(&mut self) -> Result<Statement, ParserError> {
        self.skip_newlines();
        for &(pred, handler) in STATEMENT_TABLE {
            if pred(self) {
                return handler(self);
            }
        }
        self.parse_expression_or_assign_statement()
    }

    /// Parse a block statement { ... }
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
                if let Some(s) = sync!(parse_statement) {
                    stmts.push(s);
                }
            }
            stmts
        });
        token!(CloseBrace);
        Ok(Statement::new(StatementKind::Block(statements), pos))
    }

    /// Parse a return statement
    #[parse_rule]
    fn parse_return_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = pos!();
        kw!(Return);
        let value = opt_unless_term!(parse_expression);
        term!();
        Ok(Statement::new(StatementKind::Return(value), pos))
    }

    /// Parse an if statement
    #[parse_rule]
    fn parse_if_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = pos!();
        kw!(If);
        let condition = self.parse_expression()?;
        skip_nl!();
        let then_block = block_body!(parse_block_statement);
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
        let condition = self.parse_expression()?;
        self.skip_newlines();
        let then_block = match self.parse_block_statement()? {
            Statement {
                kind: StatementKind::Block(stmts),
                ..
            } => stmts,
            _ => unreachable!(),
        };
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
            self.advance(); // consume 'elif'
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

    /// Parse a while loop
    #[parse_rule]
    fn parse_while_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = pos!();
        kw!(While);
        let condition = self.parse_expression()?;
        skip_nl!();
        let body = block_body!(parse_block_statement);
        Ok(Statement::new(
            StatementKind::While { condition, body },
            pos,
        ))
    }

    /// Parse a break statement
    #[parse_rule]
    fn parse_break_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = pos!();
        kw!(Break);
        term!();
        Ok(Statement::new(StatementKind::Break, pos))
    }

    /// Parse a continue statement
    #[parse_rule]
    fn parse_continue_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = pos!();
        kw!(Continue);
        term!();
        Ok(Statement::new(StatementKind::Continue, pos))
    }

    /// Parse a for loop
    #[parse_rule]
    fn parse_for_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = pos!();
        kw!(For);
        token!(OpenParen);
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
            let increment = if self.check(&TokenKind::CloseParen) {
                None
            } else {
                Some(Box::new(self.parse_expression_or_assign_inner()?))
            };
            token!(CloseParen);
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

    /// Variable declaration inner (no trailing terminator).
    /// Called by `parse_var_decl_or_assignment` (adds term) and the for-loop init.
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

    /// Parse variable declaration (with trailing terminator)
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

    /// Create a compound assignment expression (e.g., x += 5 becomes x = x + 5)
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

    /// Parse expression or assignment without trailing terminator.
    /// Called by `parse_expression_or_assign_statement` (adds term) and the for-loop.
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

    /// Parse expression or assignment statement (with trailing terminator)
    fn parse_expression_or_assign_statement(&mut self) -> Result<Statement, ParserError> {
        let stmt = self.parse_expression_or_assign_inner()?;
        self.match_token(&[TokenKind::Semicolon, TokenKind::Newline]);
        Ok(stmt)
    }
}
