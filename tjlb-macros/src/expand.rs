use proc_macro2::TokenStream;
use quote::quote;
use syn::{
    parse::Parser as SynParser,
    punctuated::Punctuated,
    visit_mut::{self, VisitMut},
    Expr, Macro, Stmt, Token,
};

pub struct DslRewriter;

impl VisitMut for DslRewriter {
    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        // Depth-first: rewrite inner macros before outer ones.
        visit_mut::visit_expr_mut(self, expr);

        if let Expr::Macro(mac_expr) = expr {
            if let Some(expanded) = expand_macro(&mac_expr.mac) {
                *expr = syn::parse2(expanded)
                    .unwrap_or_else(|e| panic!("parse_rule: DSL macro expansion failed: {e}"));
            }
        }
    }

    fn visit_stmt_mut(&mut self, stmt: &mut Stmt) {
        // syn 2.x parses `macro_call!(...);` at statement level as Stmt::Macro,
        // which is NOT visited by visit_expr_mut. Handle it here.
        if let Stmt::Macro(mac_stmt) = stmt {
            if let Some(expanded) = expand_macro(&mac_stmt.mac) {
                let semi = mac_stmt.semi_token;
                *stmt = Stmt::Expr(
                    syn::parse2(expanded)
                        .unwrap_or_else(|e| panic!("parse_rule: DSL macro expansion failed: {e}")),
                    semi,
                );
                return;
            }
        }
        visit_mut::visit_stmt_mut(self, stmt);
    }
}

fn expand_macro(mac: &Macro) -> Option<TokenStream> {
    let name = mac.path.get_ident()?.to_string();
    match name.as_str() {
        "pos" => Some(expand_pos()),
        "kw" => Some(expand_kw(&mac.tokens)),
        "token" => Some(expand_token(&mac.tokens)),
        "token_if" => Some(expand_token_if(&mac.tokens)),
        "kw_if" => Some(expand_kw_if(&mac.tokens)),
        "skip_nl" => Some(expand_skip_nl()),
        "term" => Some(expand_term()),
        "opt_unless_term" => Some(expand_opt_unless_term(&mac.tokens)),
        "block_body" => Some(expand_block_body(&mac.tokens)),
        "ident" => Some(expand_ident()),
        "lang_type" => Some(expand_lang_type()),
        "opt" => Some(expand_opt(&mac.tokens)),
        "many" => Some(expand_many(&mac.tokens)),
        "alt" => Some(expand_alt(&mac.tokens)),
        "scoped" => Some(expand_scoped(&mac.tokens)),
        "sync" => Some(expand_sync(&mac.tokens)),
        _ => None,
    }
}

fn expand_pos() -> TokenStream {
    quote! { self.peek().pos }
}

fn expand_skip_nl() -> TokenStream {
    quote! { self.skip_newlines() }
}

fn expand_term() -> TokenStream {
    quote! { self.match_token(&[TokenKind::Semicolon, TokenKind::Newline]) }
}

fn expand_lang_type() -> TokenStream {
    quote! { self.parse_type()? }
}

fn expand_ident() -> TokenStream {
    quote! {
        {
            match &self.peek().kind {
                TokenKind::Identifier(name) => {
                    let name = name.clone();
                    self.advance();
                    name
                }
                _ => return Err(ParserError::ExpectedToken(
                    "identifier".to_string(),
                    format!("{}", self.peek().kind),
                    self.peek().pos,
                )),
            }
        }
    }
}

fn expand_kw(tokens: &TokenStream) -> TokenStream {
    let kw_ident: syn::Ident = syn::parse2(tokens.clone())
        .unwrap_or_else(|_| panic!("kw! expects a single keyword name, e.g. kw!(Return)"));
    let kw_str = kw_ident.to_string().to_lowercase();
    quote! { self.expect_keyword(&Keyword::#kw_ident, #kw_str)? }
}

fn expand_token(tokens: &TokenStream) -> TokenStream {
    let variant: syn::Ident = syn::parse2(tokens.clone()).unwrap_or_else(|_| {
        panic!("token! expects a single TokenKind variant, e.g. token!(OpenParen)")
    });
    let display = token_display(&variant.to_string());
    quote! { self.expect(&TokenKind::#variant, #display)? }
}

fn token_display(variant: &str) -> &'static str {
    match variant {
        "OpenParen" => "(",
        "CloseParen" => ")",
        "OpenBrace" => "{",
        "CloseBrace" => "}",
        "OpenBracket" => "[",
        "CloseBracket" => "]",
        "Semicolon" => ";",
        "Comma" => ",",
        "Arrow" => "->",
        "Assign" => "=",
        "Colon" => ":",
        _ => "token",
    }
}

fn expand_token_if(tokens: &TokenStream) -> TokenStream {
    let idents: Vec<syn::Ident> = Punctuated::<syn::Ident, Token![,]>::parse_terminated
        .parse2(tokens.clone())
        .unwrap_or_else(|_| panic!("token_if! expects comma-separated TokenKind variants"))
        .into_iter()
        .collect();
    quote! { self.match_token(&[#(TokenKind::#idents),*]) }
}

fn expand_kw_if(tokens: &TokenStream) -> TokenStream {
    let kw_ident: syn::Ident = syn::parse2(tokens.clone())
        .unwrap_or_else(|_| panic!("kw_if! expects a single keyword name, e.g. kw_if!(Else)"));
    quote! {
        if self.check_keyword(&Keyword::#kw_ident) { self.advance(); true } else { false }
    }
}

fn expand_opt_unless_term(tokens: &TokenStream) -> TokenStream {
    let func: syn::Ident = syn::parse2(tokens.clone()).unwrap_or_else(|_| {
        panic!("opt_unless_term! expects a method name, e.g. opt_unless_term!(parse_expression)")
    });
    quote! {
        if self.check_terminator() { None } else { Some(self.#func()?) }
    }
}

fn expand_block_body(tokens: &TokenStream) -> TokenStream {
    let func: syn::Ident = syn::parse2(tokens.clone()).unwrap_or_else(|_| {
        panic!("block_body! expects a method name, e.g. block_body!(parse_block_statement)")
    });
    quote! {
        match self.#func()? {
            Statement { kind: StatementKind::Block(stmts), .. } => stmts,
            _ => unreachable!(),
        }
    }
}

// ── Backtracking combinators ────────────────────────────────────────────────

fn expand_opt(tokens: &TokenStream) -> TokenStream {
    let func: syn::Ident = syn::parse2(tokens.clone())
        .unwrap_or_else(|_| panic!("opt! expects a method name, e.g. opt!(parse_type)"));
    quote! {
        {
            let __saved_pos = self.current;
            let __saved_sl  = self.string_literals.len();
            match self.#func() {
                Ok(v)  => Some(v),
                Err(_) => {
                    self.current = __saved_pos;
                    self.string_literals.truncate(__saved_sl);
                    None
                }
            }
        }
    }
}

fn expand_many(tokens: &TokenStream) -> TokenStream {
    let func: syn::Ident = syn::parse2(tokens.clone())
        .unwrap_or_else(|_| panic!("many! expects a method name, e.g. many!(parse_statement)"));
    quote! {
        {
            let mut __many_vec = Vec::new();
            loop {
                let __saved_pos = self.current;
                let __saved_sl  = self.string_literals.len();
                match self.#func() {
                    Ok(v)  => __many_vec.push(v),
                    Err(_) => {
                        self.current = __saved_pos;
                        self.string_literals.truncate(__saved_sl);
                        break;
                    }
                }
            }
            __many_vec
        }
    }
}

fn expand_alt(tokens: &TokenStream) -> TokenStream {
    let funcs: Vec<syn::Ident> = Punctuated::<syn::Ident, Token![,]>::parse_terminated
        .parse2(tokens.clone())
        .unwrap_or_else(|_| {
            panic!("alt! expects comma-separated method names, e.g. alt!(parse_a, parse_b)")
        })
        .into_iter()
        .collect();
    if funcs.is_empty() {
        panic!("alt! requires at least one alternative");
    }
    // Generates a right-nested match chain: try first, on Err restore and try rest.
    let nested = build_alt_chain(&funcs);
    quote! { { #nested } }
}

fn build_alt_chain(funcs: &[syn::Ident]) -> TokenStream {
    if funcs.len() == 1 {
        let f = &funcs[0];
        return quote! { self.#f() };
    }
    let f = &funcs[0];
    let rest = build_alt_chain(&funcs[1..]);
    quote! {
        {
            let __saved_pos = self.current;
            let __saved_sl  = self.string_literals.len();
            match self.#f() {
                Ok(v)  => Ok(v),
                Err(_) => {
                    self.current = __saved_pos;
                    self.string_literals.truncate(__saved_sl);
                    #rest
                }
            }
        }
    }
}

/// `sync!(parse_X)` — error-recovering combinator.
///
/// Calls `self.parse_X()`. On success yields `Some(value)`.
/// On failure, records the error in `self.errors`, truncates any phantom
/// string-literal entries, calls `self.synchronize()` to skip to the next
/// safe token, and yields `None` so the caller can continue parsing.
fn expand_sync(tokens: &TokenStream) -> TokenStream {
    let func: syn::Ident = syn::parse2(tokens.clone())
        .unwrap_or_else(|_| panic!("sync! expects a method name, e.g. sync!(parse_statement)"));
    quote! {
        {
            let __saved_sl = self.string_literals.len();
            match self.#func() {
                Ok(v)  => Some(v),
                Err(e) => {
                    self.string_literals.truncate(__saved_sl);
                    self.errors.push(e);
                    self.synchronize();
                    None
                }
            }
        }
    }
}

/// `scoped!({ body })` — wraps `body` in an enter/exit_scope pair.
///
/// `body` is a block whose last expression is the value returned by `scoped!`.
/// Any `?` operators inside `body` propagate errors through a closure so that
/// `exit_scope` is always called even on failure.
///
/// The body must end with a plain `T`, not a `Result<T, E>` — use `?` inside
/// to unwrap intermediate Results.
fn expand_scoped(tokens: &TokenStream) -> TokenStream {
    // Parse the argument as a syn::Block so we can recursively rewrite DSL
    // macros that appear inside the scoped body.
    let mut block: syn::Block = syn::parse2(tokens.clone())
        .unwrap_or_else(|e| panic!("scoped! expects a block, e.g. scoped!({{ ... }}): {e}"));
    DslRewriter.visit_block_mut(&mut block);
    quote! {
        {
            self.symbol_table_mut().enter_scope();
            let __scope_result = (|| -> Result<_, ParserError> { Ok(#block) })();
            self.symbol_table_mut().exit_scope();
            __scope_result?
        }
    }
}
