use indexmap::IndexSet;

use crate::lexer::{Keyword, LangType, Position, Token, TokenKind, TypeBase};
use crate::parser::{
    BinaryOp, ComparisonOp, ExprKind, Expression, LiteralValue, ParserError, Statement,
    StatementKind,
};
use crate::symbol::module::ModuleSymbols;
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
    /// Transient per-function variable scopes (discarded after parsing).
    symbol_table: SymbolTable,
    /// Cross-phase global symbols (functions, type-structs, aliases); moved into
    /// the `Program` at the end of `parse_program`.
    module: ModuleSymbols,
    pub(crate) string_literals: IndexSet<String>,
    pub(crate) context_stack: Vec<&'static str>,
    pub(crate) errors: Vec<ParserError>,
    /// File registry indexed by `Position::file_id`. Set by the preprocessor;
    /// moved into `Program` at the end of `parse_program` so the type checker
    /// inherits it for its own error formatting.
    source_files: Vec<std::path::PathBuf>,
}

impl Parser {
    #[must_use]
    pub fn new(tokens: Vec<Token>) -> Self {
        Self {
            tokens,
            current: 0,
            symbol_table: SymbolTable::new(),
            module: ModuleSymbols::new(),
            string_literals: IndexSet::new(),
            context_stack: Vec::new(),
            errors: Vec::new(),
            source_files: Vec::new(),
        }
    }

    /// Set a single-entry source-file registry (the simple single-file case).
    /// Equivalent to `with_source_files(vec![path])`.
    #[must_use]
    pub fn with_source_file(mut self, path: impl Into<String>) -> Self {
        self.source_files = vec![std::path::PathBuf::from(path.into())];
        self
    }

    /// Set the full source-file registry from the preprocessor — entry file
    /// at id 0, each `$include`-pulled file at the next ids. Error formatting
    /// uses each error's `pos.file_id` to look up the right filename here.
    #[must_use]
    pub fn with_source_files(mut self, files: Vec<std::path::PathBuf>) -> Self {
        self.source_files = files;
        self
    }

    /// Format a single error prefixed with the source file the error came
    /// from (resolved via `pos.file_id`) and its line/column.
    #[must_use]
    pub fn format_error(&self, err: &ParserError) -> String {
        let Some(pos) = err.position() else {
            return err.to_string();
        };
        match self.source_files.get(pos.file_id as usize) {
            Some(path) => format!("{}:{}:{}: {}", path.display(), pos.line, pos.column, err),
            None => err.to_string(),
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
                    | Keyword::Type
                    | Keyword::Alias
                    | Keyword::If
                    | Keyword::While
                    | Keyword::For
                    | Keyword::Return
                    | Keyword::Break
                    | Keyword::Continue,
                ) => return,
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

                // `&func` for a function name is the function-pointer value
                // itself — no extra indirection. Collapse to keep the AST tidy
                // and avoid a meaningless `Reference(FunctionRef(...))` shape.
                if matches!(expr.kind, ExprKind::FunctionRef(_)) {
                    return Ok(expr);
                }

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
                TokenKind::Dot => {
                    self.advance();
                    self.parse_dot_postfix(expr)?
                }
                _ => break,
            };
        }

        Ok(expr)
    }

    /// Parse function call (after opening paren)
    fn parse_function_call(&mut self, callee: &Expression) -> Result<Expression, ParserError> {
        let pos = callee.pos;

        // Direct call: only a `FunctionRef` (produced by `variable_reference`
        // for known function names) lowers to a `FunctionCall` by-name.
        if let ExprKind::FunctionRef(name) = &callee.kind {
            let func_name = name.clone();
            let func_symbol = self
                .module
                .lookup_function(&func_name)
                .ok_or_else(|| ParserError::UndefinedFunction(func_name.clone(), pos))?;
            let return_type = func_symbol.return_type;

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

            return Ok(Expression::new(
                ExprKind::FunctionCall {
                    name: func_name,
                    args,
                },
                return_type,
                pos,
            ));
        }

        // Indirect call: any expression with a function-pointer type. The
        // signature is in the registry; pull the return type and stamp it.
        // Argument types are checked downstream by the type checker.
        if let TypeBase::FnPtr(id) = callee.expr_type.base
            && callee.expr_type.pointer_depth == 0
        {
            let return_type = self.module.fnptr_sig(id).return_type;
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

            return Ok(Expression::new(
                ExprKind::IndirectCall {
                    callee: Box::new(callee.clone()),
                    args,
                },
                return_type,
                pos,
            ));
        }

        // A bare `Variable(name)` callee that's neither a function nor a
        // fn-ptr-typed local is a typo / undeclared call.
        if let ExprKind::Variable(name) = &callee.kind {
            return Err(ParserError::UndefinedFunction(name.clone(), pos));
        }
        Err(ParserError::ExpectedExpression(pos))
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
            TokenKind::Integer(value) => {
                let value = *value;
                self.advance();
                Ok(Self::integer_literal(value, pos))
            }
            TokenKind::Float(value) => {
                let value = *value;
                self.advance();
                Ok(Self::float_literal(value, pos))
            }
            TokenKind::StringLiteral(s) => {
                let string_value = s.clone();
                self.advance();
                Ok(self.string_literal(string_value, pos))
            }
            TokenKind::Identifier(name) => {
                let name = name.clone();
                self.advance();
                // `KnownType { ... }` is a struct literal; otherwise a variable
                // reference. A bare `{` elsewhere always stays a block.
                if let Some(id) = self.module.struct_id(&name)
                    && self.check(&TokenKind::OpenBrace)
                {
                    return self.parse_struct_literal(id, pos);
                }
                Ok(self.variable_reference(name, pos))
            }
            // Boolean literals
            TokenKind::Keyword(kw @ (Keyword::True | Keyword::False)) => {
                let value = *kw == Keyword::True;
                self.advance();
                Ok(Self::bool_literal(value, pos))
            }
            // Parenthesized expression
            TokenKind::OpenParen => {
                self.advance();
                let expr = self.parse_expression()?;
                self.expect(&TokenKind::CloseParen, ")")?;
                Ok(expr)
            }
            // List initializer (array literals; in the future, struct initializers)
            TokenKind::OpenBrace => self.parse_init_list(),

            _ => Err(ParserError::ExpectedExpression(pos)),
        }
    }

    /// Build an integer-literal node, choosing the smallest signed type that fits.
    fn integer_literal(value: i64, pos: Position) -> Expression {
        let expr_type = if value >= i32::MIN as i64 && value <= i32::MAX as i64 {
            LangType::new(TypeBase::SInt, 32, 0, false)
        } else {
            LangType::new(TypeBase::SInt, 64, 0, false)
        };
        Expression::new(ExprKind::Literal(LiteralValue::Integer(value)), expr_type, pos)
    }

    /// Build a float-literal node (default type `f64`).
    fn float_literal(value: f64, pos: Position) -> Expression {
        let expr_type = LangType::new(TypeBase::SFloat, 64, 0, false);
        Expression::new(ExprKind::Literal(LiteralValue::Float(value)), expr_type, pos)
    }

    /// Build a boolean-literal node (`true`/`false`).
    fn bool_literal(value: bool, pos: Position) -> Expression {
        let expr_type = LangType::new(TypeBase::Bool, 8, 0, false);
        Expression::new(ExprKind::Literal(LiteralValue::Bool(value)), expr_type, pos)
    }

    /// Intern a string literal and build its node (`u8*`).
    fn string_literal(&mut self, value: String, pos: Position) -> Expression {
        // insert_full deduplicates and returns the stable index in O(1)
        let (index, _) = self.string_literals.insert_full(value);
        let expr_type = LangType::new(TypeBase::UInt, 8, 1, false);
        Expression::new(ExprKind::Literal(LiteralValue::String(index)), expr_type, pos)
    }

    /// Build a variable-reference node. The type is looked up in the parser's
    /// symbol table (with array-to-pointer decay); unknown names get a `void`
    /// placeholder and are resolved later (e.g. function names in a call).
    fn variable_reference(&mut self, name: String, pos: Position) -> Expression {
        if let Some(var_symbol) = self.symbol_table.lookup_variable(&name) {
            let expr_type = if var_symbol.symbol_type.is_array() {
                var_symbol.symbol_type.decay_to_pointer()
            } else {
                var_symbol.symbol_type
            };
            return Expression::new(ExprKind::Variable(name), expr_type, pos);
        }
        // Not a variable: a known function name becomes a function-pointer
        // value (`FunctionRef`). Capturing the signature now lets `&foo` and
        // bare `foo` flow through type-checking and codegen uniformly.
        let func_sig = self.module.lookup_function(&name).map(|f| {
            (
                f.params.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
                f.return_type,
            )
        });
        if let Some((params, return_type)) = func_sig {
            let id = self.module.intern_fnptr(params, return_type);
            let ty = LangType::new(TypeBase::FnPtr(id), 0, 0, false);
            return Expression::new(ExprKind::FunctionRef(name), ty, pos);
        }
        // Unknown name: stamp void; the type checker raises UndefinedVariable.
        Expression::new(
            ExprKind::Variable(name),
            LangType::new(TypeBase::Void, 0, 0, false),
            pos,
        )
    }

    /// Parse a type (including array types like u32[4])
    pub(crate) fn parse_type(&mut self) -> Result<LangType, ParserError> {
        let pos = self.peek().pos;
        let kind = self.peek().kind.clone();
        match kind {
            TokenKind::LangType(lang_type) => {
                self.advance();
                Ok(lang_type)
            }
            // Named types: aliases and type-structs. The lexer leaves these as
            // bare identifiers (it cannot know the declared type names), so we
            // resolve them against the module table and attach any `*` pointer
            // modifiers here (built-in types arrive pre-folded from the lexer).
            TokenKind::Identifier(name) => {
                self.advance();
                let base = if let Some(ty) = self.module.resolve_alias(&name) {
                    ty
                } else if let Some(id) = self.module.struct_id(&name) {
                    LangType::new(TypeBase::Struct(id), 0, 0, false)
                } else {
                    return Err(ParserError::UndefinedType(name, pos));
                };
                Ok(self.apply_type_modifiers(base))
            }
            // Parenthesised type: `(T)` — purely a grouping marker. The lexer
            // greedily folds `T[N]` and `T*` into the preceding type token, so
            // parens are the only way to spell things like "array of pointers"
            // (`(i32*)[3]`) or "array of fn-pointers" (`(fn(...) -> R)[N]`).
            // The grouped type composes with the normal trailing modifiers.
            TokenKind::OpenParen => {
                self.advance();
                let inner = self.parse_type()?;
                self.expect(&TokenKind::CloseParen, ")")?;
                Ok(self.apply_type_modifiers(inner))
            }
            // Function-pointer type: `fn(T1, T2, ...) -> R` (or `fn(...)` for
            // a `void`/`u0` return). `fn` here is always followed by `(` — a
            // function *definition* would have an identifier between them.
            TokenKind::Keyword(Keyword::Fn) => {
                self.advance();
                self.expect(&TokenKind::OpenParen, "(")?;
                let mut params: Vec<LangType> = Vec::new();
                if !self.check(&TokenKind::CloseParen) {
                    loop {
                        params.push(self.parse_type()?);
                        if !self.match_token(&[TokenKind::Comma]) {
                            break;
                        }
                    }
                }
                self.expect(&TokenKind::CloseParen, ")")?;
                let return_type = if self.match_token(&[TokenKind::Arrow]) {
                    self.parse_type()?
                } else {
                    LangType::new(TypeBase::Void, 0, 0, false)
                };
                let id = self.module.intern_fnptr(params, return_type);
                let base = LangType::new(TypeBase::FnPtr(id), 0, 0, false);
                Ok(self.apply_type_modifiers(base))
            }
            _ => Err(ParserError::ExpectedToken(
                "type".to_string(),
                format!("{}", self.peek().kind),
                self.peek().pos,
            )),
        }
    }

    /// Consume trailing pointer (`*`) modifiers on a named type and apply them.
    ///
    /// Built-in types arrive from the lexer with `*`/`[N]` already folded in;
    /// named types (aliases / type-structs) lex as a bare identifier, so the
    /// parser attaches pointer modifiers here. Stacks on top of any pointer
    /// depth the resolved type already carries (e.g. `alias P u8*` then `P*`
    /// yields `pointer_depth == 2`).
    fn apply_type_modifiers(&mut self, mut ty: LangType) -> LangType {
        // Mirror the built-in lexer's order: array suffix first, then pointer
        // depth. Restore the cursor on a malformed `[`, so a later `[i]`
        // (index expression) isn't accidentally consumed here.
        if ty.array_size.is_none() && self.check(&TokenKind::OpenBracket) {
            let saved_current = self.current;
            self.advance();
            if let TokenKind::Integer(n) = self.peek().kind {
                let n_val = n;
                self.advance();
                if self.check(&TokenKind::CloseBracket) {
                    self.advance();
                    if let Ok(size) = u32::try_from(n_val) {
                        ty = ty.with_array_size(size);
                    } else {
                        self.current = saved_current;
                    }
                } else {
                    self.current = saved_current;
                }
            } else {
                self.current = saved_current;
            }
        }
        let mut depth = ty.pointer_depth;
        while self.check(&TokenKind::Asterisk) {
            self.advance();
            depth += 1;
        }
        ty.with_pointer_depth(depth)
    }

    /// True when the upcoming tokens begin a *named-type* local declaration:
    /// `<TypeName> [*...] <ident>` where `<TypeName>` is a known alias or
    /// type-struct. Used by the statement dispatcher to tell declarations apart
    /// from assignments / expression statements that merely start with an
    /// identifier. Type names are never values, so `Type *x` is unambiguously a
    /// pointer declaration (not a multiplication).
    pub(crate) fn starts_named_var_decl(&self) -> bool {
        let TokenKind::Identifier(name) = &self.peek().kind else {
            return false;
        };
        let known =
            self.module.resolve_alias(name).is_some() || self.module.struct_id(name).is_some();
        if known {
            // Known type: skip optional `[N]` array modifier, then any pointer
            // modifiers, then require the variable name.
            let mut i = self.current + 1;
            if matches!(
                self.tokens.get(i).map(|t| &t.kind),
                Some(TokenKind::OpenBracket)
            ) && matches!(
                self.tokens.get(i + 1).map(|t| &t.kind),
                Some(TokenKind::Integer(_))
            ) && matches!(
                self.tokens.get(i + 2).map(|t| &t.kind),
                Some(TokenKind::CloseBracket)
            ) {
                i += 3;
            }
            while matches!(self.tokens.get(i).map(|t| &t.kind), Some(TokenKind::Asterisk)) {
                i += 1;
            }
            matches!(
                self.tokens.get(i).map(|t| &t.kind),
                Some(TokenKind::Identifier(_))
            )
        } else {
            // An unknown identifier directly followed by another identifier is
            // only ever a declaration with an undeclared/misspelled type — route
            // it so `parse_type` reports a precise "undefined type". (`a * b` is
            // a multiplication, not a decl, thanks to the operator between them.)
            matches!(
                self.tokens.get(self.current + 1).map(|t| &t.kind),
                Some(TokenKind::Identifier(_))
            )
        }
    }

    /// True when the upcoming tokens begin a *function-pointer* variable
    /// declaration: `fn(...)...` followed eventually by a variable name. Used
    /// by the statement dispatcher (a function *definition* is top-level only,
    /// so any `fn(` in statement position is a fn-ptr type).
    pub(crate) fn starts_fnptr_var_decl(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Keyword(Keyword::Fn))
            && matches!(
                self.tokens.get(self.current + 1).map(|t| &t.kind),
                Some(TokenKind::OpenParen)
            )
    }

    /// True when the upcoming tokens begin a *parenthesised-type* variable
    /// declaration: `(...)` (a grouped type) optionally followed by `[N]`
    /// and/or `*` modifiers, then a variable name. Distinguishes a type
    /// `(T)[N]* ident = ...` from a parenthesised expression statement.
    pub(crate) fn starts_grouped_var_decl(&self) -> bool {
        if !matches!(self.peek().kind, TokenKind::OpenParen) {
            return false;
        }
        // Walk past balanced parens to find what follows the group.
        let mut i = self.current;
        let mut depth: u32 = 0;
        loop {
            let Some(t) = self.tokens.get(i) else {
                return false;
            };
            match &t.kind {
                TokenKind::OpenParen => depth += 1,
                TokenKind::CloseParen => {
                    depth -= 1;
                    if depth == 0 {
                        i += 1;
                        break;
                    }
                }
                TokenKind::Eof => return false,
                _ => {}
            }
            i += 1;
        }
        // Optional `[N]` array modifier.
        if matches!(
            self.tokens.get(i).map(|t| &t.kind),
            Some(TokenKind::OpenBracket)
        ) && matches!(
            self.tokens.get(i + 1).map(|t| &t.kind),
            Some(TokenKind::Integer(_))
        ) && matches!(
            self.tokens.get(i + 2).map(|t| &t.kind),
            Some(TokenKind::CloseBracket)
        ) {
            i += 3;
        }
        // Any number of pointer modifiers.
        while matches!(self.tokens.get(i).map(|t| &t.kind), Some(TokenKind::Asterisk)) {
            i += 1;
        }
        // The variable name must follow.
        matches!(
            self.tokens.get(i).map(|t| &t.kind),
            Some(TokenKind::Identifier(_))
        )
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

        // Pre-register all type-struct names so they resolve regardless of
        // declaration order (self/mutual reference). Aliases resolve at their
        // definition site (define-before-use).
        self.prescan_type_names();

        skip_nl!();

        while !self.is_at_end() {
            let is_extern = kw_if!(Extern);

            // `fn ident(...)` is a function definition; `fn(...)` (no name
            // between `fn` and `(`) is a function-pointer-typed global. The
            // statement-table dispatch handles the local-decl variant.
            if self.check_keyword(&Keyword::Fn) && !self.starts_fnptr_var_decl() {
                let func = self.parse_function(is_extern)?;
                functions.push(func);
            } else if self.check_keyword(&Keyword::Alias) {
                if is_extern {
                    return Err(ParserError::UnexpectedToken(
                        "extern can only be used with functions".to_string(),
                        self.peek().pos,
                    ));
                }
                self.parse_type_alias()?;
            } else if self.check_keyword(&Keyword::Type) {
                if is_extern {
                    return Err(ParserError::UnexpectedToken(
                        "extern can only be used with functions".to_string(),
                        self.peek().pos,
                    ));
                }
                let methods = self.parse_struct_def()?;
                functions.extend(methods);
            } else if matches!(
                self.peek().kind,
                TokenKind::LangType(_) | TokenKind::Identifier(_)
            ) || self.starts_fnptr_var_decl()
                || self.starts_grouped_var_decl()
            {
                // A leading built-in type, named type (alias / type-struct),
                // function-pointer type, or parenthesised group begins a
                // global variable declaration.
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
            symbols: std::mem::take(&mut self.module),
            source_files: self.source_files.clone(),
        })
    }

    /// Pre-register every `type <Name>` struct name with a reserved id before
    /// the main parse, so named types resolve regardless of declaration order
    /// (and self/mutually-referential structs work). Does not consume tokens.
    fn prescan_type_names(&mut self) {
        let names: Vec<String> = self
            .tokens
            .windows(2)
            .filter_map(|w| match (&w[0].kind, &w[1].kind) {
                (TokenKind::Keyword(Keyword::Type), TokenKind::Identifier(name)) => {
                    Some(name.clone())
                }
                _ => None,
            })
            .collect();
        for name in names {
            self.module.intern_struct(&name);
        }
    }

    /// Parse a top-level type alias: `alias NewName TargetType`.
    ///
    /// Aliases are pure compile-time name bindings — they produce no AST node,
    /// only an entry in the module symbol table consulted by `parse_type`.
    #[parse_rule]
    fn parse_type_alias(&mut self) -> Result<(), ParserError> {
        let pos = pos!();
        kw!(Alias);
        let name = ident!();
        if self.module.resolve_alias(&name).is_some() || self.module.struct_id(&name).is_some() {
            return Err(ParserError::DuplicateType(name, pos));
        }
        let target = self.parse_type()?;
        self.module.define_alias(name, target);
        term!();
        Ok(())
    }

    /// Parse a top-level type-struct definition:
    /// `type Name { [public] Type field <term> ...  [const?] fn method(...) {...} ... }`.
    ///
    /// Fields must come before methods. Methods are desugared into mangled
    /// free functions (`Type$method`) and returned to `do_parse_program` for
    /// inclusion in `Program::functions`.
    #[parse_rule]
    fn parse_struct_def(&mut self) -> Result<Vec<crate::parser::Function>, ParserError> {
        use crate::symbol::module::{FieldInfo, Visibility};

        let pos = pos!();
        kw!(Type);
        let name = ident!();
        let id = self
            .module
            .struct_id(&name)
            .expect("type-struct name reserved during prescan");

        // A non-empty field set means this name was already defined.
        if !self.module.struct_info(id).fields.is_empty() {
            return Err(ParserError::DuplicateType(name, pos));
        }

        token!(OpenBrace);

        let mut fields: Vec<FieldInfo> = Vec::new();
        let mut methods: Vec<crate::parser::Function> = Vec::new();
        // Fields are finalised the moment we transition to method parsing so
        // method bodies (including `return Self { ... }`) see the layout.
        let mut fields_set = false;

        loop {
            skip_nl!();
            if self.check(&TokenKind::CloseBrace) || self.is_at_end() {
                break;
            }

            // Method start? `fn name(...)` or `const fn name(...)`.
            let is_const_fn = self.check_keyword(&Keyword::Const);
            if is_const_fn || self.check_keyword(&Keyword::Fn) {
                if !fields_set {
                    self.module.set_fields(id, std::mem::take(&mut fields));
                    fields_set = true;
                }
                if is_const_fn {
                    self.advance(); // consume `const`
                    skip_nl!();
                }
                let method = self.parse_method(id, &name, is_const_fn)?;
                methods.push(method);
                continue;
            }

            // Field. Fields must come before any method.
            if fields_set {
                return Err(ParserError::UnexpectedToken(
                    "fields must be declared before methods".to_string(),
                    self.peek().pos,
                ));
            }
            let vis = if kw_if!(Public) {
                Visibility::Public
            } else {
                Visibility::Private
            };
            let field_type = lang_type!();
            let field_name = ident!();
            fields.push(FieldInfo {
                name: field_name,
                ty: field_type,
                vis,
            });
            self.match_token(&[TokenKind::Semicolon, TokenKind::Newline]);
        }
        token!(CloseBrace);

        // A method-less struct never triggered the transition above.
        if !fields_set {
            self.module.set_fields(id, fields);
        }

        Ok(methods)
    }

    /// Parse a method inside a `type` body. Methods are desugared to free
    /// functions named `Type$method`. An instance method takes a leading bare
    /// `this` receiver (no type annotation); the parser supplies it as an
    /// implicit `*Struct` (or `*const Struct` for `const fn`) first parameter.
    /// A static method omits `this`.
    #[parse_rule]
    fn parse_method(
        &mut self,
        struct_id: u32,
        struct_name: &str,
        is_const_fn: bool,
    ) -> Result<crate::parser::Function, ParserError> {
        use crate::parser::{Function, FunctionProto};
        use crate::symbol::module::MethodSig;
        use crate::symbol::table::FunctionSymbol;

        let pos = pos!();
        kw!(Fn);
        let method_name = ident!();
        token!(OpenParen);

        // Optional implicit `this` receiver, then any user parameters.
        let mut params: Vec<(LangType, String)> = Vec::new();
        let has_this = if let TokenKind::Identifier(n) = &self.peek().kind
            && n == "this"
        {
            self.advance();
            let receiver_ty = LangType {
                base: TypeBase::Struct(struct_id),
                size_bits: 0,
                pointer_depth: 1,
                is_const: is_const_fn,
                array_size: None,
            };
            params.push((receiver_ty, "this".to_string()));
            // Optional comma before the next param.
            self.match_token(&[TokenKind::Comma]);
            true
        } else {
            false
        };

        while !self.check(&TokenKind::CloseParen) && !self.is_at_end() {
            let param_type = self.parse_type()?;
            let param_name = match &self.peek().kind {
                TokenKind::Identifier(n) => {
                    let n = n.clone();
                    self.advance();
                    n
                }
                _ => {
                    return Err(ParserError::ExpectedToken(
                        "parameter name".to_string(),
                        format!("{}", self.peek().kind),
                        self.peek().pos,
                    ));
                }
            };
            params.push((param_type, param_name));
            if !self.match_token(&[TokenKind::Comma]) {
                break;
            }
        }
        token!(CloseParen);

        if is_const_fn && !has_this {
            return Err(ParserError::UnexpectedToken(
                "`const fn` requires a `this` receiver".to_string(),
                pos,
            ));
        }

        let return_type = if self.match_token(&[TokenKind::Arrow]) {
            lang_type!()
        } else {
            LangType::new(TypeBase::Void, 0, 0, false)
        };

        let mangled = format!("{struct_name}${method_name}");

        let proto = FunctionProto {
            name: mangled.clone(),
            params: params.clone(),
            return_type,
            is_extern: false,
            pos,
        };

        // Register so plain `FunctionCall { name: mangled, ... }` resolves.
        self.module
            .add_function(FunctionSymbol {
                name: mangled.clone(),
                params: params.clone(),
                return_type,
                is_extern: false,
                has_body: true,
                pos,
            })
            .map_err(|e| ParserError::from_symbol(e, pos))?;

        // Register in the struct's method registry (params exclude `this`).
        let visible_params: Vec<(LangType, String)> = if has_this {
            params[1..].to_vec()
        } else {
            params.clone()
        };
        self.module.add_method(
            struct_id,
            method_name,
            MethodSig {
                mangled_name: mangled,
                params: visible_params,
                return_type,
                is_static: !has_this,
                is_const: is_const_fn,
            },
        );

        skip_nl!();

        let body = scoped!({
            for (param_type, param_name) in &params {
                self.symbol_table_mut()
                    .add_variable(param_name.clone(), *param_type, pos)
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

        Ok(Function { proto, body })
    }

    /// `true` when `name` is a method of `base`'s type (instance form) or of
    /// the type whose name `base` resolves to (static form). Used to decide
    /// between method-call dispatch and field-access in `parse_dot_postfix`.
    fn identifier_is_method_of_base(&self, base: &Expression, name: &str) -> bool {
        // Instance: base's type is a type-struct (value or pointer).
        if let TypeBase::Struct(id) = base.expr_type.base
            && self
                .module
                .struct_info(id)
                .methods
                .contains_key(name)
        {
            return true;
        }
        // Static: base is a bare identifier naming a known type-struct, with
        // no local variable shadowing it.
        if let ExprKind::Variable(var_name) = &base.kind
            && let Some(id) = self.module.struct_id(var_name)
            && self.symbol_table.lookup_variable(var_name).is_none()
            && self.module.struct_info(id).methods.contains_key(name)
        {
            return true;
        }
        false
    }

    /// Parse a `.ident` postfix — the `.` was already consumed. The followup
    /// distinguishes two forms:
    /// - `base.method(args)` → a method call, desugared to a `FunctionCall`
    ///   with the mangled name `Type$method` (autorefs value receivers; static
    ///   form `Type.method(...)` carries no receiver).
    /// - `base.field` → a `FieldAccess`, with a best-effort field-type stamp.
    fn parse_dot_postfix(&mut self, base: Expression) -> Result<Expression, ParserError> {
        let pos = base.pos;
        let name = match &self.peek().kind {
            TokenKind::Identifier(n) => {
                let n = n.clone();
                self.advance();
                n
            }
            _ => {
                return Err(ParserError::ExpectedToken(
                    "field or method name".to_string(),
                    format!("{}", self.peek().kind),
                    self.peek().pos,
                ));
            }
        };

        // Method call: `.ident(args)` — only when `ident` is actually a method
        // of the base's type. Otherwise (e.g. `.callback(` where `callback` is
        // a *field* of function-pointer type), drop through to field access
        // and let the postfix loop's `OpenParen` arm emit an indirect call.
        if self.check(&TokenKind::OpenParen) && self.identifier_is_method_of_base(&base, &name) {
            self.advance();
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
            return self.build_method_call(base, &name, args, pos);
        }

        // Field access.
        let field_type = match base.expr_type.base {
            TypeBase::Struct(id) => self
                .module
                .field(id, &name)
                .map_or_else(|| LangType::new(TypeBase::Void, 0, 0, false), |(_, f)| f.ty),
            _ => LangType::new(TypeBase::Void, 0, 0, false),
        };

        Ok(Expression::new(
            ExprKind::FieldAccess {
                base: Box::new(base),
                field: name,
            },
            field_type,
            pos,
        ))
    }

    /// Build a method-call expression for `obj.method(args)` or
    /// `Type.method(args)`. Resolves the mangled name (`Type$method`), picks
    /// instance-vs-static, and autorefs value receivers.
    fn build_method_call(
        &mut self,
        base: Expression,
        method_name: &str,
        args: Vec<Expression>,
        pos: Position,
    ) -> Result<Expression, ParserError> {
        // Static call: `TypeName.method(args)` — `base` is `Variable(TypeName)`
        // for a known struct *and* there is no local variable shadowing it.
        if let ExprKind::Variable(var_name) = &base.kind
            && let Some(id) = self.module.struct_id(var_name)
            && self.symbol_table.lookup_variable(var_name).is_none()
        {
            let type_name = self.module.struct_info(id).name.clone();
            // Strict: the static-call form must resolve to a static method (one
            // declared without `this`). An instance method declared `fn m(this,
            // ...)` must be called as `obj.m(...)`, not `Type.m(&obj, ...)` —
            // the two syntactic forms map cleanly to the two kinds.
            if let Some(sig) = self.module.struct_info(id).methods.get(method_name)
                && !sig.is_static
            {
                return Err(ParserError::MethodCallForm(
                    format!(
                        "'{type_name}.{method_name}' is an instance method; \
                         call it as `<receiver>.{method_name}(...)`"
                    ),
                    pos,
                ));
            }
            let mangled = format!("{type_name}${method_name}");
            let return_type = self.module.lookup_function(&mangled).map_or_else(
                || LangType::new(TypeBase::Void, 0, 0, false),
                |f| f.return_type,
            );
            return Ok(Expression::new(
                ExprKind::FunctionCall {
                    name: mangled,
                    args,
                },
                return_type,
                pos,
            ));
        }

        // Instance call: `base` must be a type-struct value or pointer-to-struct.
        let bt = base.expr_type;
        let id = match bt.base {
            TypeBase::Struct(id) => id,
            _ => {
                return Err(ParserError::TypeMismatch(
                    "type-struct".to_string(),
                    format!("{bt}"),
                    pos,
                ));
            }
        };
        let type_name = self.module.struct_info(id).name.clone();
        // Strict: the instance-call form must resolve to an instance method.
        // A static method (no `this`) must be invoked as `Type.method(...)`,
        // not `obj.method(...)`.
        if let Some(sig) = self.module.struct_info(id).methods.get(method_name)
            && sig.is_static
        {
            return Err(ParserError::MethodCallForm(
                format!(
                    "'{type_name}.{method_name}' is a static method; \
                     call it as `{type_name}.{method_name}(...)` without a receiver"
                ),
                pos,
            ));
        }
        let mangled = format!("{type_name}${method_name}");
        let return_type = self.module.lookup_function(&mangled).map_or_else(
            || LangType::new(TypeBase::Void, 0, 0, false),
            |f| f.return_type,
        );

        // Receiver: autoref a value, pass a pointer as-is; deeper pointers fail.
        let receiver = match bt.pointer_depth {
            0 => {
                let ref_ty = LangType {
                    base: bt.base,
                    size_bits: bt.size_bits,
                    pointer_depth: 1,
                    is_const: bt.is_const,
                    array_size: None,
                };
                let base_pos = base.pos;
                Expression::new(ExprKind::Reference(Box::new(base)), ref_ty, base_pos)
            }
            1 => base,
            _ => {
                return Err(ParserError::TypeMismatch(
                    "type-struct or pointer-to-type-struct".to_string(),
                    format!("{bt}"),
                    pos,
                ));
            }
        };

        let mut all_args = Vec::with_capacity(args.len() + 1);
        all_args.push(receiver);
        all_args.extend(args);

        Ok(Expression::new(
            ExprKind::FunctionCall {
                name: mangled,
                args: all_args,
            },
            return_type,
            pos,
        ))
    }

    /// Parse a struct literal body after the type name: `{ field = expr, ... }`.
    /// The opening brace has not yet been consumed.
    #[parse_rule]
    fn parse_struct_literal(
        &mut self,
        struct_id: u32,
        pos: Position,
    ) -> Result<Expression, ParserError> {
        token!(OpenBrace);
        let mut fields = Vec::new();
        loop {
            skip_nl!();
            if self.check(&TokenKind::CloseBrace) || self.is_at_end() {
                break;
            }
            let field_name = ident!();
            self.expect(&TokenKind::Assign, "=")?;
            let value = self.parse_expression()?;
            fields.push((field_name, value));
            skip_nl!();
            if !self.match_token(&[TokenKind::Comma]) {
                break;
            }
        }
        skip_nl!();
        token!(CloseBrace);

        let expr_type = LangType::new(TypeBase::Struct(struct_id), 0, 0, false);
        Ok(Expression::new(
            ExprKind::StructLiteral { struct_id, fields },
            expr_type,
            pos,
        ))
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

        self.module
            .add_function(FunctionSymbol {
                name: name.clone(),
                params: params.clone(),
                return_type,
                is_extern,
                has_body: !is_extern,
                pos,
            })
            .map_err(|e| ParserError::from_symbol(e, pos))?;

        skip_nl!();

        let body = if is_extern {
            term!();
            Vec::new()
        } else {
            scoped!({
                for (param_type, param_name) in &params {
                    self.symbol_table_mut()
                        .add_variable(param_name.clone(), *param_type, pos)
                        .map_err(|e| ParserError::from_symbol(e, pos))?;
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
            .map_err(|e| ParserError::from_symbol(e, pos))?;

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
