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
    /// Parse a complete program.
    /// Returns all accumulated errors if any were encountered during parsing.
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

        // Pre-register all type-struct names and pre-install all aliases so
        // named types resolve regardless of declaration order (self/mutual
        // reference, alias chains in any order).
        self.prescan_type_names();
        self.prescan_aliases();

        skip_nl!();

        while !self.is_at_end() {
            // Item attributes come first (`@attr public fn ...`) and attach to
            // whichever item follows.
            let attrs = self.parse_leading_attrs()?;
            let vis_pos = pos!();
            let vis = if kw_if!(Public) {
                Visibility::Public
            } else {
                Visibility::Private
            };
            let kind = self.parse_kind_modifier()?;
            let is_extern = matches!(&kind, Some((Keyword::Extern, _)));

            // `extern` already names a symbol defined elsewhere; marking it
            // `public` would claim to export something this module never
            // defines.
            if is_extern && vis == Visibility::Public {
                return Err(ParserError::UnexpectedToken(
                    "extern functions cannot be public — they are defined elsewhere".to_string(),
                    vis_pos,
                ));
            }

            // `public` answers "does this symbol leave the object file", which
            // only a function this module defines can.
            let defines_a_fn = matches!(&kind, Some((Keyword::Asm, _) | (Keyword::Naked, _)))
                || (self.check_keyword(&Keyword::Fn) && !self.starts_fnptr_var_decl());
            let defines_a_global = matches!(
                self.peek().kind,
                TokenKind::LangType(_) | TokenKind::Identifier(_)
            ) || self.starts_fnptr_var_decl() || self.starts_grouped_var_decl();

            if vis == Visibility::Public && !defines_a_fn && !defines_a_global {
                return Err(ParserError::UnexpectedToken(
                    "public can only be used with functions or global variables".to_string(),
                    vis_pos,
                ));
            }

            if let Some((Keyword::Asm, asm_pos)) = &kind {
                let func = self.parse_asm_function(*asm_pos, vis, attrs)?;
                functions.push(func);
            } else if let Some((Keyword::Naked, naked_pos)) = &kind {
                let func = self.parse_naked_function(*naked_pos, vis, attrs)?;
                functions.push(func);
            }
            // `fn ident(...)` is a function definition; `fn(...)` (no name
            // between `fn` and `(`) is a function-pointer-typed global. The
            // statement-table dispatch handles the local-decl variant.
            else if self.check_keyword(&Keyword::Fn) && !self.starts_fnptr_var_decl() {
                let func = self.parse_function(is_extern, vis, attrs)?;
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
                let global = self.parse_global_var(vis, attrs)?;
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
        })
    }

    /// Consume the leading kind-modifiers of a top-level declaration, yielding
    /// the one named (with its position) or `None`.
    ///
    /// `extern` and `asm` both answer "which kind of function is this", and a
    /// function is exactly one kind, so naming two is one error whichever
    /// order they appear in. Scanning them together rather than testing pairs
    /// is what keeps that true as kinds are added.
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

    /// Pre-register every `type <Name>` struct name with a reserved id before
    /// the main parse, so named types resolve regardless of declaration order
    /// (and self/mutually-referential structs work). Records each name's
    /// declaring file (the `type` keyword's `pos.file_id`) for the
    /// import-visibility check. Does not consume tokens.
    fn prescan_type_names(&mut self) {
        let names: Vec<(String, u32)> = self
            .tokens
            .windows(2)
            .filter_map(|w| match (&w[0].kind, &w[1].kind) {
                (TokenKind::Keyword(Keyword::Type), TokenKind::Identifier(name)) => {
                    Some((name.clone(), w[0].pos.file_id))
                }
                _ => None,
            })
            .collect();
        for (name, file_id) in names {
            self.module.intern_struct(&name, file_id);
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
        if self.module.resolve_alias(&name).is_some() || self.module.struct_id(&name).is_some() {
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
            if self.module.resolve_alias(&name).is_some() || self.module.struct_id(&name).is_some()
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
