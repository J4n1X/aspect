use indexmap::IndexSet;

use crate::lexer::{Keyword, LangType, Position, Token, TokenKind, TypeBase};
use crate::parser::program::PendingBody;
use crate::parser::{BinaryOp, ComparisonOp, ExprKind, Expression, LiteralValue, ParserError};
use crate::symbol::module::ModuleSymbols;
use crate::symbol::table::SymbolTable;
use aspect_macros::parse_rule;

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

/// A left-associative binary-operator entry.
const fn bin(token: TokenKind, op: BinaryOp, prec: i32) -> InfixEntry {
    InfixEntry {
        token,
        op: OpKind::Binary(op),
        prec,
        right_assoc: false,
    }
}

/// A left-associative comparison-operator entry.
const fn cmp(token: TokenKind, op: ComparisonOp, prec: i32) -> InfixEntry {
    InfixEntry {
        token,
        op: OpKind::Comparison(op),
        prec,
        right_assoc: false,
    }
}

/// Infix binding powers, weakest first:
/// `||` < `&&` < comparisons < `|` < `^` < `&` < shifts < `+ -` < `* / %`.
/// All current operators are left-associative; `right_assoc` exists for
/// future ones (e.g. exponentiation).
const INFIX_OPS: &[InfixEntry] = &[
    bin(TokenKind::LogicalOr, BinaryOp::LogicalOr, 1),
    bin(TokenKind::LogicalAnd, BinaryOp::LogicalAnd, 2),
    cmp(TokenKind::Equal, ComparisonOp::Equal, 3),
    cmp(TokenKind::NotEqual, ComparisonOp::NotEqual, 3),
    cmp(TokenKind::Less, ComparisonOp::Less, 3),
    cmp(TokenKind::Greater, ComparisonOp::Greater, 3),
    cmp(TokenKind::LessEqual, ComparisonOp::LessEqual, 3),
    cmp(TokenKind::GreaterEqual, ComparisonOp::GreaterEqual, 3),
    bin(TokenKind::Pipe, BinaryOp::Or, 4),
    bin(TokenKind::Caret, BinaryOp::Xor, 5),
    bin(TokenKind::Ampersand, BinaryOp::And, 6),
    bin(TokenKind::LeftShift, BinaryOp::LeftShift, 7),
    bin(TokenKind::RightShift, BinaryOp::RightShift, 7),
    bin(TokenKind::Plus, BinaryOp::Add, 10),
    bin(TokenKind::Minus, BinaryOp::Sub, 10),
    bin(TokenKind::Asterisk, BinaryOp::Mul, 20),
    bin(TokenKind::Slash, BinaryOp::Div, 20),
    bin(TokenKind::Percent, BinaryOp::Mod, 20),
];

pub struct Parser {
    pub(crate) tokens: Vec<Token>,
    pub(crate) current: usize,
    /// Transient per-function variable scopes (discarded after parsing).
    pub(crate) symbol_table: SymbolTable,
    /// Cross-phase global symbols (functions, type-structs, aliases); moved into
    /// the `Program` at the end of `parse_program`.
    pub(crate) module: ModuleSymbols,
    pub(crate) string_literals: IndexSet<String>,
    pub(crate) context_stack: Vec<&'static str>,
    pub(crate) errors: Vec<ParserError>,
    /// Function bodies skipped during pass 1 of `do_parse_program`, parsed in
    /// pass 2 once every prototype is registered (forward references).
    pub(crate) pending_bodies: Vec<PendingBody>,
    /// Token indices of `alias` keywords whose definition `prescan_aliases`
    /// successfully installed. Pass 1 only consumes tokens at these sites;
    /// sites not in here are re-parsed to produce their error.
    pub(crate) alias_prescan_sites: std::collections::HashSet<usize>,
    /// File registry indexed by `Position::file_id`. Set by the preprocessor;
    /// moved into `Program` at the end of `parse_program` so the type checker
    /// inherits it for its own error formatting.
    pub(crate) source_files: Vec<std::path::PathBuf>,
    /// Module of each file, indexed by `Position::file_id` (parallel to
    /// `source_files`). Set via [`Parser::with_module_info`]; files without
    /// an entry — including everything when no module info was threaded —
    /// belong to the anonymous root module `""`.
    file_modules: Vec<String>,
    /// Module → its *direct* imports, from the preprocessor. Drives the
    /// import-visibility check ([`Parser::check_import_visibility`]).
    module_imports: std::collections::HashMap<String, Vec<String>>,
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
            pending_bodies: Vec::new(),
            alias_prescan_sites: std::collections::HashSet::new(),
            source_files: Vec::new(),
            file_modules: Vec::new(),
            module_imports: std::collections::HashMap::new(),
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
    /// at id 0, each `$import`-pulled file at the next ids. Error formatting
    /// uses each error's `pos.file_id` to look up the right filename here.
    #[must_use]
    pub fn with_source_files(mut self, files: Vec<std::path::PathBuf>) -> Self {
        self.source_files = files;
        self
    }

    /// Set the module registry from the preprocessor: each file's module (one
    /// entry per file in `file_id` order, mirroring
    /// `PreprocessedSource::modules`) and each module's *direct* imports.
    /// Enables the import-visibility check — without this, every file belongs
    /// to the anonymous root module `""` and every reference is same-module.
    #[must_use]
    pub fn with_module_info(
        mut self,
        modules: Vec<(u32, String)>,
        imports: std::collections::HashMap<String, Vec<String>>,
    ) -> Self {
        debug_assert!(
            modules
                .iter()
                .enumerate()
                .all(|(i, (id, _))| *id as usize == i),
            "module registry must have exactly one entry per file in file_id order"
        );
        self.file_modules = modules.into_iter().map(|(_, module)| module).collect();
        self.module_imports = imports;
        self
    }

    /// The module the file `file_id` belongs to. Files without an entry —
    /// including every file when no module info was threaded — belong to the
    /// anonymous root module `""`.
    fn module_of_file(&self, file_id: u32) -> &str {
        self.file_modules
            .get(file_id as usize)
            .map_or("", String::as_str)
    }

    /// Enforce import visibility for one resolved reference: a symbol defined
    /// in a file of module N may be referenced from a file of module M iff
    /// `N == M` or N is a *direct* import of M (imports do not trickle down).
    /// `def_file_id` is the symbol's defining file; `use_pos` is the use site
    /// (whose `file_id` determines the referring module).
    fn check_import_visibility(
        &self,
        kind: &'static str,
        name: &str,
        def_file_id: u32,
        use_pos: Position,
    ) -> Result<(), ParserError> {
        let def_module = self.module_of_file(def_file_id);
        let use_module = self.module_of_file(use_pos.file_id);
        if def_module == use_module
            || self
                .module_imports
                .get(use_module)
                .is_some_and(|imports| imports.iter().any(|import| import == def_module))
        {
            return Ok(());
        }
        Err(ParserError::not_imported(
            kind, name, def_module, use_module, use_pos,
        ))
    }

    /// Visibility check for *using* a type-struct: naming it (type
    /// annotation, struct literal, the `Type` in a static `Type.method`
    /// reference) or calling any of its methods, including on an instance.
    /// Two gates, in order: the defining module must be the referring module
    /// or one of its direct imports (the general import rule), and a
    /// cross-module use additionally requires the struct to be declared
    /// `public type` (module encapsulation). A member's own `public` is
    /// capped by the type's: a public method of a private type is visible
    /// module-wide but never outside it. Values of a foreign private type
    /// may still *flow* through outside code (returned from and passed back
    /// into the defining module's public functions).
    fn check_struct_visibility(&self, id: u32, use_pos: Position) -> Result<(), ParserError> {
        let info = self.module.struct_info(id);
        self.check_import_visibility("type-struct", &info.name, info.file_id, use_pos)?;
        let def_module = self.module_of_file(info.file_id);
        let use_module = self.module_of_file(use_pos.file_id);
        if info.vis == crate::symbol::module::Visibility::Private && def_module != use_module {
            return Err(ParserError::private_type(
                &info.name, def_module, use_module, use_pos,
            ));
        }
        Ok(())
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

    #[must_use]
    pub fn symbol_table(&self) -> &SymbolTable {
        &self.symbol_table
    }

    pub fn symbol_table_mut(&mut self) -> &mut SymbolTable {
        &mut self.symbol_table
    }

    #[must_use]
    pub fn take_string_literals(self) -> Vec<String> {
        self.string_literals.into_iter().collect()
    }

    pub(crate) fn is_at_end(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Eof)
    }

    pub(crate) fn peek(&self) -> &Token {
        &self.tokens[self.current]
    }

    pub(crate) fn previous(&self) -> &Token {
        &self.tokens[self.current - 1]
    }

    pub(crate) fn advance(&mut self) -> &Token {
        if !self.is_at_end() {
            self.current += 1;
        }
        self.previous()
    }

    /// Compares by discriminant only — payloads are ignored, so e.g.
    /// `check(&TokenKind::Integer(0))` matches *any* integer token.
    pub(crate) fn check(&self, kind: &TokenKind) -> bool {
        if self.is_at_end() {
            return false;
        }
        std::mem::discriminant(&self.peek().kind) == std::mem::discriminant(kind)
    }

    pub(crate) fn check_keyword(&self, keyword: &Keyword) -> bool {
        matches!(&self.peek().kind, TokenKind::Keyword(k) if k == keyword)
    }

    /// Consume the current token if it matches any of `kinds`.
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

    /// Consume an identifier token and return its name; `what` names the
    /// expected item in the error message (e.g. "parameter name").
    pub(crate) fn parse_ident(&mut self, what: &str) -> Result<String, ParserError> {
        match &self.peek().kind {
            TokenKind::Identifier(name) => {
                let name = name.clone();
                self.advance();
                Ok(name)
            }
            _ => Err(ParserError::ExpectedToken(
                what.to_string(),
                format!("{}", self.peek().kind),
                self.peek().pos,
            )),
        }
    }

    /// Parse a comma-separated list of items, then expect and consume the
    /// `close` delimiter. The list may be empty; no trailing comma.
    pub(crate) fn parse_comma_separated<T>(
        &mut self,
        close: &TokenKind,
        mut parse_item: impl FnMut(&mut Self) -> Result<T, ParserError>,
    ) -> Result<Vec<T>, ParserError> {
        let mut items = Vec::new();
        if !self.check(close) {
            loop {
                items.push(parse_item(self)?);
                if !self.match_token(&[TokenKind::Comma]) {
                    break;
                }
            }
        }
        let close_msg = match close {
            TokenKind::CloseParen => ")",
            TokenKind::CloseBracket => "]",
            TokenKind::CloseBrace => "}",
            _ => "closing delimiter",
        };
        self.expect(close, close_msg)?;
        Ok(items)
    }

    pub(crate) fn skip_newlines(&mut self) {
        while matches!(self.peek().kind, TokenKind::Newline) {
            self.advance();
        }
    }

    /// True when the current token ends a statement: a `;`, a newline, an EOF.
    pub(crate) fn check_terminator(&self) -> bool {
        matches!(
            self.peek().kind,
            TokenKind::Newline | TokenKind::Semicolon
        ) || self.is_at_end()
    }

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
                    let result_type = if left.expr_type.pointer_depth == 0
                        && right.expr_type.pointer_depth > 0
                    {
                        right.expr_type
                    } else {
                        left.expr_type
                    };
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
                    let result_type = LangType::I32;
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
            self.advance();
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

    /// Parse unary expressions (`-`, `!`, `&`, `*`, `~`)
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
                            LangType::I32
                        } else {
                            LangType::I64
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
                let result_type = LangType::I32;

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
            // `variable_reference` already vetted the ref, but keep the call
            // site guarded in its own right (defense in depth — the check is
            // one hash lookup).
            self.check_import_visibility("function", &func_name, func_symbol.pos.file_id, pos)?;

            let args = self.parse_comma_separated(&TokenKind::CloseParen, Self::parse_expression)?;

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
            let args = self.parse_comma_separated(&TokenKind::CloseParen, Self::parse_expression)?;

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

        let index_expr = self.parse_expression()?;
        self.expect(&TokenKind::CloseBracket, "]")?;
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
                    self.check_struct_visibility(id, pos)?;
                    return self.parse_struct_literal(id, pos);
                }
                self.variable_reference(name, pos)
            }
            TokenKind::Keyword(kw @ (Keyword::True | Keyword::False)) => {
                let value = *kw == Keyword::True;
                self.advance();
                Ok(Self::bool_literal(value, pos))
            }
            // `null` — the untyped null pointer. Stamps `u8*` as a placeholder
            // (any single-pointer type would do — pointer-to-pointer coercion
            // is structural by depth, not by base). The type checker upgrades
            // the stamp to the contextual target in `check` mode.
            TokenKind::Keyword(Keyword::Null) => {
                self.advance();
                let placeholder = LangType::U8_PTR;
                Ok(Expression::new(ExprKind::Null, placeholder, pos))
            }
            // `sizeof(T)` — compile-time byte size of a type as a `u64`.
            TokenKind::Keyword(Keyword::Sizeof) => {
                self.advance();
                self.expect(&TokenKind::OpenParen, "(")?;
                let ty = self.parse_type()?;
                self.expect(&TokenKind::CloseParen, ")")?;
                let u64_ty = LangType::U64;
                Ok(Expression::new(ExprKind::SizeOf(ty), u64_ty, pos))
            }
            TokenKind::OpenParen => {
                self.advance();
                let expr = self.parse_expression()?;
                self.expect(&TokenKind::CloseParen, ")")?;
                Ok(expr)
            }
            // A brace expression: list initializer (`{1, 2, 3}`) or
            // value-block (`{ ...; return v }`) — see `parse_brace_expression`.
            TokenKind::OpenBrace => self.parse_brace_expression(),

            _ => Err(ParserError::ExpectedExpression(pos)),
        }
    }

    /// Build an integer-literal node, choosing the smallest signed type that fits.
    fn integer_literal(value: i64, pos: Position) -> Expression {
        let expr_type = if value >= i32::MIN as i64 && value <= i32::MAX as i64 {
            LangType::I32
        } else {
            LangType::I64
        };
        Expression::new(ExprKind::Literal(LiteralValue::Integer(value)), expr_type, pos)
    }

    /// Build a float-literal node (default type `f64`).
    fn float_literal(value: f64, pos: Position) -> Expression {
        let expr_type = LangType::F64;
        Expression::new(ExprKind::Literal(LiteralValue::Float(value)), expr_type, pos)
    }

    /// Build a boolean-literal node (`true`/`false`).
    fn bool_literal(value: bool, pos: Position) -> Expression {
        let expr_type = LangType::BOOL;
        Expression::new(ExprKind::Literal(LiteralValue::Bool(value)), expr_type, pos)
    }

    /// Intern a string literal and build its node (`u8*`).
    fn string_literal(&mut self, value: String, pos: Position) -> Expression {
        // insert_full deduplicates and returns the stable index in O(1)
        let (index, _) = self.string_literals.insert_full(value);
        let expr_type = LangType::U8_PTR;
        Expression::new(ExprKind::Literal(LiteralValue::String(index)), expr_type, pos)
    }

    /// Build a variable-reference node. The type is looked up in the parser's
    /// symbol table (with array-to-pointer decay); unknown names get a `void`
    /// placeholder and are resolved later (e.g. function names in a call).
    ///
    /// # Errors
    /// [`ParserError::NotImported`] when the name resolves to a global
    /// variable or function defined in a module the use site's module does
    /// not import. Locals and parameters are exempt (same-function by
    /// construction).
    fn variable_reference(&mut self, name: String, pos: Position) -> Result<Expression, ParserError> {
        if let Some((var_symbol, is_global)) = self.symbol_table.lookup_variable_scoped(&name) {
            let expr_type = if var_symbol.symbol_type.is_array() {
                var_symbol.symbol_type.decay_to_pointer()
            } else {
                var_symbol.symbol_type
            };
            if is_global {
                self.check_import_visibility(
                    "global variable",
                    &name,
                    var_symbol.pos.file_id,
                    pos,
                )?;
            }
            return Ok(Expression::new(ExprKind::Variable(name), expr_type, pos));
        }
        // Not a variable: a known function name becomes a function-pointer
        // value (`FunctionRef`). Capturing the signature now lets `&foo` and
        // bare `foo` flow through type-checking and codegen uniformly.
        let func_sig = self.module.lookup_function(&name).map(|f| {
            (
                f.params.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
                f.return_type,
                f.pos.file_id,
            )
        });
        if let Some((params, return_type, def_file_id)) = func_sig {
            self.check_import_visibility("function", &name, def_file_id, pos)?;
            let id = self.module.intern_fnptr(params, return_type);
            let ty = LangType::fnptr_type(id);
            return Ok(Expression::new(ExprKind::FunctionRef(name), ty, pos));
        }
        // Unknown name: stamp void; the type checker raises UndefinedVariable.
        Ok(Expression::new(
            ExprKind::Variable(name),
            LangType::VOID,
            pos,
        ))
    }

    /// Parse a type (including array types like u32[4])
    pub(crate) fn parse_type(&mut self) -> Result<LangType, ParserError> {
        let pos = self.peek().pos;
        let kind = self.peek().kind.clone();
        match kind {
            // Built-in types usually arrive pre-folded from the lexer
            // (`u8[10]*` is one token), but the scanner only folds `[N]` for
            // literal N — `u8[MAX_SIZE]` reaches us as `u8` `[` `1024` `]`
            // after define substitution, so the trailing modifiers must be
            // (re-)applied here too.
            TokenKind::LangType(lang_type) => {
                self.advance();
                Ok(self.apply_type_modifiers(lang_type))
            }
            // Named types: aliases and type-structs. The lexer leaves these as
            // bare identifiers (it cannot know the declared type names), so we
            // resolve them against the module table — enforcing import
            // visibility against each name's declaring file — and attach any
            // `*` pointer modifiers here (built-in types arrive pre-folded
            // from the lexer).
            TokenKind::Identifier(name) => {
                self.advance();
                let base = if let Some(info) = self.module.alias_info(&name) {
                    self.check_import_visibility("type alias", &name, info.file_id, pos)?;
                    // An alias is a name binding, not an export: aliasing a
                    // type-struct does not launder its module visibility, so
                    // the underlying struct is checked as if named directly.
                    if let TypeBase::Struct(id) = info.ty.base {
                        self.check_struct_visibility(id, pos)?;
                    }
                    info.ty
                } else if let Some(id) = self.module.struct_id(&name) {
                    self.check_struct_visibility(id, pos)?;
                    LangType::struct_type(id)
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
                let params = self.parse_comma_separated(&TokenKind::CloseParen, Self::parse_type)?;
                let return_type = if self.match_token(&[TokenKind::Arrow]) {
                    self.parse_type()?
                } else {
                    LangType::VOID
                };
                let id = self.module.intern_fnptr(params, return_type);
                let base = LangType::fnptr_type(id);
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
            self.type_suffix_then_ident(self.current + 1)
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
        // Optional type suffix, then the variable name must follow.
        self.type_suffix_then_ident(i)
    }

    /// Lookahead helper shared by the `starts_*_var_decl` predicates: from
    /// token index `i`, skip an optional `[N]` array suffix and any number of
    /// `*` pointer modifiers; `true` when an identifier follows. Does not
    /// consume tokens.
    fn type_suffix_then_ident(&self, mut i: usize) -> bool {
        let kind_at = |i: usize| self.tokens.get(i).map(|t| &t.kind);
        if matches!(kind_at(i), Some(TokenKind::OpenBracket))
            && matches!(kind_at(i + 1), Some(TokenKind::Integer(_)))
            && matches!(kind_at(i + 2), Some(TokenKind::CloseBracket))
        {
            i += 3;
        }
        while matches!(kind_at(i), Some(TokenKind::Asterisk)) {
            i += 1;
        }
        matches!(kind_at(i), Some(TokenKind::Identifier(_)))
    }

    // NOTE: parse_alloc is kept for backward compatibility with dynamic allocations
    // For preallocated arrays, use the type[size] syntax in variable declarations
    pub(crate) fn parse_alloc(&mut self) -> Result<Expression, ParserError> {
        let pos = self.peek().pos;
        match self.peek().kind {
            TokenKind::LangType(alloc_type) => {
                self.advance();
                // The scanner greedily folds `type[<digits>]` (e.g. `u8[256]`)
                // into a single `LangType` token carrying `array_size = Some(n)`,
                // so no separate `[` survives for the explicit path below. Accept
                // that folded spelling by synthesising the count from the folded
                // size. `u8[n]` and `u8[(256)]` never fold (the fold only fires on
                // bare digits), so they still take the explicit-bracket path.
                // Note this is stack (inside a function) or BSS (module scope)
                // allocation, not heap — see `generate_alloc`.
                let (elem_type, count_expr) = if let Some(n) = alloc_type.array_size {
                    (
                        LangType {
                            array_size: None,
                            ..alloc_type
                        },
                        Self::integer_literal(i64::from(n), pos),
                    )
                } else {
                    self.expect(&TokenKind::OpenBracket, "[")?;
                    let count_expr = self.parse_expression()?;
                    self.expect(&TokenKind::CloseBracket, "]")?;
                    (alloc_type, count_expr)
                };
                Ok(Expression::new(
                    ExprKind::Alloc {
                        alloc_type: elem_type,
                        count: Box::new(count_expr),
                    },
                    LangType {
                        base: elem_type.base,
                        size_bits: elem_type.size_bits,
                        pointer_depth: elem_type.pointer_depth + 1,
                        is_const: elem_type.is_const,
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

    /// Parse a `.ident` postfix — the `.` was already consumed. The followup
    /// distinguishes two forms:
    /// - `base.method(args)` → a method call, desugared to a `FunctionCall`
    ///   with the mangled name `Type$method` (autorefs value receivers; static
    ///   form `Type.method(...)` carries no receiver).
    /// - `base.field` → a `FieldAccess`, with a best-effort field-type stamp.
    fn parse_dot_postfix(&mut self, base: Expression) -> Result<Expression, ParserError> {
        let pos = base.pos;
        let name = self.parse_ident("field or method name")?;

        // Method call: `.ident(args)` — only when `ident` is actually a method
        // of the base's type. Otherwise (e.g. `.callback(` where `callback` is
        // a *field* of function-pointer type), drop through to field access
        // and let the postfix loop's `OpenParen` arm emit an indirect call.
        if self.check(&TokenKind::OpenParen) && self.identifier_is_method_of_base(&base, &name) {
            self.advance();
            let args = self.parse_comma_separated(&TokenKind::CloseParen, Self::parse_expression)?;
            return self.build_method_call(base, &name, args, pos);
        }

        // Static reference to a method as a function-pointer *value*:
        // `Type.method` with no following call (`base` names a known
        // type-struct, not shadowed by a local, and `name` is one of its
        // methods). Resolves to the method's mangled free function, typed
        // from that function's *actual* signature — whose first parameter is
        // already the receiver `Type*` — so the value is `fn(Type*, ...) -> R`
        // and drops straight into a fn-ptr variable or dispatch table, called
        // through the pointer with the receiver passed first. The bound form
        // (`instance.method` as a value, carrying a receiver) stays out of
        // scope and falls through to field access below.
        if let ExprKind::Variable(var_name) = &base.kind
            && let Some(id) = self.module.struct_id(var_name)
            && self.symbol_table.lookup_variable(var_name).is_none()
            && self.module.struct_info(id).methods.contains_key(&name)
        {
            self.check_struct_visibility(id, pos)?;
            let type_name = self.module.struct_info(id).name.clone();
            let mangled = crate::symbol::module::mangle_method(&type_name, &name);
            let (params, return_type) = self.module.lookup_function(&mangled).map_or_else(
                || (Vec::new(), LangType::VOID),
                |f| {
                    (
                        f.params.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
                        f.return_type,
                    )
                },
            );
            let fnptr_id = self.module.intern_fnptr(params, return_type);
            let ty = LangType::fnptr_type(fnptr_id);
            return Ok(Expression::new(ExprKind::FunctionRef(mangled), ty, pos));
        }

        // Field access.
        let field_type = match base.expr_type.base {
            TypeBase::Struct(id) => self
                .module
                .field(id, &name)
                .map_or_else(|| LangType::VOID, |(_, f)| f.ty),
            _ => LangType::VOID,
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
            self.check_struct_visibility(id, pos)?;
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
            let mangled = crate::symbol::module::mangle_method(&type_name, method_name);
            let return_type = self.module.lookup_function(&mangled).map_or_else(
                || LangType::VOID,
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
        // A private type's methods are at most module-visible, however
        // `public` the member itself is — so instance calls are gated on the
        // type's module visibility exactly like naming it.
        self.check_struct_visibility(id, pos)?;
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
        let mangled = crate::symbol::module::mangle_method(&type_name, method_name);
        let return_type = self.module.lookup_function(&mangled).map_or_else(
            || LangType::VOID,
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

        let expr_type = LangType::struct_type(struct_id);
        Ok(Expression::new(
            ExprKind::StructLiteral { struct_id, fields },
            expr_type,
            pos,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// A parser over an empty token stream with a three-file module
    /// registry: file 0 is the anonymous root module `""` (imports `mid`),
    /// file 1 is `mid` (imports `hidden`), file 2 is `hidden` (imports
    /// nothing).
    fn parser_with_modules() -> Parser {
        let modules = vec![
            (0, String::new()),
            (1, "mid".to_string()),
            (2, "hidden".to_string()),
        ];
        let imports = HashMap::from([
            (String::new(), vec!["mid".to_string()]),
            ("mid".to_string(), vec!["hidden".to_string()]),
            ("hidden".to_string(), Vec::new()),
        ]);
        let eof = Token::new(TokenKind::Eof, Position::new(0, 0), String::new());
        Parser::new(vec![eof]).with_module_info(modules, imports)
    }

    /// A use-site position inside the file with `file_id`.
    fn site(file_id: u32) -> Position {
        Position::with_file(3, 7, file_id)
    }

    #[test]
    fn same_module_references_are_always_visible() {
        let p = parser_with_modules();
        for file in 0..3 {
            assert!(p
                .check_import_visibility("function", "f", file, site(file))
                .is_ok());
        }
    }

    #[test]
    fn directly_imported_modules_are_visible() {
        let p = parser_with_modules();
        // The root imports `mid`; `mid` imports `hidden`.
        assert!(p
            .check_import_visibility("function", "f", 1, site(0))
            .is_ok());
        assert!(p
            .check_import_visibility("function", "f", 2, site(1))
            .is_ok());
    }

    #[test]
    fn transitive_imports_are_not_visible() {
        let p = parser_with_modules();
        let err = p
            .check_import_visibility("function", "gcd_u64", 2, site(0))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "function 'gcd_u64' is defined in module 'hidden', \
             which the root module does not import at 3:7"
        );
        assert_eq!(err.position(), Some(site(0)));
    }

    #[test]
    fn nothing_imports_the_root_module() {
        let p = parser_with_modules();
        let err = p
            .check_import_visibility("global variable", "counter", 0, site(1))
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "global variable 'counter' is defined in the root module, \
             which module 'mid' does not import at 3:7"
        );
    }

    #[test]
    fn importing_does_not_grant_visibility_in_reverse() {
        let p = parser_with_modules();
        // `mid` imports `hidden` — `hidden` must not see `mid`.
        assert!(p
            .check_import_visibility("function", "f", 1, site(2))
            .is_err());
    }

    #[test]
    fn without_module_info_every_file_is_the_root_module() {
        let eof = Token::new(TokenKind::Eof, Position::new(0, 0), String::new());
        let p = Parser::new(vec![eof]);
        assert!(p
            .check_import_visibility("function", "f", 4, site(9))
            .is_ok());
    }

    #[test]
    fn private_type_is_module_visible_only() {
        use crate::symbol::module::Visibility;
        let mut p = parser_with_modules();
        let id = p.module.intern_struct("Secret", 1, Visibility::Private);
        // Inside its own module the type is freely usable.
        assert!(p.check_struct_visibility(id, site(1)).is_ok());
        // From an importing module, privacy blocks it.
        let err = p.check_struct_visibility(id, site(0)).unwrap_err();
        assert_eq!(
            err.to_string(),
            "type-struct 'Secret' is private to module 'mid' and cannot be used from \
             the root module — declare it `public type` to export it at 3:7"
        );
        assert_eq!(err.position(), Some(site(0)));
    }

    #[test]
    fn public_type_is_visible_to_importers_only() {
        use crate::symbol::module::Visibility;
        let mut p = parser_with_modules();
        let id = p.module.intern_struct("Pair", 1, Visibility::Public);
        // The root imports `mid`, so the exported type is visible there.
        assert!(p.check_struct_visibility(id, site(0)).is_ok());
        // `public` does not bypass the import rule: `hidden` does not import `mid`.
        assert!(p.check_struct_visibility(id, site(2)).is_err());
    }
}
