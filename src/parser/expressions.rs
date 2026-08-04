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
    /// Module of each file, indexed by `Position::file_id`. Files without an
    /// entry — including everything when no module info was threaded — belong
    /// to the anonymous root module `""`. Moved into `Program::file_modules` so
    /// the `meta` query layer can resolve a position to its module.
    pub(crate) file_modules: Vec<String>,
    /// Module → its *direct* imports, driving the import-visibility check.
    pub(crate) module_imports: std::collections::HashMap<String, Vec<String>>,
    /// Global-variable name → module visibility: globals live in the outermost
    /// variable scope, whose `Symbol` carries no visibility, so the
    /// reference-site gate reads it here.
    pub(crate) global_vis: std::collections::HashMap<String, crate::symbol::module::Visibility>,
    /// Declaration position per type-struct/sum id, recorded when the body
    /// parses — the by-value-containment cycle check reports here, since the
    /// registry itself stores no positions.
    pub(crate) struct_decl_pos: std::collections::HashMap<u32, Position>,
    pub(crate) sum_decl_pos: std::collections::HashMap<u32, Position>,
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
            global_vis: std::collections::HashMap::new(),
            struct_decl_pos: std::collections::HashMap::new(),
            sum_decl_pos: std::collections::HashMap::new(),
        }
    }

    /// Set the full source-file registry from the preprocessor — entry file
    /// at id 0, each `$import`-pulled file at the next ids. Error formatting
    /// uses each error's `pos.file_id` to look up the right filename here.
    #[must_use]
    pub fn with_source_files(mut self, files: Vec<std::path::PathBuf>) -> Self {
        self.source_files = files;
        self
    }

    /// Enables the import-visibility check. Without this, every file belongs to
    /// the anonymous root module `""` and every reference is same-module.
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

    /// Format a single error prefixed with the source file the error came
    /// from (resolved via `pos.file_id`) and its line/column.
    #[must_use]
    pub fn format_error(&self, err: &ParserError) -> String {
        crate::lexer::format_diagnostic(&self.source_files, err, err.position())
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

    pub fn symbol_table_mut(&mut self) -> &mut SymbolTable {
        &mut self.symbol_table
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

        loop {
            // `is` sits at the comparison tier (3), left-associative like the
            // rest — `a is B is C` folds left and fails on the bool scrutinee.
            if self.check_keyword(&Keyword::Is) && min_prec <= 3 {
                self.advance();
                left = self.parse_is_suffix(left)?;
                continue;
            }
            let Some((op, prec, right_assoc)) = INFIX_OPS
                .iter()
                .find(|e| self.check(&e.token) && e.prec >= min_prec)
                .map(|entry| (entry.op, entry.prec, entry.right_assoc))
            else {
                break;
            };
            self.advance();
            let next_min = if right_assoc { prec } else { prec + 1 };
            let right = self.parse_expr_prec(next_min)?;
            let pos = left.pos;

            // A binding `is` may not sit under `||` — "matched" and "binding
            // readable" diverge there. Direct and `&&`-nested placements both
            // arrive here; parens were already policed at the primary.
            if matches!(op, OpKind::Binary(BinaryOp::LogicalOr))
                && let Some(bad) = Self::find_is_binding(&left).or_else(|| Self::find_is_binding(&right))
            {
                return Err(ParserError::UnexpectedToken(
                    "a binding `is` is not an expression — write it as an unparenthesized `&&`-conjunct of an if/elif/while condition (`||` cannot guarantee the binding matched)"
                        .to_string(),
                    bad,
                ));
            }

            left = match op {
                OpKind::Binary(bop) => {
                    // Naive result type: logical `&&`/`||` are a boolean
                    // regardless of operand type; otherwise the pointer operand
                    // wins (pointer arithmetic), falling back to the left
                    // operand's type. The checker recomputes via `wider_type`.
                    let result_type = if matches!(bop, BinaryOp::LogicalAnd | BinaryOp::LogicalOr) {
                        LangType::BOOL
                    } else if left.expr_type.pointer_depth == 0
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
                    // A comparison locally yields a boolean.
                    Expression::new(
                        ExprKind::Comparison {
                            left: Box::new(left),
                            op: cop,
                            right: Box::new(right),
                        },
                        LangType::BOOL,
                        pos,
                    )
                }
            };
        }

        Ok(left)
    }

    /// `left is Sum.Variant` / `left is Sum.Variant(a, _, b)` — the `is`
    /// keyword is already consumed, and qualification is mandatory (patterns
    /// spell the type like every other variant access). Bare variants build
    /// the binding-free bool expression; a parenthesized pattern builds the
    /// condition-restricted binding form, registering its binders into the
    /// current parse-time scope in textual order (which is what lets later
    /// `&&`-conjuncts and the success block reference them). A single-level
    /// pointer scrutinee auto-derefs, like field access.
    fn parse_is_suffix(&mut self, scrutinee: Expression) -> Result<Expression, ParserError> {
        let pos = scrutinee.pos;
        let pat_pos = self.peek().pos;
        let s_ty = scrutinee.expr_type;

        let sum_id = if s_ty.pointer_depth <= 1 && !s_ty.is_array() {
            match s_ty.base {
                TypeBase::Sum(id) => id,
                TypeBase::Enum(_) if s_ty.pointer_depth == 0 => {
                    return Err(ParserError::UnexpectedToken(
                        "`is` does not apply to enums — compare with `==` against `Enum.Variant`"
                            .to_string(),
                        pat_pos,
                    ));
                }
                _ => {
                    return Err(ParserError::UnexpectedToken(
                        "`is` probes a sum value — the scrutinee is not a sum (or a single-level pointer to one)"
                            .to_string(),
                        pat_pos,
                    ));
                }
            }
        } else {
            return Err(ParserError::UnexpectedToken(
                "`is` probes a sum value — the scrutinee is not a sum (or a single-level pointer to one)"
                    .to_string(),
                pat_pos,
            ));
        };

        let sum_name = self.module.sum_info(sum_id).name.clone();
        let head = self.parse_ident("variant pattern after `is`")?;
        if head == "_" {
            return Err(ParserError::UnexpectedToken(
                format!("sum '{sum_name}' has no variant '_' — `is` probes one named variant"),
                pat_pos,
            ));
        }
        if head != sum_name {
            if self.module.sum_variant_index(sum_id, &head).is_some() {
                return Err(ParserError::UnexpectedToken(
                    format!("variant patterns are qualified — write `{sum_name}.{head}`"),
                    pat_pos,
                ));
            }
            return Err(ParserError::UnknownSumVariant {
                sum_name,
                variant: head,
                pos: pat_pos,
            });
        }
        self.expect(&TokenKind::Dot, ".")?;
        let variant_name = self.parse_ident("variant name")?;
        let Some(idx) = self.module.sum_variant_index(sum_id, &variant_name) else {
            return Err(ParserError::UnknownSumVariant {
                sum_name,
                variant: variant_name,
                pos: pat_pos,
            });
        };
        let variant = u32::try_from(idx).expect("variant index fits u32");
        let field_types: Vec<LangType> = self.module.sum_info(sum_id).variants[idx]
            .fields
            .iter()
            .map(|(_, ty)| *ty)
            .collect();

        if !self.match_token(&[TokenKind::OpenParen]) {
            return Ok(Expression::new(
                ExprKind::Is {
                    scrutinee: Box::new(scrutinee),
                    sum_id,
                    variant,
                },
                LangType::BOOL,
                pos,
            ));
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
                pat_pos,
            ));
        }
        let binders: Vec<Option<(String, LangType)>> = names
            .into_iter()
            .zip(field_types)
            .map(|(n, ty)| if n == "_" { None } else { Some((n, ty)) })
            .collect();
        for (name, ty) in binders.iter().flatten() {
            self.symbol_table_mut()
                .add_variable(name.clone(), *ty, pat_pos)
                .map_err(|e| ParserError::from_symbol(e, pat_pos))?;
        }
        Ok(Expression::new(
            ExprKind::IsBinding {
                scrutinee: Box::new(scrutinee),
                sum_id,
                variant,
                binders,
            },
            LangType::BOOL,
            pos,
        ))
    }

    /// Position of a binding `is` anywhere in `expr`'s `&&`/comparison
    /// spine, if one exists. Used to police `||` placement — parens and
    /// non-condition contexts are policed elsewhere.
    fn find_is_binding(expr: &Expression) -> Option<Position> {
        match &expr.kind {
            ExprKind::IsBinding { .. } => Some(expr.pos),
            ExprKind::Binary { left, right, .. } => {
                Self::find_is_binding(left).or_else(|| Self::find_is_binding(right))
            }
            ExprKind::Comparison { left, right, .. } => {
                Self::find_is_binding(left).or_else(|| Self::find_is_binding(right))
            }
            _ => None,
        }
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

    pub(crate) fn parse_unary(&mut self) -> Result<Expression, ParserError> {
        let pos = self.peek().pos;

        match &self.peek().kind {
            TokenKind::Ampersand => self.parse_reference(pos),
            TokenKind::Asterisk => self.parse_dereference(pos),
            TokenKind::Minus => self.parse_negation(pos),
            TokenKind::LogicalNot => self.parse_logical_not(pos),
            TokenKind::Tilde => self.parse_bitwise_not(pos),
            _ => self.parse_postfix(),
        }
    }

    fn parse_reference(&mut self, pos: Position) -> Result<Expression, ParserError> {
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

    fn parse_dereference(&mut self, pos: Position) -> Result<Expression, ParserError> {
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

    fn parse_negation(&mut self, pos: Position) -> Result<Expression, ParserError> {
        self.advance();
        let expr = self.parse_unary()?;

        // Fold negation into a numeric literal so `-128` becomes one
        // node, letting it coerce to narrow signed types (i8) uncast.
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

    fn parse_logical_not(&mut self, pos: Position) -> Result<Expression, ParserError> {
        self.advance();
        let expr = self.parse_unary()?;
        let result_type = LangType::BOOL;

        Ok(Expression::new(
            ExprKind::UnaryNot(Box::new(expr)),
            result_type,
            pos,
        ))
    }

    fn parse_bitwise_not(&mut self, pos: Position) -> Result<Expression, ParserError> {
        self.advance();
        let expr = self.parse_unary()?;
        let result_type = expr.expr_type;

        Ok(Expression::new(
            ExprKind::BitwiseNot(Box::new(expr)),
            result_type,
            pos,
        ))
    }

    /// Loops so chained operations like `arr[i][j]` or `f()()` parse correctly.
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
            let def_file_id = func_symbol.pos.file_id;
            let def_vis = func_symbol.vis;
            // Defense in depth: `variable_reference` already vetted the ref.
            self.check_name_visibility("function", &func_name, def_file_id, def_vis, pos)?;

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
            // `null` stamps `u8*` as a placeholder (any single-pointer type
            // would do — coercion is structural by depth). The checker upgrades
            // it to the contextual target in `check` mode.
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
                // Parens flip a binding `is` into expression position — the
                // C-muscle-memory spelling `if (s is Circle(r)) {` must not
                // silently change meaning, so it errors with the fix.
                if matches!(expr.kind, ExprKind::IsBinding { .. }) {
                    return Err(ParserError::UnexpectedToken(
                        "a binding `is` is not an expression — write it as an unparenthesized `&&`-conjunct of an if/elif/while condition"
                            .to_string(),
                        expr.pos,
                    ));
                }
                Ok(expr)
            }
            // A brace expression: list initializer (`{1, 2, 3}`) or
            // value-block (`{ ...; return v }`) — see `parse_brace_expression`.
            TokenKind::OpenBrace => self.parse_brace_expression(),

            _ => Err(ParserError::ExpectedExpression(pos)),
        }
    }

    /// Chooses the smallest signed type that fits.
    fn integer_literal(value: i64, pos: Position) -> Expression {
        let expr_type = if value >= i32::MIN as i64 && value <= i32::MAX as i64 {
            LangType::I32
        } else {
            LangType::I64
        };
        Expression::new(ExprKind::Literal(LiteralValue::Integer(value)), expr_type, pos)
    }

    fn float_literal(value: f64, pos: Position) -> Expression {
        let expr_type = LangType::F64;
        Expression::new(ExprKind::Literal(LiteralValue::Float(value)), expr_type, pos)
    }

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
                let def_file_id = var_symbol.pos.file_id;
                let gvis = self
                    .global_vis
                    .get(&name)
                    .copied()
                    .unwrap_or(crate::symbol::module::Visibility::Private);
                self.check_name_visibility("global variable", &name, def_file_id, gvis, pos)?;
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
                f.vis,
            )
        });
        if let Some((params, return_type, def_file_id, def_vis)) = func_sig {
            self.check_name_visibility("function", &name, def_file_id, def_vis, pos)?;
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

    // NOTE: parse_alloc is kept for backward compatibility with dynamic allocations
    // For preallocated arrays, use the type[size] syntax in variable declarations
    pub(crate) fn parse_alloc(&mut self) -> Result<Expression, ParserError> {
        let pos = self.peek().pos;
        match self.peek().kind {
            TokenKind::LangType(alloc_type) => {
                self.advance();
                // The scanner folds `type[<digits>]` (`u8[256]`) into one token
                // carrying `array_size`; synthesise the count from it. `u8[n]`
                // and `u8[(256)]` don't fold and take the explicit-bracket path.
                // This is stack/BSS allocation, not heap — see `generate_alloc`.
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

