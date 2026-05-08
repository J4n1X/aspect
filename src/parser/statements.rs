use crate::lexer::{TokenKind, Keyword, Position};
use crate::parser::{Statement, StatementKind, Expression, ParserError};
use crate::parser::expressions::Parser;

impl Parser {
    /// Parse a statement
    pub(crate) fn parse_statement(&mut self) -> Result<Statement, ParserError> {
        self.skip_newlines();

        match &self.peek().kind {
            // Block statement
            TokenKind::OpenBrace => self.parse_block_statement(),

            // Return statement
            TokenKind::Keyword(Keyword::Return) => self.parse_return_statement(),

            // If statement
            TokenKind::Keyword(Keyword::If) => self.parse_if_statement(),

            // While loop
            TokenKind::Keyword(Keyword::While) => self.parse_while_statement(),

            // For loop
            TokenKind::Keyword(Keyword::For) => self.parse_for_statement(),

            // Break statement
            TokenKind::Keyword(Keyword::Break) => self.parse_break_statement(),

            // Continue statement
            TokenKind::Keyword(Keyword::Continue) => self.parse_continue_statement(),

            // Variable declaration (starts with type)
            TokenKind::LangType(_) => self.parse_var_decl_or_assignment(),

            // Expression statement or assignment
            TokenKind::Identifier(_) => {
                // Look ahead to see if this is a simple assignment (identifier = value)
                if let Some(next) = self.peek_ahead(1) {
                    if matches!(next.kind,
                        TokenKind::Assign | TokenKind::PlusAssign | TokenKind::MinusAssign |
                        TokenKind::MultAssign | TokenKind::DivAssign | TokenKind::ModAssign |
                        TokenKind::AndAssign | TokenKind::OrAssign | TokenKind::XorAssign |
                        TokenKind::LeftShiftAssign | TokenKind::RightShiftAssign
                    ) {
                        return self.parse_assignment_statement();
                    }
                }
                // Could be array access assignment (arr[i] = value) or expression statement
                self.parse_expression_or_indexed_assignment()
            }

            // Dereference - parse it and check if it's an assignment
            TokenKind::Asterisk => {
                let pos = self.peek().pos;
                let expr = self.parse_unary()?;

                // Check if this is an assignment
                if self.check(&TokenKind::Assign) {
                    // Verify it's actually a dereference
                    if !matches!(expr.kind, crate::parser::ExprKind::Dereference(_)) {
                        return Err(ParserError::UnexpectedToken(
                            "Expected dereference expression for assignment".to_string(),
                            pos,
                        ));
                    }

                    self.advance(); // consume '='
                    let value = self.parse_expression()?;
                    self.match_token(&[TokenKind::Semicolon, TokenKind::Newline]);

                    Ok(Statement::new(
                        StatementKind::DerefAssign { target: expr, value },
                        pos,
                    ))
                } else {
                    // Just an expression statement
                    self.match_token(&[TokenKind::Semicolon, TokenKind::Newline]);
                    Ok(Statement::new(StatementKind::Expression(expr), pos))
                }
            }

            _ => self.parse_expression_statement(),
        }
    }

    /// Parse a block statement { ... }
    pub(crate) fn parse_block_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = self.peek().pos;
        self.expect(&TokenKind::OpenBrace, "{")?;

        self.symbol_table_mut().enter_scope();

        let mut statements = Vec::new();

        self.skip_newlines();

        while !self.check(&TokenKind::CloseBrace) && !self.is_at_end() {
            statements.push(self.parse_statement()?);
            self.skip_newlines();
        }

        self.expect(&TokenKind::CloseBrace, "}")?;

        self.symbol_table_mut().exit_scope();

        Ok(Statement::new(StatementKind::Block(statements), pos))
    }

    /// Parse a return statement
    fn parse_return_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = self.peek().pos;
        self.advance(); // consume 'return'

        let value = if self.check(&TokenKind::Newline) || self.check(&TokenKind::Semicolon) {
            None
        } else {
            Some(self.parse_expression()?)
        };

        // Consume optional semicolon or newline
        self.match_token(&[TokenKind::Semicolon, TokenKind::Newline]);

        Ok(Statement::new(StatementKind::Return(value), pos))
    }

    /// Parse an if statement
    fn parse_if_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = self.peek().pos;
        self.advance(); // consume 'if'

        let condition = self.parse_expression()?;

        self.skip_newlines();

        // Parse then block
        let then_block = if self.check(&TokenKind::OpenBrace) {
            match self.parse_block_statement()? {
                Statement { kind: StatementKind::Block(stmts), .. } => stmts,
                _ => unreachable!(),
            }
        } else {
            vec![self.parse_statement()?]
        };

        self.skip_newlines();

        // Parse optional else block
        let else_block = if self.check_keyword(&Keyword::Else) {
            self.advance(); // consume 'else'
            self.skip_newlines();

            if self.check(&TokenKind::OpenBrace) {
                match self.parse_block_statement()? {
                    Statement { kind: StatementKind::Block(stmts), .. } => Some(stmts),
                    _ => unreachable!(),
                }
            } else {
                Some(vec![self.parse_statement()?])
            }
        } else if self.check_keyword(&Keyword::Elif) {
            // Handle elif as else { if ... }
            Some(vec![self.parse_if_statement()?])
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
    fn parse_while_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = self.peek().pos;
        self.advance(); // consume 'while'

        let condition = self.parse_expression()?;

        self.skip_newlines();

        // Parse body
        let body = if self.check(&TokenKind::OpenBrace) {
            match self.parse_block_statement()? {
                Statement { kind: StatementKind::Block(stmts), .. } => stmts,
                _ => unreachable!(),
            }
        } else {
            vec![self.parse_statement()?]
        };

        Ok(Statement::new(
            StatementKind::While { condition, body },
            pos,
        ))
    }

    /// Parse a break statement
    fn parse_break_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = self.peek().pos;
        self.advance(); // consume 'break'
        self.match_token(&[TokenKind::Semicolon, TokenKind::Newline]);
        Ok(Statement::new(StatementKind::Break, pos))
    }

    /// Parse a continue statement
    fn parse_continue_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = self.peek().pos;
        self.advance(); // consume 'continue'
        self.match_token(&[TokenKind::Semicolon, TokenKind::Newline]);
        Ok(Statement::new(StatementKind::Continue, pos))
    }

    /// Parse a for loop
    fn parse_for_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = self.peek().pos;
        self.advance(); // consume 'for'

        self.expect(&TokenKind::OpenParen, "(")?;

        self.symbol_table_mut().enter_scope();

        // Parse init (can be declaration or expression)
        let init = if self.check(&TokenKind::Semicolon) {
            None
        } else if self.check(&TokenKind::LangType(crate::lexer::LangType::new(
            crate::lexer::TypeBase::Void,
            0,
            0,
            false,
        ))) || matches!(self.peek().kind, TokenKind::LangType(_))
        {
            Some(Box::new(self.parse_var_decl_for_loop()?))
        } else {
            Some(Box::new(self.parse_expression_statement_for_loop()?))
        };

        self.expect(&TokenKind::Semicolon, ";")?;

        // Parse condition
        let condition = if self.check(&TokenKind::Semicolon) {
            None
        } else {
            Some(self.parse_expression()?)
        };

        self.expect(&TokenKind::Semicolon, ";")?;

        // Parse increment (can be expression statement or assignment statement)
        let increment = if self.check(&TokenKind::CloseParen) {
            None
        } else {
            // Check if this is an assignment
            if let TokenKind::Identifier(_) = &self.peek().kind {
                if let Some(next_tok) = self.peek_ahead(1) {
                    if matches!(next_tok.kind,
                        TokenKind::Assign | TokenKind::PlusAssign | TokenKind::MinusAssign |
                        TokenKind::MultAssign | TokenKind::DivAssign | TokenKind::ModAssign |
                        TokenKind::AndAssign | TokenKind::OrAssign | TokenKind::XorAssign |
                        TokenKind::LeftShiftAssign | TokenKind::RightShiftAssign
                    ) {
                        // Parse as assignment statement
                        Some(Box::new(self.parse_assignment_statement_for_loop()?))
                    } else {
                        // Parse as expression statement
                        Some(Box::new(self.parse_expression_statement_for_loop()?))
                    }
                } else {
                    Some(Box::new(self.parse_expression_statement_for_loop()?))
                }
            } else {
                Some(Box::new(self.parse_expression_statement_for_loop()?))
            }
        };

        self.expect(&TokenKind::CloseParen, ")")?;

        self.skip_newlines();

        // Parse body
        let body = if self.check(&TokenKind::OpenBrace) {
            match self.parse_block_statement()? {
                Statement { kind: StatementKind::Block(stmts), .. } => stmts,
                _ => unreachable!(),
            }
        } else {
            vec![self.parse_statement()?]
        };

        self.symbol_table_mut().exit_scope();

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

    /// Parse variable declaration or assignment
    fn parse_var_decl_or_assignment(&mut self) -> Result<Statement, ParserError> {
        let pos = self.peek().pos;

        // Parse type
        let var_type = self.parse_type()?;

        // Expect identifier
        let name = match &self.peek().kind {
            TokenKind::Identifier(name) => {
                let name = name.clone();
                self.advance();
                name
            }
            _ => {
                return Err(ParserError::ExpectedToken(
                    "identifier".to_string(),
                    format!("{}", self.peek().kind),
                    self.peek().pos,
                ))
            }
        };

        // Parse optional initializer
        let initializer = if self.match_token(&[TokenKind::Assign]) {
            Some(self.parse_expression()?)
        } else {
            None
        };

        // Add variable to symbol table
        self.symbol_table_mut()
            .add_variable(name.clone(), var_type, pos)
            .map_err(|e| ParserError::UnexpectedToken(e, pos))?;

        // Consume optional semicolon or newline
        self.match_token(&[TokenKind::Semicolon, TokenKind::Newline]);

        Ok(Statement::new(
            StatementKind::VarDecl {
                var_type,
                name,
                initializer,
            },
            pos,
        ))
    }

    /// Parse assignment statement
    fn parse_assignment_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = self.peek().pos;

        // Parse variable name
        let name = match &self.peek().kind {
            TokenKind::Identifier(name) => {
                let name = name.clone();
                self.advance();
                name
            }
            _ => {
                return Err(ParserError::ExpectedToken(
                    "identifier".to_string(),
                    format!("{}", self.peek().kind),
                    self.peek().pos,
                ))
            }
        };

        // Check if variable exists
        let var_type = self.symbol_table()
            .lookup_variable(&name)
            .ok_or_else(|| ParserError::UndefinedVariable(name.clone(), pos))?
            .symbol_type;

        // Parse assignment operator
        let op_token = self.advance().clone();

        // Parse value expression
        let value_expr = self.parse_expression()?;

        // Convert compound assignments (+=, -=, etc.) to regular assignment
        let value = match op_token.kind {
            TokenKind::Assign => value_expr,
            TokenKind::PlusAssign => {
                Self::create_compound_assignment(&name, var_type, value_expr, crate::parser::BinaryOp::Add, pos)
            }
            TokenKind::MinusAssign => {
                Self::create_compound_assignment(&name, var_type, value_expr, crate::parser::BinaryOp::Sub, pos)
            }
            TokenKind::MultAssign => {
                Self::create_compound_assignment(&name, var_type, value_expr, crate::parser::BinaryOp::Mul, pos)
            }
            TokenKind::DivAssign => {
                Self::create_compound_assignment(&name, var_type, value_expr, crate::parser::BinaryOp::Div, pos)
            }
            TokenKind::ModAssign => {
                Self::create_compound_assignment(&name, var_type, value_expr, crate::parser::BinaryOp::Mod, pos)
            }
            TokenKind::AndAssign => {
                Self::create_compound_assignment(&name, var_type, value_expr, crate::parser::BinaryOp::And, pos)
            }
            TokenKind::OrAssign => {
                Self::create_compound_assignment(&name, var_type, value_expr, crate::parser::BinaryOp::Or, pos)
            }
            TokenKind::XorAssign => {
                Self::create_compound_assignment(&name, var_type, value_expr, crate::parser::BinaryOp::Xor, pos)
            }
            TokenKind::LeftShiftAssign => {
                Self::create_compound_assignment(&name, var_type, value_expr, crate::parser::BinaryOp::LeftShift, pos)
            }
            TokenKind::RightShiftAssign => {
                Self::create_compound_assignment(&name, var_type, value_expr, crate::parser::BinaryOp::RightShift, pos)
            }
            _ => {
                return Err(ParserError::UnexpectedToken(
                    format!("{}", op_token.kind),
                    pos,
                ))
            }
        };

        // Consume optional semicolon or newline
        self.match_token(&[TokenKind::Semicolon, TokenKind::Newline]);

        Ok(Statement::new(StatementKind::VarAssign { name, value }, pos))
    }

    /// Create a compound assignment expression (e.g., x += 5 becomes x = x + 5)
    fn create_compound_assignment(
        name: &str,
        var_type: crate::lexer::LangType,
        value_expr: Expression,
        op: crate::parser::BinaryOp,
        pos: Position,
    ) -> Expression {
        let var_expr = Expression::new(
            crate::parser::ExprKind::Variable(name.to_string()),
            var_type,
            pos,
        );

        Expression::new(
            crate::parser::ExprKind::Binary {
                left: Box::new(var_expr),
                op,
                right: Box::new(value_expr),
            },
            var_type,
            pos,
        )
    }

    /// Parse expression statement
    fn parse_expression_statement(&mut self) -> Result<Statement, ParserError> {
        let pos = self.peek().pos;
        let expr = self.parse_expression()?;

        // Consume optional semicolon or newline
        self.match_token(&[TokenKind::Semicolon, TokenKind::Newline]);

        Ok(Statement::new(StatementKind::Expression(expr), pos))
    }

    /// Parse an expression that might be followed by an assignment (for array access like arr[i] = value)
    fn parse_expression_or_indexed_assignment(&mut self) -> Result<Statement, ParserError> {
        let pos = self.peek().pos;
        let expr = self.parse_expression()?;

        // Check if this is followed by an assignment operator
        if self.check(&TokenKind::Assign) {
            // This must be an indexed/dereference assignment
            // The expression should be a dereference (array access becomes ptr arithmetic + deref)
            if !matches!(expr.kind, crate::parser::ExprKind::Dereference(_)) {
                return Err(ParserError::UnexpectedToken(
                    "Cannot assign to this expression - expected array access or dereference".to_string(),
                    pos,
                ));
            }

            self.advance(); // consume '='
            let value = self.parse_expression()?;
            self.match_token(&[TokenKind::Semicolon, TokenKind::Newline]);

            Ok(Statement::new(
                StatementKind::DerefAssign { target: expr, value },
                pos,
            ))
        } else {
            // Just an expression statement
            self.match_token(&[TokenKind::Semicolon, TokenKind::Newline]);
            Ok(Statement::new(StatementKind::Expression(expr), pos))
        }
    }

    /// Parse expression statement without consuming terminator (for use in for loops)
    fn parse_expression_statement_for_loop(&mut self) -> Result<Statement, ParserError> {
        let pos = self.peek().pos;
        let expr = self.parse_expression()?;

        Ok(Statement::new(StatementKind::Expression(expr), pos))
    }

    /// Parse variable declaration without consuming terminator (for use in for loops)
    fn parse_var_decl_for_loop(&mut self) -> Result<Statement, ParserError> {
        let pos = self.peek().pos;

        // Parse type
        let var_type = self.parse_type()?;

        // Expect identifier
        let name = match &self.peek().kind {
            TokenKind::Identifier(name) => {
                let name = name.clone();
                self.advance();
                name
            }
            _ => {
                return Err(ParserError::ExpectedToken(
                    "identifier".to_string(),
                    format!("{}", self.peek().kind),
                    self.peek().pos,
                ))
            }
        };

        // Parse optional initializer
        let initializer = if self.match_token(&[TokenKind::Assign]) {
            Some(self.parse_expression()?)
        } else {
            None
        };

        // Add variable to symbol table
        self.symbol_table_mut()
            .add_variable(name.clone(), var_type, pos)
            .map_err(|e| ParserError::UnexpectedToken(e, pos))?;

        Ok(Statement::new(
            StatementKind::VarDecl {
                var_type,
                name,
                initializer,
            },
            pos,
        ))
    }

    /// Parse assignment statement without consuming terminator (for use in for loops)
    fn parse_assignment_statement_for_loop(&mut self) -> Result<Statement, ParserError> {
        let pos = self.peek().pos;

        // Parse variable name
        let name = match &self.peek().kind {
            TokenKind::Identifier(name) => {
                let name = name.clone();
                self.advance();
                name
            }
            _ => {
                return Err(ParserError::ExpectedToken(
                    "identifier".to_string(),
                    format!("{}", self.peek().kind),
                    self.peek().pos,
                ))
            }
        };

        // Check if variable exists
        let var_type = self.symbol_table()
            .lookup_variable(&name)
            .ok_or_else(|| ParserError::UndefinedVariable(name.clone(), pos))?
            .symbol_type;

        // Parse assignment operator
        let op_token = self.advance().clone();

        // Parse value expression
        let value_expr = self.parse_expression()?;

        // Convert compound assignments (+=, -=, etc.) to regular assignment
        let value = match op_token.kind {
            TokenKind::Assign => value_expr,
            TokenKind::PlusAssign => {
                Self::create_compound_assignment(&name, var_type, value_expr, crate::parser::BinaryOp::Add, pos)
            }
            TokenKind::MinusAssign => {
                Self::create_compound_assignment(&name, var_type, value_expr, crate::parser::BinaryOp::Sub, pos)
            }
            TokenKind::MultAssign => {
                Self::create_compound_assignment(&name, var_type, value_expr, crate::parser::BinaryOp::Mul, pos)
            }
            TokenKind::DivAssign => {
                Self::create_compound_assignment(&name, var_type, value_expr, crate::parser::BinaryOp::Div, pos)
            }
            TokenKind::ModAssign => {
                Self::create_compound_assignment(&name, var_type, value_expr, crate::parser::BinaryOp::Mod, pos)
            }
            TokenKind::AndAssign => {
                Self::create_compound_assignment(&name, var_type, value_expr, crate::parser::BinaryOp::And, pos)
            }
            TokenKind::OrAssign => {
                Self::create_compound_assignment(&name, var_type, value_expr, crate::parser::BinaryOp::Or, pos)
            }
            TokenKind::XorAssign => {
                Self::create_compound_assignment(&name, var_type, value_expr, crate::parser::BinaryOp::Xor, pos)
            }
            TokenKind::LeftShiftAssign => {
                Self::create_compound_assignment(&name, var_type, value_expr, crate::parser::BinaryOp::LeftShift, pos)
            }
            TokenKind::RightShiftAssign => {
                Self::create_compound_assignment(&name, var_type, value_expr, crate::parser::BinaryOp::RightShift, pos)
            }
            _ => {
                return Err(ParserError::UnexpectedToken(
                    format!("{}", op_token.kind),
                    pos,
                ))
            }
        };

        Ok(Statement::new(StatementKind::VarAssign { name, value }, pos))
    }

}
