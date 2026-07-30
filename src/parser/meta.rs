//! Top-level parsing for the metaprogramming hooks — `rule` and `transform` in
//! all their forms. Split out of `program.rs` so the two-pass driver's top-level
//! loop stays about *ordinary* items; `try_parse_meta_item` is its one
//! entry point here. All of this is still parsed inline in the single token
//! stream — this module only keeps the soft-keyword machinery in one place.

use crate::lexer::{Keyword, LangType, Position, TokenKind, TypeBase};
use crate::parser::expressions::Parser;
use crate::parser::{Attribute, Function, GlobalVar, ParserError, RuleDecl, TransformDecl};
use crate::symbol::module::Visibility;
use aspect_macros::parse_rule;

/// A parsed metaprogramming declaration, for the caller to file into the right
/// `Program` collection.
pub(crate) enum MetaItem {
    Rule(RuleDecl),
    Transform(TransformDecl),
    /// A `rule fn` / `transform fn` handler (already stamped with its
    /// [`crate::parser::MetaKind`]).
    Function(Function),
    /// A `meta` global (already stamped `is_meta`).
    Global(GlobalVar),
}

impl Parser {
    /// If the cursor is at a metaprogramming declaration — `rule`/`transform` in
    /// any of their forms — parse it and return the item; otherwise `None`, and
    /// the caller falls through to ordinary item parsing. Meta declarations take
    /// no `export` and no kind modifier; the checker/handler *functions*
    /// additionally take no `public` (visibility lives on the binding).
    pub(crate) fn try_parse_meta_item(
        &mut self,
        attrs: &[Attribute],
        vis: Visibility,
        export: bool,
        vis_pos: Position,
        kind: &Option<(Keyword, Position)>,
    ) -> Result<Option<MetaItem>, ParserError> {
        // `rule <anchor> <fn>` — a soft keyword (a type/global literally named
        // `rule` still parses as `rule x = …`), detected by lookahead before the
        // item gates. `public` governs reach (whole-program vs declaring module,
        // like `public type`); `export`/kind make no sense on a rule.
        if self.is_rule_decl() {
            Self::reject_attrs(attrs, "a rule declaration")?;
            Self::reject_export(export, vis_pos, "a rule")?;
            Self::reject_kind(kind, "a rule")?;
            return Ok(Some(MetaItem::Rule(self.parse_rule_decl(vis)?)));
        }

        // `rule fn` — a rule-checker function. `rule` is a soft keyword; `rule
        // fn` is unambiguous because `fn` is a keyword. std/meta is in scope in
        // its body, it may not be called from ordinary code, and it is codegen'd
        // into the JIT-only judge module.
        if self.is_rule_fn() {
            Self::reject_attrs(attrs, "a rule function")?;
            if vis == Visibility::Public || export {
                return Err(ParserError::UnexpectedToken(
                    "a rule function cannot be public or export".to_string(),
                    vis_pos,
                ));
            }
            Self::reject_kind(kind, "a rule function")?;
            self.advance(); // consume the `rule` soft keyword
            let mut func = self.parse_function(false, Visibility::Private, false, Vec::new())?;
            func.proto.meta_kind = Some(crate::parser::MetaKind::Rule);
            return Ok(Some(MetaItem::Function(func)));
        }

        // `transform fn <name>(Expr) -> Expr` — an obligation handler run during
        // elaboration. `transform fn <name>` is told from a `transform fn(...)`
        // fn-pointer coercion key by the identifier after `fn`.
        if self.is_transform_fn() {
            Self::reject_attrs(attrs, "a transform function")?;
            if vis == Visibility::Public || export {
                return Err(ParserError::UnexpectedToken(
                    "a transform function cannot be public or export — visibility lives on the `transform <key> <fn>` binding".to_string(),
                    vis_pos,
                ));
            }
            Self::reject_kind(kind, "a transform function")?;
            self.advance(); // consume the `transform` soft keyword
            let mut func = self.parse_function(false, Visibility::Private, false, Vec::new())?;
            func.proto.meta_kind = Some(crate::parser::MetaKind::Transform);
            return Ok(Some(MetaItem::Function(func)));
        }

        // `transform <key> <handler>` — bind a handler to a coercion (`From ->
        // To`) or attribute (`@name`) key. `public` governs reach (like a rule);
        // `export`/kind make no sense on a transform.
        if self.is_transform_decl() {
            Self::reject_attrs(attrs, "a transform declaration")?;
            Self::reject_export(export, vis_pos, "a transform")?;
            Self::reject_kind(kind, "a transform")?;
            return Ok(Some(MetaItem::Transform(self.parse_transform_decl(vis)?)));
        }

        // `meta <scalar> <name> [= <init>]` — compile-time-only mutable state.
        // `meta` is a soft keyword, told from a global of a type named `meta` by
        // the scalar `LangType` token that must follow (a type name would lex as
        // an identifier). Whole-compilation state, so no `public`/`export`/kind.
        if self.is_meta_global() {
            Self::reject_attrs(attrs, "a meta global")?;
            Self::reject_export(export, vis_pos, "a meta global")?;
            Self::reject_kind(kind, "a meta global")?;
            if vis == Visibility::Public {
                return Err(ParserError::UnexpectedToken(
                    "a meta global cannot be public — it is whole-compilation state, not a module symbol".to_string(),
                    vis_pos,
                ));
            }
            self.advance(); // consume the `meta` soft keyword
            let mut global = self.parse_global_var(Visibility::Private, false, Vec::new())?;
            if !Self::is_meta_scalar(&global.var_type) {
                return Err(ParserError::UnexpectedToken(
                    format!(
                        "a meta global must be an integer or bool type in v1, found '{}'",
                        global.var_type
                    ),
                    global.pos,
                ));
            }
            global.is_meta = true;
            return Ok(Some(MetaItem::Global(global)));
        }

        Ok(None)
    }

    /// Lookahead-only detector for a `meta` global: the identifier `meta`, a
    /// scalar `LangType` token, then an identifier. The `LangType` requirement is
    /// what keeps a global of a user type named `meta` (`meta g = …`, where the
    /// second token is an identifier, not a built-in type) parsing as an ordinary
    /// global. Consumes nothing.
    fn is_meta_global(&self) -> bool {
        matches!(&self.peek().kind, TokenKind::Identifier(n) if n == "meta")
            && matches!(
                self.tokens.get(self.current + 1).map(|t| &t.kind),
                Some(TokenKind::LangType(_))
            )
            && matches!(
                self.tokens.get(self.current + 2).map(|t| &t.kind),
                Some(TokenKind::Identifier(_))
            )
    }

    /// A meta global's type must be a by-value integer or bool in v1 (the store
    /// is a scalar; pointers/structs/arrays/floats are deferred).
    fn is_meta_scalar(ty: &LangType) -> bool {
        ty.pointer_depth == 0
            && !ty.is_array()
            && matches!(ty.base, TypeBase::SInt | TypeBase::UInt | TypeBase::Bool)
    }

    /// Reject an `export` modifier on a construct that has no linked symbol.
    fn reject_export(export: bool, vis_pos: Position, what: &str) -> Result<(), ParserError> {
        if export {
            return Err(ParserError::UnexpectedToken(
                format!("{what} cannot be export"),
                vis_pos,
            ));
        }
        Ok(())
    }

    /// Reject a kind modifier (`extern`/`asm`/`naked`) on a meta declaration.
    fn reject_kind(kind: &Option<(Keyword, Position)>, what: &str) -> Result<(), ParserError> {
        if let Some((kw, kw_pos)) = kind {
            return Err(ParserError::UnexpectedToken(
                format!("{what} cannot be {kw}"),
                *kw_pos,
            ));
        }
        Ok(())
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
    /// <anchor> <checker>` declaration and from a global of a type named `rule`.
    /// Consumes nothing.
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
    fn parse_rule_decl(&mut self, vis: Visibility) -> Result<RuleDecl, ParserError> {
        use crate::parser::RuleAnchor;
        let pos = pos!(); // provided by #[parse_rule]
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

    /// Lookahead-only detector for `transform fn <name>`: the identifier
    /// `transform`, the `fn` keyword, then an identifier (the handler name). The
    /// trailing identifier distinguishes a handler descriptor from a `transform
    /// fn(...) -> ...` fn-pointer coercion key (where `(` follows `fn`). Consumes
    /// nothing.
    fn is_transform_fn(&self) -> bool {
        matches!(&self.peek().kind, TokenKind::Identifier(n) if n == "transform")
            && matches!(
                self.tokens.get(self.current + 1).map(|t| &t.kind),
                Some(TokenKind::Keyword(Keyword::Fn))
            )
            && matches!(
                self.tokens.get(self.current + 2).map(|t| &t.kind),
                Some(TokenKind::Identifier(_))
            )
    }

    /// Lookahead-only detector for a `transform <key> <handler>` binding: the
    /// identifier `transform` followed by an `@` (attribute key) or a
    /// type-starting token (coercion `From -> To` key). Checked after
    /// [`Self::is_transform_fn`], which claims `transform fn <name>`. Consumes
    /// nothing.
    fn is_transform_decl(&self) -> bool {
        if !matches!(&self.peek().kind, TokenKind::Identifier(n) if n == "transform") {
            return false;
        }
        matches!(
            self.tokens.get(self.current + 1).map(|t| &t.kind),
            Some(
                TokenKind::At
                    | TokenKind::LangType(_)
                    | TokenKind::Identifier(_)
                    | TokenKind::OpenParen
                    | TokenKind::Keyword(Keyword::Const)
                    | TokenKind::Keyword(Keyword::Fn)
            )
        )
    }

    /// Parse `transform <key> <handler_fn>` with the cursor on the `transform`
    /// soft keyword (guaranteed by [`Self::is_transform_decl`]). The key is an
    /// `@attribute` or a coercion `<from> -> <to>`; `parse_type` on the from-type
    /// greedily eats a fn-pointer type's own `->`, so the arrow that survives is
    /// always the key separator.
    #[parse_rule]
    fn parse_transform_decl(&mut self, vis: Visibility) -> Result<TransformDecl, ParserError> {
        use crate::parser::TransformKey;
        let pos = pos!();
        self.advance(); // the `transform` soft keyword
        let key = if self.check(&TokenKind::At) {
            self.advance();
            TransformKey::Attribute(ident!())
        } else {
            let from = self.parse_type()?;
            token!(Arrow);
            let to = self.parse_type()?;
            TransformKey::Coerce { from, to }
        };
        let handler_fn = ident!();
        term!();
        Ok(TransformDecl {
            key,
            handler_fn,
            vis,
            pos,
        })
    }
}
