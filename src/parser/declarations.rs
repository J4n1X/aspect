use crate::lexer::{Keyword, LangType, TokenKind, TypeBase};
use crate::parser::expressions::Parser;
use crate::parser::{ExprKind, Expression, ParserError};
use crate::symbol::module::Visibility;
use aspect_macros::parse_rule;

impl Parser {
    /// Parse a top-level type-struct definition:
    /// `type Name { [public] Type field <term> ...  [const?] fn method(...) {...} ... }`.
    ///
    /// Fields must come before methods. Methods are desugared into mangled
    /// free functions (`Type$method`) and returned to `do_parse_program` for
    /// inclusion in `Program::functions`.
    #[parse_rule]
    pub(crate) fn parse_struct_def(&mut self) -> Result<Vec<crate::parser::Function>, ParserError> {
        use crate::symbol::module::FieldInfo;

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

            // Optional `public` prefix — shared by fields and methods. Absence
            // means private for both (encapsulation by default).
            let vis = if kw_if!(Public) {
                Visibility::Public
            } else {
                Visibility::Private
            };

            // Method vs field. A method is `[const] fn IDENT (...)`; a
            // function-pointer *field* type is `fn (...)`. They are told apart
            // by lookahead (`fn` followed by a name vs `(`), so a `public
            // fn(i32) -> i32 cb` field is not mistaken for a method.
            if self.upcoming_is_method() {
                if !fields_set {
                    self.module.set_fields(id, std::mem::take(&mut fields));
                    fields_set = true;
                }
                let is_const_fn = self.check_keyword(&Keyword::Const);
                if is_const_fn {
                    self.advance(); // consume `const`
                    skip_nl!();
                }
                let method = self.parse_method(id, &name, is_const_fn, vis)?;
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

    /// Lookahead from the current position: does a method declaration start
    /// here? A method is `[const] fn IDENT (`; a function-pointer field type is
    /// `fn (`, so the token after `fn` — an identifier vs `(` — discriminates.
    /// Any `public` prefix has already been consumed by the caller.
    fn upcoming_is_method(&self) -> bool {
        let mut i = self.current;
        let kind_at = |idx: usize| self.tokens.get(idx).map(|t| &t.kind);
        // Optional leading `const`.
        if matches!(kind_at(i), Some(TokenKind::Keyword(Keyword::Const))) {
            i += 1;
        }
        // Must be `fn` followed by the method name (not `(`, which begins a
        // function-pointer type used as a field).
        if !matches!(kind_at(i), Some(TokenKind::Keyword(Keyword::Fn))) {
            return false;
        }
        matches!(kind_at(i + 1), Some(TokenKind::Identifier(_)))
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
        vis: crate::symbol::module::Visibility,
    ) -> Result<crate::parser::Function, ParserError> {
        use crate::parser::{Function, FunctionProto};
        use crate::symbol::module::{mangle_method, MethodSig};
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

        params.extend(self.parse_comma_separated(&TokenKind::CloseParen, |p| {
            let param_type = p.parse_type()?;
            let param_name = p.parse_ident("parameter name")?;
            Ok((param_type, param_name))
        })?);

        if is_const_fn && !has_this {
            return Err(ParserError::UnexpectedToken(
                "`const fn` requires a `this` receiver".to_string(),
                pos,
            ));
        }

        let return_type = if self.match_token(&[TokenKind::Arrow]) {
            lang_type!()
        } else {
            LangType::VOID
        };

        let mangled = mangle_method(struct_name, &method_name);

        let proto = FunctionProto {
            name: mangled.clone(),
            params: params.clone(),
            return_type,
            // A method's `public` governs access through its type, not object
            // -file export; nothing outside Aspect calls a mangled method.
            vis: Visibility::Private,
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
                vis,
            },
        );

        skip_nl!();

        // Same deferral as free functions: methods can call anything declared
        // anywhere in the file, including later methods of the same type.
        self.defer_function_body(proto.name.clone(), params, pos)?;

        Ok(Function {
            proto,
            body: crate::parser::FunctionBody::Aspect(Vec::new()),
        })
    }

    /// `true` when `name` is a method of `base`'s type (instance form) or of
    /// the type whose name `base` resolves to (static form). Used to decide
    /// between method-call dispatch and field-access in `parse_dot_postfix`.
    pub(crate) fn identifier_is_method_of_base(&self, base: &Expression, name: &str) -> bool {
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

    /// Reject a parameter list that names the same parameter twice.
    ///
    /// A fn *with* a body catches this in pass 2, when the body's scope
    /// declares each parameter and the second declaration collides. The
    /// bodyless forms — `extern fn` and `asm fn` — never open that scope, so
    /// without this check they silently accept `f(i64 a, i64 a)`. Checking the
    /// proto directly covers all three forms at their declaration site; the
    /// error matches pass 2's spelling and position so a duplicate reports
    /// identically no matter which form it appears in.
    pub(crate) fn check_duplicate_params(
        params: &[(LangType, String)],
        pos: crate::lexer::Position,
    ) -> Result<(), ParserError> {
        let mut seen: Vec<&str> = Vec::with_capacity(params.len());
        for (_, name) in params {
            if seen.contains(&name.as_str()) {
                return Err(ParserError::DuplicateDeclaration(name.clone(), pos));
            }
            seen.push(name);
        }
        Ok(())
    }

    #[parse_rule]
    pub(crate) fn parse_function(
        &mut self,
        is_extern: bool,
        vis: Visibility,
    ) -> Result<crate::parser::Function, ParserError> {
        use crate::parser::{Function, FunctionProto};
        use crate::symbol::table::FunctionSymbol;

        let pos = pos!();
        kw!(Fn);
        let name = ident!();
        token!(OpenParen);

        let params = self.parse_comma_separated(&TokenKind::CloseParen, |p| {
            let param_type = p.parse_type()?;
            let param_name = p.parse_ident("parameter name")?;
            Ok((param_type, param_name))
        })?;
        Self::check_duplicate_params(&params, pos)?;

        let return_type = if self.match_token(&[TokenKind::Arrow]) {
            lang_type!()
        } else {
            LangType::VOID
        };

        let proto = FunctionProto {
            name: name.clone(),
            params: params.clone(),
            return_type,
            vis,
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
            crate::parser::FunctionBody::Extern
        } else {
            // Body parsing is deferred to pass 2 (see `do_parse_program`) so
            // functions can call others defined later in the file.
            self.defer_function_body(name, params, pos)?;
            crate::parser::FunctionBody::Aspect(Vec::new())
        };

        Ok(Function { proto, body })
    }

    #[parse_rule]
    pub(crate) fn parse_global_var(&mut self, vis: Visibility) -> Result<crate::parser::GlobalVar, ParserError> {
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
            vis
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
            LangType::VOID,
            pos,
        ))
    }

    /// Disambiguate a bare `{` in expression position.
    ///
    /// A brace expression that parses as a comma-separated expression list
    /// *is* a list initializer (`{1, 2, 3}`, `{x}`, `{}`). Anything else —
    /// statement terminators, declarations, `return` — fails the list parse
    /// and is re-parsed as a **value-block** (`{ ...; return v }`). The two
    /// grammars cannot both accept one input: a valid value-block must
    /// contain a `return`, which can never appear in a valid list.
    ///
    /// Speculation is safe here for the same reason as `parse_cast_or_alloc`:
    /// expression parsing has no side effects beyond interned string
    /// literals, which are rolled back by truncation.
    pub(crate) fn parse_brace_expression(&mut self) -> Result<Expression, ParserError> {
        let saved = self.current;
        let saved_strlits = self.string_literals.len();
        match self.parse_init_list() {
            Ok(list) => Ok(list),
            Err(list_err) => {
                let list_at = self.current;
                self.current = saved;
                self.string_literals.truncate(saved_strlits);
                self.parse_value_block().map_err(|block_err| {
                    // Two failed readings: report the one that got further —
                    // it is almost always the one the user meant.
                    if list_at > self.current {
                        self.current = list_at;
                        list_err
                    } else {
                        block_err
                    }
                })
            }
        }
    }

    /// Parse a value-block: `{ stmt* }` as an expression. The opening brace
    /// has not yet been consumed. Statements are parsed with the regular
    /// statement grammar in a fresh variable scope; errors propagate (no
    /// `sync!` recovery — inside an expression there is no safe resync
    /// point). The expression type is a `void` placeholder; the type
    /// checker resolves it from the block's `return` statements.
    #[parse_rule]
    fn parse_value_block(&mut self) -> Result<Expression, ParserError> {
        let pos = pos!();
        token!(OpenBrace);
        let statements = scoped!({
            let mut stmts = Vec::new();
            loop {
                skip_nl!();
                if self.check(&TokenKind::CloseBrace) || self.is_at_end() {
                    break;
                }
                stmts.push(self.parse_statement()?);
            }
            stmts
        });
        token!(CloseBrace);
        Ok(Expression::new(
            ExprKind::ValueBlock(statements),
            LangType::VOID,
            pos,
        ))
    }
}
