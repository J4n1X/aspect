use crate::lexer::{Token, TokenKind, Keyword, LangType, TypeBase};
use crate::parser::{Expression, ExprKind, BinaryOp, ComparisonOp, LiteralValue, ParserError};
use crate::symbol::table::SymbolTable;

pub struct Parser {
    tokens: Vec<Token>,
    current: usize,
    symbol_table: SymbolTable,
    string_literals: Vec<String>,
}

impl Parser {
    #[must_use]
    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            current: 0,
            symbol_table: SymbolTable::new(),
            string_literals: Vec::new(),
        }
    }

    /// Get reference to symbol table
    #[must_use]
    pub fn symbol_table(&self) -> &SymbolTable {
        &self.symbol_table
    }

    /// Get mutable reference to symbol table
    pub fn symbol_table_mut(&mut self) -> &mut SymbolTable {
        &mut self.symbol_table
    }

    /// Get string literals
    #[must_use]
    pub fn take_string_literals(self) -> Vec<String> {
        self.string_literals
    }

    /// Check if we've reached the end of tokens
    pub(crate) fn is_at_end(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Eof)
    }

    /// Peek at current token without consuming it
    pub(crate) fn peek(&self) -> &Token {
        &self.tokens[self.current]
    }

    /// Peek ahead n tokens
    pub(crate) fn peek_ahead(&self, n: usize) -> Option<&Token> {
        self.tokens.get(self.current + n)
    }

    /// Get previous token
    pub(crate) fn previous(&self) -> &Token {
        &self.tokens[self.current - 1]
    }

    /// Advance to next token
    pub(crate) fn advance(&mut self) -> &Token {
        if !self.is_at_end() {
            self.current += 1;
        }
        self.previous()
    }

    /// Check if current token matches a kind
    pub(crate) fn check(&self, kind: &TokenKind) -> bool {
        if self.is_at_end() {
            return false;
        }
        std::mem::discriminant(&self.peek().kind) == std::mem::discriminant(kind)
    }

    /// Check if current token is a keyword
    pub(crate) fn check_keyword(&self, keyword: &Keyword) -> bool {
        matches!(&self.peek().kind, TokenKind::Keyword(k) if k == keyword)
    }

    /// Consume a token if it matches the expected kind
    pub(crate) fn match_token(&mut self, kinds: &[TokenKind]) -> bool {
        for kind in kinds {
            if self.check(kind) {
                self.advance();
                return true;
            }
        }
        false
    }

    /// Expect a specific token kind and consume it
    pub(crate) fn expect(&mut self, kind: &TokenKind, message: &str) -> Result<&Token, ParserError> {
        if self.check(kind) {
            Ok(self.advance())
        } else {
            Err(ParserError::ExpectedToken(
                message.to_string(),
                format!("{}", self.peek().kind),
                self.peek().pos,
            ))
        }
    }

    /// Skip newline tokens
    pub(crate) fn skip_newlines(&mut self) {
        while matches!(self.peek().kind, TokenKind::Newline) {
            self.advance();
        }
    }

    /// Parse an expression
    pub(crate) fn parse_expression(&mut self) -> Result<Expression, ParserError> {
        self.parse_logical_or()
    }

    /// Parse logical OR expressions (||)
    fn parse_logical_or(&mut self) -> Result<Expression, ParserError> {
        self.parse_binary_expr(Self::parse_logical_and, &[TokenKind::LogicalOr])
    }

    /// Parse logical AND expressions (&&)
    fn parse_logical_and(&mut self) -> Result<Expression, ParserError> {
        self.parse_binary_expr(Self::parse_comparison, &[TokenKind::LogicalAnd])
    }

    /// Parse comparison expressions (==, !=, <, >, <=, >=)
    fn parse_comparison(&mut self) -> Result<Expression, ParserError> {
        let mut left = self.parse_bitwise_or()?;

        while let Some(op) = self.match_comparison_op() {
            let right = self.parse_bitwise_or()?;
            let pos = left.pos;

            // Comparisons return i32 (boolean as integer)
            let result_type = LangType::new(TypeBase::SInt, 32, 0, false);

            left = Expression::new(
                ExprKind::Comparison {
                    left: Box::new(left),
                    op,
                    right: Box::new(right),
                },
                result_type,
                pos,
            );
        }

        Ok(left)
    }

    fn match_comparison_op(&mut self) -> Option<ComparisonOp> {
        let op = match &self.peek().kind {
            TokenKind::Equal => ComparisonOp::Equal,
            TokenKind::NotEqual => ComparisonOp::NotEqual,
            TokenKind::Less => ComparisonOp::Less,
            TokenKind::Greater => ComparisonOp::Greater,
            TokenKind::LessEqual => ComparisonOp::LessEqual,
            TokenKind::GreaterEqual => ComparisonOp::GreaterEqual,
            _ => return None,
        };
        self.advance();
        Some(op)
    }

    /// Parse bitwise OR expressions
    fn parse_bitwise_or(&mut self) -> Result<Expression, ParserError> {
        self.parse_binary_expr(Self::parse_bitwise_xor, &[TokenKind::Pipe])
    }

    /// Parse bitwise XOR expressions
    fn parse_bitwise_xor(&mut self) -> Result<Expression, ParserError> {
        self.parse_binary_expr(Self::parse_bitwise_and, &[TokenKind::Caret])
    }

    /// Parse bitwise AND expressions
    fn parse_bitwise_and(&mut self) -> Result<Expression, ParserError> {
        self.parse_binary_expr(Self::parse_shift, &[TokenKind::Ampersand])
    }

    /// Parse shift expressions
    fn parse_shift(&mut self) -> Result<Expression, ParserError> {
        self.parse_binary_expr(Self::parse_additive, &[TokenKind::LeftShift, TokenKind::RightShift])
    }

    /// Parse additive expressions (+, -)
    fn parse_additive(&mut self) -> Result<Expression, ParserError> {
        self.parse_binary_expr(Self::parse_multiplicative, &[TokenKind::Plus, TokenKind::Minus])
    }

    /// Parse multiplicative expressions (*, /, %)
    fn parse_multiplicative(&mut self) -> Result<Expression, ParserError> {
        self.parse_binary_expr(Self::parse_other, &[TokenKind::Asterisk, TokenKind::Slash, TokenKind::Percent])
    }

    /// Generic binary expression parser
    fn parse_binary_expr<F>(&mut self, next_level: F, operators: &[TokenKind]) -> Result<Expression, ParserError>
    where
        F: Fn(&mut Self) -> Result<Expression, ParserError>,
    {
        let mut left = next_level(self)?;

        while operators.iter().any(|op| self.check(op)) {
            let op_kind = self.peek().kind.clone();
            self.advance();
            let op = self.token_to_binary_op(&op_kind)?;
            let right = next_level(self)?;
            let pos = left.pos;

            // For now, use left's type as result type (proper type checking would go here)
            let result_type = left.expr_type;

            left = Expression::new(
                ExprKind::Binary {
                    left: Box::new(left),
                    op,
                    right: Box::new(right),
                },
                result_type,
                pos,
            );
        }

        Ok(left)
    }

    /// Convert token to binary operator
    fn token_to_binary_op(&self, kind: &TokenKind) -> Result<BinaryOp, ParserError> {
        match kind {
            TokenKind::Plus => Ok(BinaryOp::Add),
            TokenKind::Minus => Ok(BinaryOp::Sub),
            TokenKind::Asterisk => Ok(BinaryOp::Mul),
            TokenKind::Slash => Ok(BinaryOp::Div),
            TokenKind::Percent => Ok(BinaryOp::Mod),
            TokenKind::Ampersand => Ok(BinaryOp::And),
            TokenKind::Pipe => Ok(BinaryOp::Or),
            TokenKind::Caret => Ok(BinaryOp::Xor),
            TokenKind::LeftShift => Ok(BinaryOp::LeftShift),
            TokenKind::RightShift => Ok(BinaryOp::RightShift),
            TokenKind::LogicalAnd => Ok(BinaryOp::LogicalAnd),
            TokenKind::LogicalOr => Ok(BinaryOp::LogicalOr),
            _ => Err(ParserError::InvalidBinaryOperation(self.peek().pos)),
        }
    }

    // TODO: This needs a better name (and it should be written better)
    fn parse_other(&mut self) -> Result<Expression, ParserError> {
        self.parse_alloc()
            .or_else(|_| self.parse_cast())
    }

    /// Parse cast expressions (expr as type)
    fn parse_cast(&mut self) -> Result<Expression, ParserError> {
        let mut expr = self.parse_unary()?;

        while self.check_keyword(&Keyword::As) {
            self.advance(); // consume 'as'

            let target_type = self.parse_type()?;
            let pos = expr.pos;

            expr = Expression::new(
                ExprKind::Cast {
                    expr: Box::new(expr),
                    target_type,
                },
                target_type,
                pos,
            );
        }

        Ok(expr)
    }

    /// Parse unary expressions (-, !, &, *)
    pub(crate) fn parse_unary(&mut self) -> Result<Expression, ParserError> {
        let pos = self.peek().pos;

        match &self.peek().kind {
            TokenKind::Ampersand => {
                self.advance();
                let expr = self.parse_unary()?;

                // Taking address increases pointer depth
                let mut result_type = expr.expr_type;
                result_type.pointer_depth += 1;

                Ok(Expression::new(
                    ExprKind::Reference(Box::new(expr)),
                    result_type,
                    pos,
                ))
            }
            TokenKind::Asterisk => {
                self.advance();
                let expr = self.parse_unary()?;

                // Dereferencing decreases pointer depth
                if expr.expr_type.pointer_depth == 0 {
                    return Err(ParserError::InvalidDereference(pos));
                }

                let mut result_type = expr.expr_type;
                result_type.pointer_depth -= 1;

                Ok(Expression::new(
                    ExprKind::Dereference(Box::new(expr)),
                    result_type,
                    pos,
                ))
            }
            TokenKind::Minus => {
                self.advance();
                let expr = self.parse_unary()?;
                let zero_pos = pos;
                let result_type = expr.expr_type;

                // Unary minus as 0 - expr
                let zero = Expression::new(
                    ExprKind::Literal(LiteralValue::Integer(0)),
                    result_type,
                    zero_pos,
                );

                Ok(Expression::new(
                    ExprKind::Binary {
                        left: Box::new(zero),
                        op: BinaryOp::Sub,
                        right: Box::new(expr),
                    },
                    result_type,
                    pos,
                ))
            }
            TokenKind::LogicalNot => {
                self.advance();
                let expr = self.parse_unary()?;

                // Logical not returns i32 (boolean as integer)
                let result_type = LangType::new(TypeBase::SInt, 32, 0, false);

                Ok(Expression::new(
                    ExprKind::UnaryNot(Box::new(expr)),
                    result_type,
                    pos,
                ))
            }
            TokenKind::Tilde => {
                self.advance();
                let expr = self.parse_unary()?;
                let result_type = expr.expr_type;

                Ok(Expression::new(
                    ExprKind::BitwiseNot(Box::new(expr)),
                    result_type,
                    pos,
                ))
            }
            _ => self.parse_postfix(),
        }
    }

    /// Parse postfix expressions (function calls, array access)
    fn parse_postfix(&mut self) -> Result<Expression, ParserError> {
        let mut expr = self.parse_primary()?;

        if let TokenKind::OpenParen = &self.peek().kind {
            // Function call
            self.advance();
            expr = self.parse_function_call(&expr)?;
        } else if let TokenKind::OpenBracket = &self.peek().kind {
            // Array access
            self.advance();
            expr = self.parse_array_access(&expr)?;
        } else {
            return Ok(expr);
        }

        Ok(expr)
    }

    /// Parse function call (after opening paren)
    fn parse_function_call(&mut self, callee: &Expression) -> Result<Expression, ParserError> {
        let pos = callee.pos;

        // Extract function name
        let func_name = match &callee.kind {
            ExprKind::Variable(name) => name.clone(),
            _ => {
                return Err(ParserError::ExpectedExpression(pos));
            }
        };

        // Look up function in symbol table
        let func_symbol = self.symbol_table.lookup_function(&func_name)
            .ok_or_else(|| ParserError::UndefinedFunction(func_name.clone(), pos))?;

        let return_type = func_symbol.return_type;

        // Parse arguments
        let mut args = Vec::new();

        if !self.check(&TokenKind::CloseParen) {
            loop {
                args.push(self.parse_expression()?);

                if !self.match_token(&[TokenKind::Comma]) {
                    break;
                }
            }
        }

        self.expect(&TokenKind::CloseParen, ")")?;

        Ok(Expression::new(
            ExprKind::FunctionCall {
                name: func_name,
                args,
            },
            return_type,
            pos,
        ))
    }

    fn parse_array_access(&mut self, array_expr: &Expression) -> Result<Expression, ParserError> {
        let pos = array_expr.pos;
        
        // Fetch the index expression
        let index_expr = self.parse_expression()?;
        self.expect(&TokenKind::CloseBracket, "]")?;
        // Make sure the index_expr is an integer type
        if matches!(index_expr.expr_type.base, TypeBase::SInt | TypeBase::UInt) {
            let return_type = {
                let mut t = array_expr.expr_type;
                if t.pointer_depth > 0 {
                    t.pointer_depth -= 1;
                }
                t
            };
            // Combine Binary add and dereference to get array access
            let added_expr = Expression::new(
                ExprKind::Binary {
                    left: Box::new(array_expr.clone()),
                    op: BinaryOp::Add,
                    right: Box::new(index_expr),
                },
                array_expr.expr_type,
                pos,
            );
            Ok(Expression::new(
                ExprKind::Dereference(Box::new(added_expr)),
                return_type,
                pos,
            ))
        } else {
            Err(ParserError::TypeMismatch(
                "integer".to_string(),
                format!("{:?}", index_expr.expr_type),
                index_expr.pos,
            ))
        }
    }

    /// Parse primary expressions (literals, identifiers, parenthesized expressions)
    fn parse_primary(&mut self) -> Result<Expression, ParserError> {
        let pos = self.peek().pos;

        match &self.peek().kind {
            // Integer literal
            TokenKind::Integer(value) => {
                let value = *value;
                self.advance();

                // Choose the smallest signed type that fits the value
                let expr_type = if value >= i32::MIN as i64 && value <= i32::MAX as i64 {
                    LangType::new(TypeBase::SInt, 32, 0, false)
                } else {
                    LangType::new(TypeBase::SInt, 64, 0, false)
                };

                Ok(Expression::new(
                    ExprKind::Literal(LiteralValue::Integer(value)),
                    expr_type,
                    pos,
                ))
            }

            // Float literal
            TokenKind::Float(value) => {
                let value = *value;
                self.advance();

                // Default type is f64
                let expr_type = LangType::new(TypeBase::SFloat, 64, 0, false);

                Ok(Expression::new(
                    ExprKind::Literal(LiteralValue::Float(value)),
                    expr_type,
                    pos,
                ))
            }

            // String literal
            TokenKind::StringLiteral(s) => {
                let string_value = s.clone();
                self.advance();

                // Add to string literals table
                let index = self.string_literals.len();
                self.string_literals.push(string_value);

                // String literals are u8 pointers
                let expr_type = LangType::new(TypeBase::UInt, 8, 1, false);

                Ok(Expression::new(
                    ExprKind::Literal(LiteralValue::String(index)),
                    expr_type,
                    pos,
                ))
            }

            // Identifier (variable reference or function name - defer type lookup)
            TokenKind::Identifier(name) => {
                let name = name.clone();
                self.advance();

                // For now, return a placeholder type - will be resolved in postfix parsing
                // if this is a function call, or should exist as a variable otherwise
                let expr_type = if let Some(var_symbol) = self.symbol_table.lookup_variable(&name) {
                    // Array-to-pointer decay: when an array variable is used in an expression,
                    // it decays to a pointer to its first element
                    if var_symbol.symbol_type.is_array() {
                        var_symbol.symbol_type.decay_to_pointer()
                    } else {
                        var_symbol.symbol_type
                    }
                } else {
                    // Might be a function name, use void as placeholder
                    LangType::new(TypeBase::Void, 0, 0, false)
                };

                Ok(Expression::new(
                    ExprKind::Variable(name),
                    expr_type,
                    pos,
                ))
            }

            // Parenthesized expression
            TokenKind::OpenParen => {
                self.advance();
                let expr = self.parse_expression()?;
                self.expect(&TokenKind::CloseParen, ")")?;
                Ok(expr)
            }

            _ => Err(ParserError::ExpectedExpression(pos)),
        }
    }

    /// Parse a type (including array types like u32[4])
    pub(crate) fn parse_type(&mut self) -> Result<LangType, ParserError> {
        let kind = self.peek().kind.clone();
        match kind {
            TokenKind::LangType(lang_type) => {
                self.advance();
                Ok(lang_type)
            }
            _ => Err(ParserError::ExpectedToken(
                "type".to_string(),
                format!("{}", self.peek().kind),
                self.peek().pos,
            )),
        }
    }

    // NOTE: parse_alloc is kept for backward compatibility with dynamic allocations
    // For preallocated arrays, use the type[size] syntax in variable declarations
    pub(crate) fn parse_alloc(&mut self) -> Result<Expression, ParserError> {
        let pos = self.peek().pos;
        match self.peek().kind {
            TokenKind::LangType(alloc_type) => {
                self.advance();
                self.expect(&TokenKind::OpenBracket, "[")?;
                let count_expr = self.parse_expression()?;
                self.expect(&TokenKind::CloseBracket, "]")?;
                Ok(Expression::new(
                    ExprKind::Alloc {
                        alloc_type,
                        count: Box::new(count_expr),
                    },
                    LangType {
                        base: alloc_type.base,
                        size_bits: alloc_type.size_bits,
                        pointer_depth: alloc_type.pointer_depth + 1,
                        is_const: alloc_type.is_const,
                        array_size: None,
                    },
                    pos,
                ))
            }
            _ => Err(ParserError::ExpectedToken(
                "type for allocation".to_string(),
                format!("{}", self.peek().kind),
                self.peek().pos,
            )),
        }
    }

    /// Parse a complete program
    /// # Errors
    /// If parsing fails at any point.
    pub fn parse_program(&mut self) -> Result<crate::parser::Program, ParserError> {
        use crate::parser::{Program};

        let mut functions = Vec::new();
        let mut global_vars = Vec::new();

        self.skip_newlines();

        while !self.is_at_end() {
            // Check if this is a function or global variable
            let is_extern = self.check_keyword(&Keyword::Extern);

            if is_extern {
                self.advance(); // consume 'extern'
            }

            if self.check_keyword(&Keyword::Fn) {
                // Parse function
                let func = self.parse_function(is_extern)?;
                functions.push(func);
            } else if self.check(&TokenKind::LangType(LangType::new(TypeBase::Void, 0, 0, false)))
                || matches!(self.peek().kind, TokenKind::LangType(_))
            {
                // Parse global variable
                if is_extern {
                    return Err(ParserError::UnexpectedToken(
                        "extern can only be used with functions".to_string(),
                        self.peek().pos,
                    ));
                }
                let global = self.parse_global_var()?;
                global_vars.push(global);
            } else {
                return Err(ParserError::UnexpectedToken(
                    format!("{}", self.peek().kind),
                    self.peek().pos,
                ));
            }

            self.skip_newlines();
        }

        Ok(Program {
            functions,
            global_vars,
            string_literals: self.string_literals.clone(),
        })
    }

    /// Parse a function definition
    fn parse_function(&mut self, is_extern: bool) -> Result<crate::parser::Function, ParserError> {
        use crate::parser::{Function, FunctionProto};
        use crate::symbol::table::FunctionSymbol;

        let pos = self.peek().pos;

        self.expect(&TokenKind::Keyword(Keyword::Fn), "fn")?;

        // Parse function name
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

        self.expect(&TokenKind::OpenParen, "(")?;

        // Parse parameters
        let mut params = Vec::new();

        if !self.check(&TokenKind::CloseParen) {
            loop {
                let param_type = self.parse_type()?;

                let param_name = match &self.peek().kind {
                    TokenKind::Identifier(name) => {
                        let name = name.clone();
                        self.advance();
                        name
                    }
                    _ => {
                        return Err(ParserError::ExpectedToken(
                            "parameter name".to_string(),
                            format!("{}", self.peek().kind),
                            self.peek().pos,
                        ))
                    }
                };

                params.push((param_type, param_name));

                if !self.match_token(&[TokenKind::Comma]) {
                    break;
                }
            }
        }

        self.expect(&TokenKind::CloseParen, ")")?;

        // Parse return type (optional, defaults to void)
        let return_type = if self.match_token(&[TokenKind::Arrow]) {
            self.parse_type()?
        } else {
            LangType::new(TypeBase::Void, 0, 0, false)
        };

        let proto = FunctionProto {
            name: name.clone(),
            params: params.clone(),
            return_type,
            is_extern,
            pos,
        };

        // Add function to symbol table
        let func_symbol = FunctionSymbol {
            name: name.clone(),
            params: params.clone(),
            return_type,
            is_extern,
            has_body: !is_extern,
            pos,
        };

        self.symbol_table_mut()
            .add_function(func_symbol)
            .map_err(|e| ParserError::UnexpectedToken(e, pos))?;

        self.skip_newlines();

        // Parse function body (not for extern functions)
        let body = if is_extern {
            // Extern functions have no body
            self.match_token(&[TokenKind::Semicolon, TokenKind::Newline]);
            Vec::new()
        } else {
            // Enter function scope and add parameters as variables
            self.symbol_table_mut().enter_scope();

            for (param_type, param_name) in &params {
                self.symbol_table_mut()
                    .add_variable(param_name.clone(), *param_type, pos)
                    .map_err(|e| ParserError::UnexpectedToken(e, pos))?;
            }

            // Parse function body block
            let body_stmt = self.parse_block_statement()?;

            self.symbol_table_mut().exit_scope();

            match body_stmt.kind {
                crate::parser::StatementKind::Block(stmts) => stmts,
                _ => unreachable!(),
            }
        };

        Ok(Function { proto, body })
    }

    /// Parse a global variable declaration
    fn parse_global_var(&mut self) -> Result<crate::parser::GlobalVar, ParserError> {
        use crate::parser::GlobalVar;

        let pos = self.peek().pos;

        let var_type = self.parse_type()?;

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

        // Add global variable to symbol table (at global scope)
        self.symbol_table_mut()
            .add_variable(name.clone(), var_type, pos)
            .map_err(|e| ParserError::UnexpectedToken(e, pos))?;

        // Consume optional semicolon or newline
        self.match_token(&[TokenKind::Semicolon, TokenKind::Newline]);

        Ok(GlobalVar {
            var_type,
            name,
            initializer,
            pos,
        })
    }
}
