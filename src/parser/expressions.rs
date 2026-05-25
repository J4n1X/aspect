use indexmap::IndexSet;

use crate::lexer::{Keyword, LangType, Token, TokenKind, TypeBase};
use crate::parser::{
    BinaryOp, ComparisonOp, ExprKind, Expression, LiteralValue, ParserError, Statement,
    StatementKind,
};
use crate::symbol::table::SymbolTable;
use tjlb_macros::parse_rule;

#[derive(Clone, Copy)]
enum OpKind {
    Binary(BinaryOp),
    Comparison(ComparisonOp),
}

struct InfixEntry {
    token: TokenKind,
    op: OpKind,
    prec: i32,
    right_assoc: bool,
}

const INFIX_OPS: &[InfixEntry] = &[
    InfixEntry {
        token: TokenKind::LogicalOr,
        op: OpKind::Binary(BinaryOp::LogicalOr),
        prec: 1,
        right_assoc: false,
    },
    InfixEntry {
        token: TokenKind::LogicalAnd,
        op: OpKind::Binary(BinaryOp::LogicalAnd),
        prec: 2,
        right_assoc: false,
    },
    InfixEntry {
        token: TokenKind::Equal,
        op: OpKind::Comparison(ComparisonOp::Equal),
        prec: 3,
        right_assoc: false,
    },
    InfixEntry {
        token: TokenKind::NotEqual,
        op: OpKind::Comparison(ComparisonOp::NotEqual),
        prec: 3,
        right_assoc: false,
    },
    InfixEntry {
        token: TokenKind::Less,
        op: OpKind::Comparison(ComparisonOp::Less),
        prec: 3,
        right_assoc: false,
    },
    InfixEntry {
        token: TokenKind::Greater,
        op: OpKind::Comparison(ComparisonOp::Greater),
        prec: 3,
        right_assoc: false,
    },
    InfixEntry {
        token: TokenKind::LessEqual,
        op: OpKind::Comparison(ComparisonOp::LessEqual),
        prec: 3,
        right_assoc: false,
    },
    InfixEntry {
        token: TokenKind::GreaterEqual,
        op: OpKind::Comparison(ComparisonOp::GreaterEqual),
        prec: 3,
        right_assoc: false,
    },
    InfixEntry {
        token: TokenKind::Pipe,
        op: OpKind::Binary(BinaryOp::Or),
        prec: 4,
        right_assoc: false,
    },
    InfixEntry {
        token: TokenKind::Caret,
        op: OpKind::Binary(BinaryOp::Xor),
        prec: 5,
        right_assoc: false,
    },
    InfixEntry {
        token: TokenKind::Ampersand,
        op: OpKind::Binary(BinaryOp::And),
        prec: 6,
        right_assoc: false,
    },
    InfixEntry {
        token: TokenKind::LeftShift,
        op: OpKind::Binary(BinaryOp::LeftShift),
        prec: 7,
        right_assoc: false,
    },
    InfixEntry {
        token: TokenKind::RightShift,
        op: OpKind::Binary(BinaryOp::RightShift),
        prec: 7,
        right_assoc: false,
    },
    InfixEntry {
        token: TokenKind::Plus,
        op: OpKind::Binary(BinaryOp::Add),
        prec: 10,
        right_assoc: false,
    },
    InfixEntry {
        token: TokenKind::Minus,
        op: OpKind::Binary(BinaryOp::Sub),
        prec: 10,
        right_assoc: false,
    },
    InfixEntry {
        token: TokenKind::Asterisk,
        op: OpKind::Binary(BinaryOp::Mul),
        prec: 20,
        right_assoc: false,
    },
    InfixEntry {
        token: TokenKind::Slash,
        op: OpKind::Binary(BinaryOp::Div),
        prec: 20,
        right_assoc: false,
    },
    InfixEntry {
        token: TokenKind::Percent,
        op: OpKind::Binary(BinaryOp::Mod),
        prec: 20,
        right_assoc: false,
    },
];

pub struct Parser {
    tokens: Vec<Token>,
    pub(crate) current: usize,
    symbol_table: SymbolTable,
    pub(crate) string_literals: IndexSet<String>,
    pub(crate) context_stack: Vec<&'static str>,
    pub(crate) errors: Vec<ParserError>,
    source_file: String,
}

impl Parser {
    #[must_use]
    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            current: 0,
            symbol_table: SymbolTable::new(),
            string_literals: IndexSet::new(),
            context_stack: Vec::new(),
            errors: Vec::new(),
            source_file: String::new(),
        }
    }

    /// Set the source file name used in error messages.
    #[must_use]
    pub fn with_source_file(mut self, path: impl Into<String>) -> Self {
        self.source_file = path.into();
        self
    }

    /// Format a single error prefixed with the source file and position.
    #[must_use]
    pub fn format_error(&self, err: &ParserError) -> String {
        match err.position() {
            Some(pos) if !self.source_file.is_empty() => {
                format!("{}:{}:{}: {}", self.source_file, pos.line, pos.column, err)
            }
            _ => err.to_string(),
        }
    }

    /// Advance past tokens until a safe recovery point.
    /// Stops BEFORE `}` or statement-starting keywords, AFTER `;`/`\n`.
    pub(crate) fn synchronize(&mut self) {
        while !self.is_at_end() {
            match &self.peek().kind {
                TokenKind::CloseBrace => return,
                TokenKind::Keyword(
                    Keyword::Fn
                    | Keyword::If
                    | Keyword::While
                    | Keyword::For
                    | Keyword::Return
                    | Keyword::Break
                    | Keyword::Continue,
                ) => {
                    return
                }
                TokenKind::Newline | TokenKind::Semicolon => {
                    self.advance();
                    return;
                }
                _ => {
                    self.advance();
                }
            }
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
        self.string_literals.into_iter().collect()
    }

    /// Check if we've reached the end of tokens
    pub(crate) fn is_at_end(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Eof)
    }

    /// Peek at current token without consuming it
    pub(crate) fn peek(&self) -> &Token {
        &self.tokens[self.current]
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

    /// Expect a specific keyword and consume it (validates the inner keyword, unlike `expect`)
    pub(crate) fn expect_keyword(
        &mut self,
        keyword: &Keyword,
        message: &str,
    ) -> Result<&Token, ParserError> {
        if self.check_keyword(keyword) {
            Ok(self.advance())
        } else {
            Err(ParserError::ExpectedToken(
                message.to_string(),
                format!("{}", self.peek().kind),
                self.peek().pos,
            ))
        }
    }

    /// Expect a specific token kind and consume it
    pub(crate) fn expect(
        &mut self,
        kind: &TokenKind,
        message: &str,
    ) -> Result<&Token, ParserError> {
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

    /// True when the current token is a statement terminator or EOF.
    pub(crate) fn check_terminator(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Newline | TokenKind::Semicolon) || self.is_at_end()
    }

    /// Parse an expression
    pub(crate) fn parse_expression(&mut self) -> Result<Expression, ParserError> {
        self.parse_expr_prec(0)
    }

    fn parse_expr_prec(&mut self, min_prec: i32) -> Result<Expression, ParserError> {
        let mut left = self.parse_cast_or_alloc()?;

        while let Some((op, prec, right_assoc)) = INFIX_OPS
            .iter()
            .find(|e| self.check(&e.token) && e.prec >= min_prec)
            .map(|entry| (entry.op, entry.prec, entry.right_assoc))
        {
            self.advance();
            let next_min = if right_assoc { prec } else { prec + 1 };
            let right = self.parse_expr_prec(next_min)?;
            let pos = left.pos;

            left = match op {
                OpKind::Binary(bop) => {
                    let result_type = left.expr_type;
                    Expression::new(
                        ExprKind::Binary {
                            left: Box::new(left),
                            op: bop,
                            right: Box::new(right),
                        },
                        result_type,
                        pos,
                    )
                }
                OpKind::Comparison(cop) => {
                    let result_type = LangType::new(TypeBase::SInt, 32, 0, false);
                    Expression::new(
                        ExprKind::Comparison {
                            left: Box::new(left),
                            op: cop,
                            right: Box::new(right),
                        },
                        result_type,
                        pos,
                    )
                }
            };
        }

        Ok(left)
    }

    fn parse_cast_or_alloc(&mut self) -> Result<Expression, ParserError> {
        let saved = self.current;
        let saved_strlits = self.string_literals.len();
        if let Ok(expr) = self.parse_alloc() {
            return Ok(expr);
        }
        self.current = saved;
        self.string_literals.truncate(saved_strlits);
        self.parse_cast()
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

                // Fold negation into numeric literals so that e.g. `-128` becomes
                // `Literal(Integer(-128))` with the correct type, enabling coercion to
                // narrow signed types like i8 without an explicit cast.
                match &expr.kind {
                    ExprKind::Literal(LiteralValue::Integer(val)) => {
                        let neg = -(*val);
                        let expr_type = if neg >= i32::MIN as i64 && neg <= i32::MAX as i64 {
                            LangType::new(TypeBase::SInt, 32, 0, false)
                        } else {
                            LangType::new(TypeBase::SInt, 64, 0, false)
                        };
                        return Ok(Expression::new(
                            ExprKind::Literal(LiteralValue::Integer(neg)),
                            expr_type,
                            pos,
                        ));
                    }
                    ExprKind::Literal(LiteralValue::Float(val)) => {
                        return Ok(Expression::new(
                            ExprKind::Literal(LiteralValue::Float(-(*val))),
                            expr.expr_type,
                            pos,
                        ));
                    }
                    _ => {}
                }

                // General case: unary minus as 0 - expr
                let result_type = expr.expr_type;
                let zero = Expression::new(
                    ExprKind::Literal(LiteralValue::Integer(0)),
                    result_type,
                    pos,
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

    /// Parse postfix expressions (function calls, array access).
    /// Loops so that chained operations like arr[i][j] or f()() parse correctly.
    fn parse_postfix(&mut self) -> Result<Expression, ParserError> {
        let mut expr = self.parse_primary()?;

        loop {
            expr = match &self.peek().kind {
                TokenKind::OpenParen => {
                    self.advance();
                    self.parse_function_call(&expr)?
                }
                TokenKind::OpenBracket => {
                    self.advance();
                    self.parse_array_access(&expr)?
                }
                _ => break,
            };
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
        let func_symbol = self
            .symbol_table
            .lookup_function(&func_name)
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

                // insert_full deduplicates and returns the stable index in O(1)
                let (index, _) = self.string_literals.insert_full(string_value);

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

                Ok(Expression::new(ExprKind::Variable(name), expr_type, pos))
            }

            // Parenthesized expression
            TokenKind::OpenParen => {
                self.advance();
                let expr = self.parse_expression()?;
                self.expect(&TokenKind::CloseParen, ")")?;
                Ok(expr)
            }

            // List initializer (for array literals and in the future, for struct initializers)
            TokenKind::OpenBrace => self.parse_init_list(),

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

    /// Parse a complete program.
    /// Returns all accumulated errors if any were encountered during parsing.
    /// # Errors
    /// Returns `Err(Vec<ParserError>)` if one or more parse errors occurred.
    pub fn parse_program(&mut self) -> Result<crate::parser::Program, Vec<ParserError>> {
        let result = self.do_parse_program();
        let mut errs = std::mem::take(&mut self.errors);
        match result {
            Ok(prog) if errs.is_empty() => Ok(prog),
            Ok(_) => {
                errs.sort_by_key(|e| {
                    e.position()
                        .map_or((usize::MAX, usize::MAX), |p| (p.line, p.column))
                });
                Err(errs)
            }
            Err(e) => {
                errs.push(e);
                errs.sort_by_key(|e| {
                    e.position()
                        .map_or((usize::MAX, usize::MAX), |p| (p.line, p.column))
                });
                Err(errs)
            }
        }
    }

    #[parse_rule]
    fn do_parse_program(&mut self) -> Result<crate::parser::Program, ParserError> {
        use crate::parser::Program;

        let mut functions = Vec::new();
        let mut global_vars = Vec::new();

        skip_nl!();

        while !self.is_at_end() {
            let is_extern = kw_if!(Extern);

            if self.check_keyword(&Keyword::Fn) {
                let func = self.parse_function(is_extern)?;
                functions.push(func);
            } else if matches!(self.peek().kind, TokenKind::LangType(_)) {
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

            skip_nl!();
        }

        Ok(Program {
            functions,
            global_vars,
            string_literals: self.string_literals.iter().cloned().collect(),
        })
    }

    /// Parse a function definition
    #[parse_rule]
    fn parse_function(&mut self, is_extern: bool) -> Result<crate::parser::Function, ParserError> {
        use crate::parser::{Function, FunctionProto};
        use crate::symbol::table::FunctionSymbol;

        let pos = pos!();
        kw!(Fn);
        let name = ident!();
        token!(OpenParen);

        let mut params = Vec::new();
        if !self.check(&TokenKind::CloseParen) {
            loop {
                let param_type = lang_type!();
                let param_name = ident!();
                params.push((param_type, param_name));
                if !self.match_token(&[TokenKind::Comma]) {
                    break;
                }
            }
        }
        token!(CloseParen);

        let return_type = if self.match_token(&[TokenKind::Arrow]) {
            lang_type!()
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

        self.symbol_table_mut()
            .add_function(FunctionSymbol {
                name: name.clone(),
                params: params.clone(),
                return_type,
                is_extern,
                has_body: !is_extern,
                pos,
            })
            .map_err(|e| ParserError::UnexpectedToken(e, pos))?;

        skip_nl!();

        let body = if is_extern {
            term!();
            Vec::new()
        } else {
            scoped!({
                for (param_type, param_name) in &params {
                    self.symbol_table_mut()
                        .add_variable(param_name.clone(), *param_type, pos)
                        .map_err(|e| ParserError::UnexpectedToken(e, pos))?;
                }
                match self.parse_block_statement()? {
                    Statement {
                        kind: StatementKind::Block(stmts),
                        ..
                    } => stmts,
                    _ => unreachable!(),
                }
            })
        };

        Ok(Function { proto, body })
    }

    /// Parse a global variable declaration
    #[parse_rule]
    fn parse_global_var(&mut self) -> Result<crate::parser::GlobalVar, ParserError> {
        use crate::parser::GlobalVar;

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
            .map_err(|e| ParserError::UnexpectedToken(e, pos))?;

        term!();

        Ok(GlobalVar {
            var_type,
            name,
            initializer,
            pos,
        })
    }

    fn parse_init_list(&mut self) -> Result<Expression, ParserError> {
        let pos = self.peek().pos;
        self.expect(&TokenKind::OpenBrace, "{")?;

        let mut elements = Vec::new();
        self.skip_newlines();
        if !self.check(&TokenKind::CloseBrace) {
            loop {
                self.skip_newlines();
                elements.push(self.parse_expression()?);
                self.skip_newlines();
                if !self.match_token(&[TokenKind::Comma]) {
                    break;
                }
            }
        }
        self.skip_newlines();
        self.expect(&TokenKind::CloseBrace, "}")?;

        Ok(Expression::new(
            ExprKind::ListInitializer(elements),
            LangType::new(TypeBase::Void, 0, 0, false),
            pos,
        ))
    }
}
