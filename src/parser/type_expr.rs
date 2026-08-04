use crate::lexer::{Keyword, LangType, Position, TokenKind, TypeBase};
use crate::parser::{Parser, ParserError};

impl Parser {
    pub(crate) fn parse_type(&mut self) -> Result<LangType, ParserError> {
        let pos = self.peek().pos;
        let kind = self.peek().kind.clone();
        match kind {
            // A bare `const` keyword reaches here only for a *named* base or a
            // re-split builtin (`const u8[MAX]` after define substitution) — the
            // scanner fuses `const` with built-in scalar spellings. `const` is a
            // single flag over the whole resolved type.
            TokenKind::Keyword(Keyword::Const) => {
                self.advance();
                let inner = self.parse_type()?;
                Ok(inner.with_const(true))
            }
            // Usually pre-folded (`u8[10]*` is one token), but the scanner only
            // folds `[N]` for literal N — `u8[MAX_SIZE]` reaches us as `u8` `[`
            // `1024` `]` after define substitution, so re-apply the modifiers.
            TokenKind::LangType(lang_type) => {
                self.advance();
                Ok(self.apply_type_modifiers(lang_type))
            }
            // Named types (aliases, type-structs) lex as bare identifiers, so
            // resolve them against the module table.
            TokenKind::Identifier(name) => {
                self.advance();
                self.resolve_named_type(&name, pos)
            }
            // Parens are the only way to spell "array of pointers" (`(i32*)[3]`)
            // or "array of fn-pointers": the lexer greedily folds `T[N]`/`T*`
            // into the preceding type token.
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
                self.parse_fnptr_type()
            }
            _ => Err(ParserError::ExpectedToken(
                "type".to_string(),
                format!("{}", self.peek().kind),
                self.peek().pos,
            )),
        }
    }

    /// Resolve a bare identifier as a type name, enforcing import visibility
    /// against the name's declaring file. Aliasing does not launder module
    /// visibility, so an alias's underlying struct/enum/sum is checked as if
    /// named directly.
    fn resolve_named_type(&mut self, name: &str, pos: Position) -> Result<LangType, ParserError> {
        let base = if let Some(info) = self.module.alias_info(name) {
            self.check_import_visibility("type alias", name, info.file_id, pos)?;
            if let TypeBase::Struct(id) = info.ty.base {
                self.check_struct_visibility(id, pos)?;
            } else if let TypeBase::Enum(id) = info.ty.base {
                self.check_enum_visibility(id, pos)?;
            } else if let TypeBase::Sum(id) = info.ty.base {
                self.check_sum_visibility(id, pos)?;
            }
            info.ty
        } else if let Some(id) = self.module.struct_id(name) {
            self.check_struct_visibility(id, pos)?;
            LangType::struct_type(id)
        } else if let Some(id) = self.module.enum_id(name) {
            self.check_enum_visibility(id, pos)?;
            LangType::enum_type(id)
        } else if let Some(id) = self.module.sum_id(name) {
            self.check_sum_visibility(id, pos)?;
            LangType::sum_type(id)
        } else {
            return Err(ParserError::UndefinedType(name.to_string(), pos));
        };
        Ok(self.apply_type_modifiers(base))
    }

    /// The leading `fn` keyword is already consumed; parses `(T1, T2, ...) ->
    /// R` (or a `void`/`u0` return when `->` is absent).
    fn parse_fnptr_type(&mut self) -> Result<LangType, ParserError> {
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

    /// Attaches `[N]`/`*` modifiers to a named type (built-in types arrive
    /// pre-folded). Stacks on any depth the resolved type already carries
    /// (`alias P u8*` then `P*` yields `pointer_depth == 2`).
    fn apply_type_modifiers(&mut self, mut ty: LangType) -> LangType {
        // Array suffix first, then pointer depth (the lexer's order). Restore
        // the cursor on a malformed `[` so a later index `[i]` isn't consumed.
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
        let known = self.module.resolve_alias(name).is_some()
            || self.module.struct_id(name).is_some()
            || self.module.enum_id(name).is_some()
            || self.module.sum_id(name).is_some();
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
}
