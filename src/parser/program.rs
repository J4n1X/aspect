use crate::lexer::{Keyword, LangType, Position, TokenKind};
use crate::parser::expressions::Parser;
use crate::parser::{ParserError, Statement, StatementKind};
use crate::symbol::module::Visibility;
use aspect_macros::parse_rule;

/// A function body whose parsing is deferred to pass 2 of `do_parse_program`:
/// enough context to jump back and parse it once every prototype is known.
pub(crate) struct PendingBody {
    /// Proto name (mangled for methods) — the unique key used to fill the
    /// parsed body back into `Program::functions`.
    name: String,
    /// Full parameter list, including any implicit `this` receiver.
    params: Vec<(LangType, String)>,
    pos: Position,
    /// Token index of the body's `{`.
    body_start: usize,
}

impl Parser {
    /// # Errors
    /// Returns `Err(Vec<ParserError>)` if one or more parse errors occurred.
    pub fn parse_program(&mut self) -> Result<crate::parser::Program, Vec<ParserError>> {
        let result = self.do_parse_program();
        let mut errs = std::mem::take(&mut self.errors);
        match result {
            Ok(prog) if errs.is_empty() => return Ok(prog),
            Ok(_) => {}
            Err(e) => errs.push(e),
        }
        errs.sort_by_key(|e| {
            e.position()
                .map_or((usize::MAX, usize::MAX), |p| (p.line, p.column))
        });
        Err(errs)
    }

    /// Two-pass program parse. Pass 1 walks the top level: signatures,
    /// globals and struct layouts are parsed and registered (struct names and
    /// aliases were pre-installed by the prescans), but function bodies are
    /// only skipped (brace-matched) and recorded. Pass 2 revisits each
    /// recorded body with the full symbol table. Declaration order is thus
    /// non-semantic, with one exception: global-variable *initializers* are
    /// parsed in pass 1 and only see earlier definitions.
    #[parse_rule]
    fn do_parse_program(&mut self) -> Result<crate::parser::Program, ParserError> {
        use crate::parser::Program;

        let mut functions = Vec::new();
        let mut global_vars = Vec::new();
        let mut rules = Vec::new();

        // Pre-register type/enum names and aliases so named types resolve
        // regardless of declaration order. Enums are interned before the alias
        // prescan so an alias colliding with an enum name is caught there.
        self.prescan_type_names();
        self.prescan_enum_names();
        self.prescan_aliases();

        skip_nl!();

        while !self.is_at_end() {
            // Item attributes come first (`@attr public fn ...`) and attach to
            // whichever item follows.
            let attrs = self.parse_leading_attrs()?;
            let (vis, export, vis_pos) = self.parse_vis_linkage_modifiers()?;
            let kind = self.parse_kind_modifier()?;
            let is_extern = matches!(&kind, Some((Keyword::Extern, _)));

            // `rule <anchor> <fn>` — a soft keyword: a type or global literally
            // named `rule` still parses (`rule x = …`), so a rule is detected by
            // lookahead (`Self::is_rule_decl`), before the item gates below.
            // `public` governs reach (whole-program vs the declaring module,
            // like `public type`); `export`/linkage make no sense on a rule.
            if self.is_rule_decl() {
                Self::reject_attrs(&attrs, "a rule declaration")?;
                if export {
                    return Err(ParserError::UnexpectedToken(
                        "a rule cannot be export".to_string(),
                        vis_pos,
                    ));
                }
                if let Some((kw, kw_pos)) = &kind {
                    return Err(ParserError::UnexpectedToken(
                        format!("a rule cannot be {kw}"),
                        *kw_pos,
                    ));
                }
                rules.push(self.parse_rule_decl(vis)?);
                skip_nl!();
                continue;
            }

            // `rule fn` — a rule-checker function (Phase 2b). `rule` is a soft
            // keyword; `rule fn` is unambiguous because `fn` is a keyword (never
            // a global of a type named `rule`). std/meta is in scope in its
            // body, it may not be called from ordinary code, and it is codegen'd
            // into the JIT-only judge module. (`expansion fn` / `transform fn`
            // will join it as the other two hooks land.)
            if self.is_rule_fn() {
                Self::reject_attrs(&attrs, "a rule function")?;
                if vis == Visibility::Public || export {
                    return Err(ParserError::UnexpectedToken(
                        "a rule function cannot be public or export".to_string(),
                        vis_pos,
                    ));
                }
                if let Some((kw, kw_pos)) = &kind {
                    return Err(ParserError::UnexpectedToken(
                        format!("a rule function cannot be {kw}"),
                        *kw_pos,
                    ));
                }
                self.advance(); // consume the `rule` soft keyword
                let mut func =
                    self.parse_function(false, Visibility::Private, false, Vec::new())?;
                func.proto.meta_kind = Some(crate::parser::MetaKind::Rule);
                functions.push(func);
                skip_nl!();
                continue;
            }

            // `extern` may be `public` (nameable from importers) but never
            // `export`: there is no local symbol here to give external linkage.
            if is_extern && export {
                return Err(ParserError::UnexpectedToken(
                    "extern functions cannot be exported — they are defined elsewhere, so there is no local symbol to give external linkage".to_string(),
                    vis_pos,
                ));
            }

            // `public` = module visibility (functions, globals, type-structs).
            // `export` = external linkage, which only a symbol with a linked
            // object-file symbol can carry — never a type or alias.
            let defines_a_fn = matches!(&kind, Some((Keyword::Asm, _) | (Keyword::Naked, _)))
                || (self.check_keyword(&Keyword::Fn) && !self.starts_fnptr_var_decl());
            let defines_a_type =
                self.check_keyword(&Keyword::Type) || self.check_keyword(&Keyword::Enum);
            let defines_a_global = matches!(
                self.peek().kind,
                TokenKind::LangType(_) | TokenKind::Identifier(_)
            ) || self.starts_fnptr_var_decl()
                || self.starts_grouped_var_decl()
                // `const <named-type>` global (`const Point* g`): a bare `const`
                // keyword survives the scanner only for non-scalar bases, and at
                // top level (after any `public`/`export`) it begins a global.
                || self.check_keyword(&Keyword::Const);

            if vis == Visibility::Public && !defines_a_fn && !defines_a_type && !defines_a_global {
                return Err(ParserError::UnexpectedToken(
                    "public can only be used with functions, global variables, or type definitions"
                        .to_string(),
                    vis_pos,
                ));
            }
            if export && !defines_a_fn && !defines_a_global {
                return Err(ParserError::UnexpectedToken(
                    "export can only be used with functions or global variables — a type, enum or alias has no linked symbol"
                        .to_string(),
                    vis_pos,
                ));
            }

            if let Some((Keyword::Asm, asm_pos)) = &kind {
                let func = self.parse_asm_function(*asm_pos, vis, export, attrs)?;
                functions.push(func);
            } else if let Some((Keyword::Naked, naked_pos)) = &kind {
                let func = self.parse_naked_function(*naked_pos, vis, export, attrs)?;
                functions.push(func);
            }
            // `fn ident(...)` is a definition; `fn(...)` is a function-pointer
            // -typed global.
            else if self.check_keyword(&Keyword::Fn) && !self.starts_fnptr_var_decl() {
                let func = self.parse_function(is_extern, vis, export, attrs)?;
                functions.push(func);
            } else if self.check_keyword(&Keyword::Alias) {
                // An alias is a pure compile-time name binding — there is no
                // node for an attribute to ride on.
                Self::reject_attrs(&attrs, "an alias declaration")?;
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
                let methods = self.parse_struct_def(attrs)?;
                functions.extend(methods);
            } else if self.check_keyword(&Keyword::Enum) {
                if is_extern {
                    return Err(ParserError::UnexpectedToken(
                        "extern can only be used with functions".to_string(),
                        self.peek().pos,
                    ));
                }
                self.parse_enum_def(attrs)?;
            } else if matches!(
                self.peek().kind,
                TokenKind::LangType(_) | TokenKind::Identifier(_)
            ) || self.starts_fnptr_var_decl()
                || self.starts_grouped_var_decl()
                || self.check_keyword(&Keyword::Const)
            {
                // A leading built-in type, named type (alias / type-struct),
                // function-pointer type, parenthesised group, or `const`
                // (over a named base) begins a global variable declaration.
                if is_extern {
                    return Err(ParserError::UnexpectedToken(
                        "extern can only be used with functions".to_string(),
                        self.peek().pos,
                    ));
                }
                let global = self.parse_global_var(vis, export, attrs)?;
                global_vars.push(global);
            } else {
                return Err(ParserError::UnexpectedToken(
                    format!("{}", self.peek().kind),
                    self.peek().pos,
                ));
            }

            skip_nl!();
        }

        // Pass 2: every prototype (free function and method) is registered by
        // now — parse the deferred bodies and fill them into their functions.
        let mut bodies = self.parse_pending_bodies();
        for func in &mut functions {
            if let Some(body) = bodies.remove(&func.proto.name) {
                func.body = crate::parser::FunctionBody::Aspect(body);
            }
        }

        Ok(Program {
            functions,
            global_vars,
            string_literals: self.string_literals.iter().cloned().collect(),
            symbols: std::mem::take(&mut self.module),
            source_files: self.source_files.clone(),
            rules,
            file_modules: self.file_modules.clone(),
        })
    }

    /// `public` and `export` are two orthogonal axes (`public export` is the
    /// fully-open form) accepted in either order, so they are scanned together
    /// and a repeat of either is the error. Returns visibility, whether
    /// `export` was given, and the first modifier's position for diagnostics.
    fn parse_vis_linkage_modifiers(
        &mut self,
    ) -> Result<(Visibility, bool, Position), ParserError> {
        let start_pos = self.peek().pos;
        let mut vis = Visibility::Private;
        let mut export = false;
        let mut saw_public = false;
        loop {
            if self.check_keyword(&Keyword::Public) {
                let p = self.peek().pos;
                if saw_public {
                    return Err(ParserError::UnexpectedToken("duplicate `public`".to_string(), p));
                }
                saw_public = true;
                vis = Visibility::Public;
                self.advance();
            } else if self.check_keyword(&Keyword::Export) {
                let p = self.peek().pos;
                if export {
                    return Err(ParserError::UnexpectedToken("duplicate `export`".to_string(), p));
                }
                export = true;
                self.advance();
            } else {
                return Ok((vis, export, start_pos));
            }
        }
    }

    /// `extern`/`asm`/`naked` all answer "which kind of function is this", and
    /// a function is exactly one kind, so naming two is one error in any order.
    /// Scanning them together (not testing pairs) keeps that true as kinds grow.
    fn parse_kind_modifier(&mut self) -> Result<Option<(Keyword, Position)>, ParserError> {
        let mut kind: Option<(Keyword, Position)> = None;
        loop {
            let next = if self.check_keyword(&Keyword::Extern) {
                Keyword::Extern
            } else if self.check_keyword(&Keyword::Asm) {
                Keyword::Asm
            } else if self.check_keyword(&Keyword::Naked) {
                Keyword::Naked
            } else {
                return Ok(kind);
            };
            let next_pos = self.peek().pos;
            if let Some((prev, _)) = &kind {
                let msg = if *prev == next {
                    format!("duplicate `{next}`")
                } else {
                    "extern, asm and naked cannot be combined on one function".to_string()
                };
                return Err(ParserError::UnexpectedToken(msg, next_pos));
            }
            kind = Some((next, next_pos));
            self.advance();
        }
    }

    /// Lookahead-only detector for the soft keyword `rule`. A rule is
    /// `rule <Type|@attr> <fn>`; a value global is at most `Type name [= …]`
    /// (two identifiers). So a leading `rule` begins a declaration iff the next
    /// token is `@` (attribute anchor) or it is followed by *two* identifiers
    /// (`rule T f`) — a type literally named `rule` in `rule x = …` stays a
    /// global. Consumes nothing.
    fn is_rule_decl(&self) -> bool {
        let TokenKind::Identifier(name) = &self.peek().kind else {
            return false;
        };
        if name != "rule" {
            return false;
        }
        let kind_at = |n: usize| self.tokens.get(self.current + n).map(|t| &t.kind);
        if matches!(kind_at(1), Some(TokenKind::At)) {
            return true;
        }
        matches!(kind_at(1), Some(TokenKind::Identifier(_)))
            && matches!(kind_at(2), Some(TokenKind::Identifier(_)))
    }

    /// Lookahead-only detector for the `rule fn` soft keyword: the identifier
    /// `rule` immediately before the `fn` keyword. Distinct from a `rule
    /// <anchor> <checker>` declaration (`rule` before `@` or two identifiers)
    /// and from a global of a type named `rule`. Consumes nothing.
    fn is_rule_fn(&self) -> bool {
        matches!(&self.peek().kind, TokenKind::Identifier(n) if n == "rule")
            && matches!(
                self.tokens.get(self.current + 1).map(|t| &t.kind),
                Some(TokenKind::Keyword(Keyword::Fn))
            )
    }

    /// Parse `rule <anchor> <checker_fn>` with the cursor on the `rule` soft
    /// keyword (guaranteed by [`Self::is_rule_decl`]). The anchor is a
    /// type-struct name or an `@attribute`; `checker_fn` names a builtin rule.
    #[parse_rule]
    fn parse_rule_decl(&mut self, vis: Visibility) -> Result<crate::parser::RuleDecl, ParserError> {
        use crate::parser::{RuleAnchor, RuleDecl};
        let pos = pos!();
        self.advance(); // the `rule` soft keyword (not a real keyword)
        let anchor = if self.check(&TokenKind::At) {
            self.advance();
            RuleAnchor::Attribute(ident!())
        } else {
            RuleAnchor::Type(ident!())
        };
        let checker_fn = ident!();
        term!();
        Ok(RuleDecl {
            anchor,
            checker_fn,
            vis,
            pos,
        })
    }

    /// Collect the `(name, file_id, visibility)` of every `<kw> <Name>`
    /// declaration, honoring a directly-preceding `public`. Shared spine of
    /// [`Self::prescan_type_names`] and [`Self::prescan_enum_names`]; both must
    /// know visibility at intern time, since import cycles can place a module's
    /// *uses* of a name before its definition in the inlined token stream. Does
    /// not consume tokens.
    fn prescan_named(&self, kw: Keyword) -> Vec<(String, u32, Visibility)> {
        self.tokens
            .windows(2)
            .enumerate()
            .filter_map(|(i, w)| match (&w[0].kind, &w[1].kind) {
                (TokenKind::Keyword(k), TokenKind::Identifier(name)) if *k == kw => {
                    let vis = if i > 0
                        && matches!(
                            self.tokens[i - 1].kind,
                            TokenKind::Keyword(Keyword::Public)
                        ) {
                        Visibility::Public
                    } else {
                        Visibility::Private
                    };
                    Some((name.clone(), w[0].pos.file_id, vis))
                }
                _ => None,
            })
            .collect()
    }

    /// Reserves an id for every `type <Name>` before the main parse, so named
    /// types resolve regardless of order (self/mutual reference included).
    fn prescan_type_names(&mut self) {
        for (name, file_id, vis) in self.prescan_named(Keyword::Type) {
            self.module.intern_struct(&name, file_id, vis);
        }
    }

    /// The enum twin of [`Self::prescan_type_names`]: reserves an id for every
    /// `enum <Name>`, so forward references and import cycles resolve.
    fn prescan_enum_names(&mut self) {
        for (name, file_id, vis) in self.prescan_named(Keyword::Enum) {
            self.module.intern_enum(&name, file_id, vis);
        }
    }

    /// Pre-install every `alias` definition before pass 1, so aliases resolve
    /// regardless of declaration order. Fixpoint-iterates so chains may appear
    /// in any order (`alias A B` before `alias B i32`). Nothing is reported
    /// here: a site that never resolves (undefined target, cycle, duplicate)
    /// is left out of `alias_prescan_sites`, and pass 1's `parse_type_alias`
    /// re-parses it to produce the error at its natural position.
    fn prescan_aliases(&mut self) {
        let saved = self.current;
        let mut sites: Vec<usize> = self
            .tokens
            .iter()
            .enumerate()
            .filter(|(_, t)| matches!(t.kind, TokenKind::Keyword(Keyword::Alias)))
            .map(|(i, _)| i)
            .collect();
        loop {
            let before = sites.len();
            sites.retain(|&site| {
                self.current = site;
                self.try_prescan_alias().is_err()
            });
            if sites.is_empty() || sites.len() == before {
                break;
            }
        }
        self.current = saved;
    }

    /// Attempt to parse and install one `alias Name Target` definition with
    /// the cursor on the `alias` keyword. Fails when the target doesn't
    /// resolve yet — `prescan_aliases` retries it next round.
    #[parse_rule]
    fn try_prescan_alias(&mut self) -> Result<(), ParserError> {
        let site = self.current;
        let pos = pos!();
        kw!(Alias);
        let name = ident!();
        if self.module.resolve_alias(&name).is_some()
            || self.module.struct_id(&name).is_some()
            || self.module.enum_id(&name).is_some()
        {
            return Err(ParserError::DuplicateType(name, pos));
        }
        let target = self.parse_type()?;
        self.module.define_alias(name, target, pos.file_id);
        self.alias_prescan_sites.insert(site);
        Ok(())
    }

    /// Pass 1 body handling: record where a function body starts, then skip
    /// over it (balanced braces). The body is parsed in pass 2 by
    /// `parse_pending_bodies` once every prototype is registered, so calls
    /// resolve regardless of definition order.
    pub(crate) fn defer_function_body(
        &mut self,
        name: String,
        params: Vec<(LangType, String)>,
        pos: Position,
    ) -> Result<(), ParserError> {
        if !self.check(&TokenKind::OpenBrace) {
            return Err(ParserError::ExpectedToken(
                "{".to_string(),
                format!("{}", self.peek().kind),
                self.peek().pos,
            ));
        }
        self.pending_bodies.push(PendingBody {
            name,
            params,
            pos,
            body_start: self.current,
        });
        let mut depth = 0usize;
        while !self.is_at_end() {
            match self.peek().kind {
                TokenKind::OpenBrace => depth += 1,
                TokenKind::CloseBrace => {
                    depth -= 1;
                    if depth == 0 {
                        self.advance();
                        return Ok(());
                    }
                }
                _ => {}
            }
            self.advance();
        }
        Err(ParserError::UnexpectedEof)
    }

    /// Pass 2: parse every body deferred during pass 1. Errors are collected
    /// per body so one broken function doesn't hide errors in the others.
    /// Returns the parsed bodies keyed by proto name.
    fn parse_pending_bodies(&mut self) -> std::collections::HashMap<String, Vec<Statement>> {
        let mut bodies = std::collections::HashMap::new();
        for pending in std::mem::take(&mut self.pending_bodies) {
            self.current = pending.body_start;
            match self.parse_deferred_body(&pending) {
                Ok(stmts) => {
                    bodies.insert(pending.name, stmts);
                }
                Err(e) => self.errors.push(e),
            }
        }
        bodies
    }

    #[parse_rule]
    fn parse_deferred_body(&mut self, pending: &PendingBody) -> Result<Vec<Statement>, ParserError> {
        let body = scoped!({
            for (param_type, param_name) in &pending.params {
                self.symbol_table_mut()
                    .add_variable(param_name.clone(), *param_type, pending.pos)
                    .map_err(|e| ParserError::from_symbol(e, pending.pos))?;
            }
            match self.parse_block_statement()? {
                Statement {
                    kind: StatementKind::Block(stmts),
                    ..
                } => stmts,
                _ => unreachable!(),
            }
        });
        Ok(body)
    }

    /// Parse a top-level type alias: `alias NewName TargetType`.
    ///
    /// Aliases are pure compile-time name bindings — they produce no AST node,
    /// only an entry in the module symbol table consulted by `parse_type`.
    /// Definition normally happened in `prescan_aliases` (so aliases can be
    /// referenced before their definition); here we only consume the tokens
    /// and report the errors the prescan stayed silent about (duplicates,
    /// unresolvable targets, cycles).
    #[parse_rule]
    fn parse_type_alias(&mut self) -> Result<(), ParserError> {
        let site = self.current;
        let pos = pos!();
        kw!(Alias);
        let name = ident!();
        if self.alias_prescan_sites.contains(&site) {
            self.parse_type()?;
        } else {
            if self.module.resolve_alias(&name).is_some()
                || self.module.struct_id(&name).is_some()
                || self.module.enum_id(&name).is_some()
            {
                return Err(ParserError::DuplicateType(name, pos));
            }
            let target = self.parse_type()?;
            self.module.define_alias(name, target, pos.file_id);
        }
        term!();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::parser::{MetaKind, Parser, Program, RuleAnchor};

    fn parse(source: &str) -> Program {
        let tokens = crate::lexer::tokenize(source.to_string()).expect("lex");
        Parser::new(tokens).parse_program().expect("parse")
    }

    /// `rule <Type> <fn>` — three identifiers — is a rule declaration.
    #[test]
    fn type_anchored_rule_parses() {
        let program = parse("rule Config singleton\nfn f() -> i32 {\n    return 0\n}");
        assert_eq!(program.rules.len(), 1);
        assert!(matches!(&program.rules[0].anchor, RuleAnchor::Type(n) if n == "Config"));
        assert_eq!(program.rules[0].checker_fn, "singleton");
    }

    /// `rule @attr <fn>` — the `@` after `rule` — is an attribute-anchored rule.
    #[test]
    fn attribute_anchored_rule_parses() {
        let program = parse("rule @nopanic auditor\nfn f() -> i32 {\n    return 0\n}");
        assert_eq!(program.rules.len(), 1);
        assert!(matches!(&program.rules[0].anchor, RuleAnchor::Attribute(n) if n == "nopanic"));
        assert_eq!(program.rules[0].checker_fn, "auditor");
    }

    /// The soft keyword: a type literally named `rule` used as a global
    /// (`rule g = …`, two identifiers then `=`) is a global, not a rule.
    #[test]
    fn type_named_rule_stays_a_global() {
        let program = parse(
            "type rule {\n    public i32 v\n}\nrule g = rule { v = 5 }\nfn f() -> i32 {\n    return 0\n}",
        );
        assert!(program.rules.is_empty());
        assert!(program.global_vars.iter().any(|g| g.name == "g"));
    }

    /// A rule may carry `public` — it makes the rule whole-program (vs. its
    /// declaring module by default), mirroring `public type`.
    #[test]
    fn public_rule_parses_whole_program() {
        let program = parse("public rule Config singleton\nfn f() -> i32 {\n    return 0\n}");
        assert_eq!(program.rules.len(), 1);
        assert_eq!(
            program.rules[0].vis,
            crate::symbol::module::Visibility::Public
        );
    }

    /// A private (bare) rule defaults to module scope.
    #[test]
    fn bare_rule_is_module_scoped() {
        let program = parse("rule Config singleton\nfn f() -> i32 {\n    return 0\n}");
        assert_eq!(
            program.rules[0].vis,
            crate::symbol::module::Visibility::Private
        );
    }

    /// A rule still may not carry `export` — there is no linkage on a rule.
    #[test]
    fn export_rule_is_rejected() {
        let tokens = crate::lexer::tokenize(
            "export rule Config singleton\nfn f() -> i32 {\n    return 0\n}".to_string(),
        )
        .expect("lex");
        assert!(Parser::new(tokens).parse_program().is_err());
    }

    /// `rule fn` marks a rule-checker function — and is a *function*, not a
    /// `rule <anchor> <checker>` declaration (the `fn` disambiguates).
    #[test]
    fn rule_fn_is_marked() {
        let program = parse("rule fn check(i32 x) -> i32 {\n    return x\n}");
        assert_eq!(program.functions.len(), 1);
        assert_eq!(program.functions[0].proto.meta_kind, Some(MetaKind::Rule));
        assert_eq!(program.functions[0].proto.name, "check");
        assert!(program.rules.is_empty());
    }

    /// An ordinary function has no meta kind.
    #[test]
    fn ordinary_fn_has_no_meta_kind() {
        let program = parse("fn f() -> i32 {\n    return 0\n}");
        assert_eq!(program.functions[0].proto.meta_kind, None);
    }

    /// A rule fn may not carry `public` — rejected at parse time.
    #[test]
    fn public_rule_fn_is_rejected() {
        let tokens =
            crate::lexer::tokenize("public rule fn f() -> i32 {\n    return 0\n}".to_string())
                .expect("lex");
        assert!(Parser::new(tokens).parse_program().is_err());
    }
}
