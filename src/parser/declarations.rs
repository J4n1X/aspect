use crate::lexer::{Keyword, LangType, Position, TokenKind, TypeBase};
use crate::parser::expressions::Parser;
use crate::parser::{ExprKind, Expression, Function, GlobalVar, ParserError};
use crate::symbol::ids::{StructId, TypeDefId};
use crate::symbol::module::{ModuleSymbols, Visibility};
use aspect_macros::parse_rule;

/// Outcome of parsing one top-level declaration: the free functions/methods it
/// defined (a struct def can yield several methods at once), the single global
/// variable it defined, or neither (alias/enum/sum definitions add only to the
/// symbol table).
pub(crate) enum TopLevelItem {
    Fns(Vec<Function>),
    Global(GlobalVar),
    None,
}

/// The top-level declaration forms. Classifying once — instead of deriving a
/// predicate per modifier rule and again per dispatch arm — is what makes
/// modifier legality a table below, so a new form cannot silently skip a rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeclForm {
    AsmFn,
    NakedFn,
    Fn,
    Alias,
    Type,
    Enum,
    Sum,
    Global,
}

impl DeclForm {
    /// `public` is module visibility; an alias has no symbol to import.
    fn accepts_public(self) -> bool {
        self != Self::Alias
    }

    /// `export` needs a linked object-file symbol — only fns and globals have one.
    fn accepts_export(self) -> bool {
        matches!(self, Self::AsmFn | Self::NakedFn | Self::Fn | Self::Global)
    }

    /// Only a plain `fn` can live in another object file; `parse_kind_modifier`
    /// has already rejected `extern` alongside `asm`/`naked`.
    fn accepts_extern(self) -> bool {
        self == Self::Fn
    }
}

impl Parser {
    /// Which form the upcoming tokens begin, or `None` when nothing does. The
    /// order is load-bearing: `fn ident(` is a definition while `fn(` is a
    /// function-pointer-typed global, and the keyword forms must be tried before
    /// the catch-all global shapes. Does not consume tokens.
    fn classify_decl_form(&self, kind: Option<&(Keyword, Position)>) -> Option<DeclForm> {
        match kind {
            Some((Keyword::Asm, _)) => return Some(DeclForm::AsmFn),
            Some((Keyword::Naked, _)) => return Some(DeclForm::NakedFn),
            _ => {}
        }
        if self.check_keyword(&Keyword::Fn) && !self.starts_fnptr_var_decl() {
            return Some(DeclForm::Fn);
        }
        for (kw, form) in [
            (Keyword::Alias, DeclForm::Alias),
            (Keyword::Type, DeclForm::Type),
            (Keyword::Enum, DeclForm::Enum),
            (Keyword::Sum, DeclForm::Sum),
        ] {
            if self.check_keyword(&kw) {
                return Some(form);
            }
        }
        // A leading built-in type, named type (alias / type-struct), function-
        // pointer type, parenthesised group, or `const` over a named base begins
        // a global. A bare `const` keyword survives the scanner only for
        // non-scalar bases.
        if matches!(
            self.peek().kind,
            TokenKind::LangType(_) | TokenKind::Identifier(_)
        ) || self.starts_fnptr_var_decl()
            || self.starts_grouped_var_decl()
            || self.check_keyword(&Keyword::Const)
        {
            return Some(DeclForm::Global);
        }
        None
    }

    /// Classify, validate, and dispatch one top-level declaration — the body
    /// of `do_parse_program`'s loop. `kind`/`vis_pos` come from the modifier
    /// scan the caller already ran.
    pub(crate) fn parse_top_level_item(
        &mut self,
        vis: Visibility,
        export: bool,
        is_extern: bool,
        kind: Option<(Keyword, Position)>,
        vis_pos: Position,
    ) -> Result<TopLevelItem, ParserError> {
        // `extern` may be `public` (nameable from importers) but never
        // `export`: there is no local symbol here to give external linkage.
        if is_extern && export {
            return Err(ParserError::UnexpectedToken(
                "extern functions cannot be exported — they are defined elsewhere, so there is no local symbol to give external linkage".to_string(),
                vis_pos,
            ));
        }

        let form_pos = self.peek().pos;
        let Some(form) = self.classify_decl_form(kind.as_ref()) else {
            return Err(ParserError::UnexpectedToken(
                format!("{}", self.peek().kind),
                form_pos,
            ));
        };

        if vis == Visibility::Public && !form.accepts_public() {
            return Err(ParserError::UnexpectedToken(
                "public can only be used with functions, global variables, or type definitions"
                    .to_string(),
                vis_pos,
            ));
        }
        if export && !form.accepts_export() {
            return Err(ParserError::UnexpectedToken(
                "export can only be used with functions or global variables — a type, enum, sum or alias has no linked symbol"
                    .to_string(),
                vis_pos,
            ));
        }
        if is_extern && !form.accepts_extern() {
            return Err(ParserError::UnexpectedToken(
                "extern can only be used with functions".to_string(),
                form_pos,
            ));
        }

        let kind_pos = kind.map(|(_, pos)| pos);
        match form {
            DeclForm::AsmFn => {
                let pos = kind_pos.expect("asm form comes from the kind modifier");
                Ok(TopLevelItem::Fns(vec![self.parse_asm_function(pos, vis, export)?]))
            }
            DeclForm::NakedFn => {
                let pos = kind_pos.expect("naked form comes from the kind modifier");
                Ok(TopLevelItem::Fns(vec![
                    self.parse_naked_function(pos, vis, export)?,
                ]))
            }
            DeclForm::Fn => Ok(TopLevelItem::Fns(vec![
                self.parse_function(is_extern, vis, export)?,
            ])),
            DeclForm::Alias => {
                self.parse_type_alias()?;
                Ok(TopLevelItem::None)
            }
            DeclForm::Type => Ok(TopLevelItem::Fns(self.parse_struct_def()?)),
            DeclForm::Enum => {
                self.parse_enum_def()?;
                Ok(TopLevelItem::None)
            }
            DeclForm::Sum => {
                self.parse_sum_def()?;
                Ok(TopLevelItem::None)
            }
            DeclForm::Global => Ok(TopLevelItem::Global(self.parse_global_var(vis, export)?)),
        }
    }

    /// Register a function in the module symbol table, mapping a duplicate or
    /// signature clash to a positioned error. `has_body` is `!is_extern` (only
    /// `extern` declarations lack a body); shared by the fn/asm/naked/method
    /// parsers.
    pub(crate) fn register_fn_symbol(
        &mut self,
        name: &str,
        params: &[(LangType, String)],
        return_type: LangType,
        is_extern: bool,
        vis: Visibility,
        pos: crate::lexer::Position,
    ) -> Result<(), ParserError> {
        self.module
            .add_function(crate::symbol::table::FunctionSymbol {
                name: name.to_string(),
                params: params.to_vec(),
                return_type,
                is_extern,
                has_body: !is_extern,
                vis,
                pos,
            })
            .map_err(|e| ParserError::from_symbol(e, pos))
    }

    /// A body may only be parsed against a prescan-reserved def of the matching
    /// kind that nothing has defined yet; either failure is the same
    /// duplicate-type error, reported at this body.
    ///
    /// `prove` is the registry's kind-checked id conversion.
    fn claim_type_decl<I>(
        &mut self,
        name: &str,
        pos: Position,
        prove: fn(&ModuleSymbols, TypeDefId) -> Option<I>,
    ) -> Result<I, ParserError> {
        let def = self
            .module
            .lookup_type(name)
            .expect("type name reserved during prescan");
        let (id, defined) = (def.id, def.defined);
        let duplicate = || ParserError::DuplicateType(name.to_string(), pos);
        if defined {
            return Err(duplicate());
        }
        prove(&self.module, id).ok_or_else(duplicate)
    }

    /// `type Name { [public] Type field ... [const?] fn method(...) {...} ... }`.
    ///
    /// Fields must come before methods. Methods are desugared into mangled free
    /// functions (`Type$method`) and returned to `do_parse_program`.
    #[parse_rule]
    pub(crate) fn parse_struct_def(&mut self) -> Result<Vec<crate::parser::Function>, ParserError> {
        use crate::symbol::module::FieldInfo;

        let pos = pos!();
        kw!(Type);
        let name = ident!();
        let id = self.claim_type_decl(&name, pos, ModuleSymbols::as_struct)?;

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

            // Method vs field by lookahead: a method is `[const] fn IDENT (`,
            // a function-pointer *field* type is `fn (`.
            if self.upcoming_is_method() {
                if !fields_set {
                    self.module.set_fields(id.def(), std::mem::take(&mut fields));
                    fields_set = true;
                }
                let is_const_fn = self.check_keyword(&Keyword::Const);
                if is_const_fn {
                    self.advance();
                    skip_nl!();
                }
                let method = self.parse_method(id, &name, is_const_fn, vis)?;
                methods.push(method);
                continue;
            }

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
            self.module.set_fields(id.def(), fields);
        }

        Ok(methods)
    }

    /// `enum Name { V1, V2, ... }`. Variants are comma- and/or newline-separated
    /// identifiers, each assigned its declaration-order index as its value. Like
    /// an alias, an enum has no AST node — only a symbol-table entry.
    #[parse_rule]
    pub(crate) fn parse_enum_def(&mut self) -> Result<(), ParserError> {
        let pos = pos!();
        kw!(Enum);
        let name = ident!();
        let id = self.claim_type_decl(&name, pos, ModuleSymbols::as_enum)?;

        token!(OpenBrace);

        let mut variants: Vec<String> = Vec::new();
        loop {
            skip_nl!();
            if self.check(&TokenKind::CloseBrace) || self.is_at_end() {
                break;
            }
            let variant = ident!();
            if variants.iter().any(|v| v == &variant) {
                return Err(ParserError::DuplicateDeclaration(variant, pos));
            }
            variants.push(variant);
            skip_nl!();
            self.match_token(&[TokenKind::Comma]);
        }
        // An enum with no variants is uninhabited, so it is rejected. Checked
        // before the closing brace so the diagnostic points at the `}`.
        if variants.is_empty() {
            return Err(ParserError::ExpectedToken(
                "at least one enum variant".to_string(),
                format!("{}", self.peek().kind),
                pos,
            ));
        }
        token!(CloseBrace);

        self.module.set_enum_variants(id.def(), variants);
        Ok(())
    }

    /// `sum Name { Variant(T field, ...) ... }`. One variant per line; a
    /// payload-less variant is a bare name (no parens); payload fields are
    /// named, parameter-style. Like an enum, a sum has no AST node — only a
    /// symbol-table entry (construction and `switch` observe it later).
    #[parse_rule]
    pub(crate) fn parse_sum_def(&mut self) -> Result<(), ParserError> {
        use crate::symbol::module::SumVariant;

        let pos = pos!();
        kw!(Sum);
        let name = ident!();
        let id = self.claim_type_decl(&name, pos, ModuleSymbols::as_sum)?;

        token!(OpenBrace);

        let mut variants: Vec<SumVariant> = Vec::new();
        loop {
            skip_nl!();
            if self.check(&TokenKind::CloseBrace) || self.is_at_end() {
                break;
            }
            let variant_pos = self.peek().pos;
            let variant_name = ident!();
            if variants.iter().any(|v| v.name == variant_name) {
                return Err(ParserError::DuplicateDeclaration(variant_name, variant_pos));
            }

            let mut fields: Vec<(String, LangType)> = Vec::new();
            if self.match_token(&[TokenKind::OpenParen]) {
                if self.check(&TokenKind::CloseParen) {
                    return Err(ParserError::UnexpectedToken(
                        format!(
                            "empty payload parens on variant '{variant_name}' — a payload-less variant is a bare name"
                        ),
                        self.peek().pos,
                    ));
                }
                loop {
                    let field_type = self.parse_type()?;
                    if !matches!(self.peek().kind, TokenKind::Identifier(_)) {
                        return Err(ParserError::ExpectedToken(
                            "payload field name — fields are named, parameter-style".to_string(),
                            format!("{}", self.peek().kind),
                            self.peek().pos,
                        ));
                    }
                    let field_name = ident!();
                    if fields.iter().any(|(n, _)| n == &field_name) {
                        return Err(ParserError::DuplicateDeclaration(
                            field_name,
                            variant_pos,
                        ));
                    }
                    fields.push((field_name, field_type));
                    if !self.match_token(&[TokenKind::Comma]) {
                        break;
                    }
                }
                token!(CloseParen);
            }
            variants.push(SumVariant {
                name: variant_name,
                fields,
            });

            // One variant per line — a same-line sibling is rejected here.
            if !self.check(&TokenKind::CloseBrace)
                && !self.match_token(&[TokenKind::Newline, TokenKind::Semicolon])
            {
                return Err(ParserError::ExpectedToken(
                    "newline after sum variant (one variant per line)".to_string(),
                    format!("{}", self.peek().kind),
                    self.peek().pos,
                ));
            }
        }
        // A sum with no variants is uninhabited, so it is rejected — mirrors
        // the enum rule, diagnostic pointing at the `}`.
        if variants.is_empty() {
            return Err(ParserError::ExpectedToken(
                "at least one sum variant".to_string(),
                format!("{}", self.peek().kind),
                pos,
            ));
        }
        token!(CloseBrace);

        self.module.set_sum_variants(id.def(), variants);
        Ok(())
    }

    /// Post-pass-1 check: no type-struct or sum may store itself by value,
    /// directly or through a struct/sum cycle — the layout would be infinite.
    /// Arrays count (an array is by-value storage); any pointer depth breaks
    /// the cycle. Runs once after every layout is final, pushing one error per
    /// distinct cycle-closing type.
    pub(crate) fn check_byvalue_containment_cycles(&mut self) {
        use crate::parser::cycles::{find_byvalue_cycles, Node};

        for node in find_byvalue_cycles(&self.module) {
            let def = self.module.type_def(match node {
                Node::Struct(id) => id.def(),
                Node::Sum(id) => id.def(),
            });
            self.errors
                .push(ParserError::RecursiveByValue(def.name.clone(), def.pos));
        }
    }

    /// A method is `[const] fn IDENT (`; a function-pointer field type is
    /// `fn (`, so the token after `fn` — identifier vs `(` — discriminates.
    /// Any `public` prefix was already consumed by the caller.
    fn upcoming_is_method(&self) -> bool {
        let mut i = self.current;
        let kind_at = |idx: usize| self.tokens.get(idx).map(|t| &t.kind);
        if matches!(kind_at(i), Some(TokenKind::Keyword(Keyword::Const))) {
            i += 1;
        }
        if !matches!(kind_at(i), Some(TokenKind::Keyword(Keyword::Fn))) {
            return false;
        }
        matches!(kind_at(i + 1), Some(TokenKind::Identifier(_)))
    }

    /// Methods are desugared to free functions named `Type$method`. An instance
    /// method's leading bare `this` receiver is supplied as an implicit `*Struct`
    /// (or `*const Struct` for `const fn`) first parameter; a static method omits it.
    #[parse_rule]
    fn parse_method(
        &mut self,
        struct_id: StructId,
        struct_name: &str,
        is_const_fn: bool,
        vis: crate::symbol::module::Visibility,
    ) -> Result<crate::parser::Function, ParserError> {
        use crate::parser::{Function, FunctionProto};
        use crate::symbol::module::{mangle_method, MethodSig};
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
            // A method's `public` governs access through its type (the
            // `MethodSig.vis` gate), not the module namespace or linkage.
            vis: Visibility::Private,
            export: false,
            pos,
        };

        // Register so plain `FunctionCall { name: mangled, ... }` resolves.
        self.register_fn_symbol(&mangled, &params, return_type, false, Visibility::Private, pos)?;

        // Register in the struct's method registry (params exclude `this`).
        let visible_params: Vec<(LangType, String)> = if has_this {
            params[1..].to_vec()
        } else {
            params.clone()
        };
        self.module.add_method(
            struct_id.def(),
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
            && self.module[id].methods.contains_key(name)
        {
            return true;
        }
        // Static: base is a bare identifier naming a known type-struct, with
        // no local variable shadowing it.
        if let ExprKind::Variable(var_name) = &base.kind
            && let Some(id) = self.module.struct_id(var_name)
            && self.symbol_table.lookup_variable(var_name).is_none()
            && self.module[id].methods.contains_key(name)
        {
            return true;
        }
        false
    }

    /// A fn *with* a body catches this in pass 2 when its scope re-declares a
    /// parameter, but the bodyless forms (`extern fn`, `asm fn`) never open
    /// that scope. Checking the proto directly covers all three forms with a
    /// matching diagnostic.
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
        export: bool,
    ) -> Result<crate::parser::Function, ParserError> {
        use crate::parser::{Function, FunctionProto};
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
            export,
            pos,
        };

        self.register_fn_symbol(&name, &params, return_type, is_extern, vis, pos)?;

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
    pub(crate) fn parse_global_var(
        &mut self,
        vis: Visibility,
        export: bool,
    ) -> Result<crate::parser::GlobalVar, ParserError> {
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
        // The reference-site gate needs this: the outermost variable scope's
        // `Symbol` carries no visibility of its own.
        self.global_vis.insert(name.clone(), vis);

        term!();

        Ok(GlobalVar {
            var_type,
            name,
            initializer,
            pos,
            vis,
            export,
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

    /// A brace expression that parses as a comma-separated list *is* a list
    /// initializer; anything else re-parses as a **value-block**. The two
    /// grammars cannot both accept one input: a valid value-block must contain
    /// a `return`, which can never appear in a valid list.
    ///
    /// Speculation is safe (as in `parse_cast_or_alloc`): expression parsing
    /// has no side effects beyond interned string literals, rolled back by
    /// truncation.
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

    /// `{ stmt* }` as an expression (opening brace not yet consumed). Errors
    /// propagate with no `sync!` recovery — inside an expression there is no
    /// safe resync point. The type is a `void` placeholder the checker later
    /// resolves from the block's `return` statements.
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

#[cfg(test)]
mod tests {
    use crate::parser::{Parser, Program};

    fn parse(source: &str) -> Program {
        let tokens = crate::lexer::tokenize(source.to_string()).expect("lex");
        Parser::new(tokens).parse_program().expect("parse")
    }

    /// An `enum` registers its variants in declaration order; the index is the
    /// value. Comma-separated on one line.
    #[test]
    fn enum_registers_variants_in_order() {
        let program = parse("enum Color { Red, Green, Blue }\nfn f() -> i32 {\n    return 0\n}");
        let id = program.symbols.enum_id("Color").expect("Color interned");
        let info = &program.symbols[id];
        assert_eq!(info.variants, ["Red", "Green", "Blue"]);
        assert_eq!(program.symbols.enum_variant_index(id, "Blue"), Some(2));
        assert_eq!(program.symbols.enum_variant_index(id, "Cyan"), None);
    }

    /// Variants may be separated by newlines, commas, or a mix of both.
    #[test]
    fn enum_variants_may_be_newline_separated() {
        let program = parse("enum E {\n    A\n    B,\n    C\n}\nfn f() -> i32 {\n    return 0\n}");
        let id = program.symbols.enum_id("E").expect("E interned");
        assert_eq!(program.symbols[id].variants, ["A", "B", "C"]);
    }

    /// An enum with no variants is uninhabited and rejected.
    #[test]
    fn empty_enum_is_rejected() {
        let tokens = crate::lexer::tokenize("enum E { }\nfn f() -> i32 {\n    return 0\n}".to_string())
            .expect("lex");
        assert!(Parser::new(tokens).parse_program().is_err());
    }
}
