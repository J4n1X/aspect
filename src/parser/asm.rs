use crate::lexer::{Keyword, LangType, Position, TokenKind};
use crate::parser::expressions::Parser;
use crate::parser::ParserError;
use crate::symbol::module::Visibility;
use aspect_macros::parse_rule;

impl Parser {
    /// Parse an `asm fn` declaration: a sibling of `extern fn`. Where an
    /// extern fn's body lives in another object file, an asm fn's body *is*
    /// the given instructions.
    ///
    /// Unlike an ordinary fn the body is parsed inline rather than deferred
    /// to pass 2 — an asm body is string literals, so nothing in it can
    /// forward-reference anything.
    ///
    /// Registering an ordinary `FunctionSymbol` at the end is what makes call
    /// sites ordinary: `lookup_function`, the type checker and call codegen
    /// all treat this exactly like any other function with a body.
    /// `pos` is the `asm` keyword, already consumed by the caller's
    /// kind-modifier scan.
    #[parse_rule]
    pub(crate) fn parse_asm_function(
        &mut self,
        pos: Position,
        vis: Visibility,
        export: bool,
        attrs: Vec<crate::parser::Attribute>,
    ) -> Result<crate::parser::Function, ParserError> {
        use crate::parser::{AsmReg, AsmSpec, Function, FunctionProto};
        use crate::symbol::table::FunctionSymbol;

        kw!(Fn);
        let name = ident!();
        token!(OpenParen);

        // Every parameter must be pinned (no compiler-allocated operands), so
        // `param_regs` stays exactly parallel to `params`.
        let mut param_regs: Vec<AsmReg> = Vec::new();
        let params = self.parse_comma_separated(&TokenKind::CloseParen, |p| {
            let param_type = p.parse_type()?;
            let param_name = p.parse_ident("parameter name")?;
            if !p.check(&TokenKind::Colon) {
                return Err(ParserError::AsmMissingParamRegister(
                    param_name,
                    p.peek().pos,
                ));
            }
            p.advance(); // ':'
            param_regs.push(p.parse_asm_reg()?);
            Ok((param_type, param_name))
        })?;
        Self::check_duplicate_params(&params, pos)?;

        let (return_type, return_reg) = if self.match_token(&[TokenKind::Arrow]) {
            let ty = self.parse_type()?;
            let reg = if self.check(&TokenKind::Colon) {
                self.advance();
                Some(self.parse_asm_reg()?)
            } else {
                None
            };
            (ty, reg)
        } else {
            (LangType::VOID, None)
        };

        // A void asm fn has no output register; a value-returning one must
        // say where its value comes out.
        if return_type.is_void_value() {
            if let Some(reg) = &return_reg {
                return Err(ParserError::AsmVoidReturnRegister(name.clone(), reg.pos));
            }
        } else if return_reg.is_none() {
            return Err(ParserError::AsmMissingReturnRegister(name.clone(), pos));
        }

        // `clobbers` is contextual — a plain identifier, never a keyword — so
        // it stays usable as an ordinary name elsewhere. The clause is
        // optional and may sit on its own line.
        skip_nl!();
        let clobbers = if matches!(&self.peek().kind, TokenKind::Identifier(n) if n == "clobbers") {
            self.advance();
            token!(OpenParen);
            self.parse_comma_separated(&TokenKind::CloseParen, Self::parse_asm_reg)?
        } else {
            Vec::new()
        };
        skip_nl!();

        // Body: adjacent string literals, one line of assembly each.
        let body_pos = pos!();
        let lines = self.parse_asm_body_lines()?;
        if lines.is_empty() {
            return Err(ParserError::AsmEmptyBody(name.clone(), body_pos));
        }

        self.module
            .add_function(FunctionSymbol {
                name: name.clone(),
                params: params.clone(),
                return_type,
                is_extern: false,
                has_body: true,
                vis,
                pos,
            })
            .map_err(|e| ParserError::from_symbol(e, pos))?;

        Ok(Function {
            proto: FunctionProto {
                name,
                params,
                return_type,
                vis,
                export,
                attrs,
                pos,
            },
            body: crate::parser::FunctionBody::Asm(AsmSpec {
                param_regs,
                return_reg,
                clobbers,
                lines,
                pos,
            }),
        })
    }

    /// Parse a `naked fn` declaration. Like `asm fn`, the body *is* its
    /// instructions — but a naked function carries LLVM's `naked` attribute (no
    /// prologue/epilogue), so it takes ordinary (un-pinned) parameters that
    /// arrive in their platform-ABI registers and the asm body addresses
    /// directly. Its motivating use is a freestanding `_start` that reads
    /// `argc`/`argv` off the stack. `pos` is the `naked` keyword, already
    /// consumed by the caller's kind-modifier scan.
    #[parse_rule]
    pub(crate) fn parse_naked_function(
        &mut self,
        pos: Position,
        vis: Visibility,
        export: bool,
        attrs: Vec<crate::parser::Attribute>,
    ) -> Result<crate::parser::Function, ParserError> {
        use crate::parser::{Function, FunctionProto, NakedSpec};
        use crate::symbol::table::FunctionSymbol;

        kw!(Fn);
        let name = ident!();
        token!(OpenParen);

        // Ordinary parameters — no register pins. A naked fn receives its
        // arguments per the platform ABI (SysV: rdi, rsi, …); the asm body
        // reads them where the ABI leaves them.
        let params = self.parse_comma_separated(&TokenKind::CloseParen, |p| {
            let param_type = p.parse_type()?;
            let param_name = p.parse_ident("parameter name")?;
            Ok((param_type, param_name))
        })?;
        Self::check_duplicate_params(&params, pos)?;

        let return_type = if self.match_token(&[TokenKind::Arrow]) {
            self.parse_type()?
        } else {
            LangType::VOID
        };

        skip_nl!();
        let body_pos = pos!();
        let lines = self.parse_asm_body_lines()?;
        if lines.is_empty() {
            return Err(ParserError::AsmEmptyBody(name.clone(), body_pos));
        }

        self.module
            .add_function(FunctionSymbol {
                name: name.clone(),
                params: params.clone(),
                return_type,
                is_extern: false,
                has_body: true,
                vis,
                pos,
            })
            .map_err(|e| ParserError::from_symbol(e, pos))?;

        Ok(Function {
            proto: FunctionProto {
                name,
                params,
                return_type,
                vis,
                export,
                attrs,
                pos,
            },
            body: crate::parser::FunctionBody::Naked(NakedSpec { lines, pos }),
        })
    }

    /// Parse an inline-asm body block: `{ "line" "line" … }`, one string
    /// literal per line. Consumed as raw tokens, NOT through expression
    /// parsing — asm lines must never land in the program's string-literal
    /// table. Shared by `asm fn` and `naked fn`; emptiness is the caller's
    /// error to report (with the right declaration position).
    fn parse_asm_body_lines(&mut self) -> Result<Vec<String>, ParserError> {
        self.expect(&TokenKind::OpenBrace, "{")?;
        let mut lines: Vec<String> = Vec::new();
        loop {
            self.skip_newlines();
            if self.check(&TokenKind::CloseBrace) || self.is_at_end() {
                break;
            }
            match &self.peek().kind {
                TokenKind::StringLiteral(s) => {
                    lines.push(s.clone());
                    self.advance();
                }
                other => {
                    return Err(ParserError::ExpectedToken(
                        "assembly string literal".to_string(),
                        format!("{other}"),
                        self.peek().pos,
                    ));
                }
            }
        }
        self.expect(&TokenKind::CloseBrace, "}")?;
        Ok(lines)
    }

    /// Consume one contextual register name. Register names are ordinary
    /// identifiers — meaningful only after a `:` in an `asm fn` signature or
    /// inside `clobbers(...)` — so `rax` stays usable as a variable name
    /// everywhere else in the language. Validating the name against the
    /// target's register table is the type checker's job, not the parser's.
    fn parse_asm_reg(&mut self) -> Result<crate::parser::AsmReg, ParserError> {
        let pos = self.peek().pos;
        match &self.peek().kind {
            TokenKind::Identifier(name) => {
                let name = name.clone();
                self.advance();
                Ok(crate::parser::AsmReg { name, pos })
            }
            other => Err(ParserError::ExpectedToken(
                "register name".to_string(),
                format!("{other}"),
                pos,
            )),
        }
    }
}
