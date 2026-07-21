mod error_position;
mod expand;
mod generate_tests;

use expand::DslRewriter;
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use syn::visit_mut::VisitMut;

/// Rewrites the decorated function two ways: DSL macro calls (`pos!`, `kw!`,
/// `token!`, …) in the body expand to their full Rust, and the body is wrapped
/// in a context push/pop so `Parser.context_stack` reflects the running rule.
#[proc_macro_attribute]
pub fn parse_rule(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut func: syn::ItemFn =
        syn::parse(item).expect("#[parse_rule] can only be applied to functions");

    // Human-readable label from the function name:
    // "parse_return_statement" -> "return statement", "do_parse_program" -> "program".
    let raw_name = func.sig.ident.to_string();
    let label: String = raw_name
        .strip_prefix("do_parse_")
        .or_else(|| raw_name.strip_prefix("parse_"))
        .unwrap_or(&raw_name)
        .replace('_', " ");

    DslRewriter.visit_item_fn_mut(&mut func);

    // The IIFE captures `self` by mutable reference for the duration of the call,
    // which releases the borrow before `context_stack.pop()` runs.
    let orig_stmts: Vec<syn::Stmt> = func.block.stmts.drain(..).collect();
    let wrapped: syn::Block = syn::parse2(quote::quote! {
        {
            self.context_stack.push(#label);
            let __ctx_result = (|| { #(#orig_stmts)* })();
            self.context_stack.pop();
            __ctx_result
        }
    })
    .expect("parse_rule: failed to build context-wrapped block");

    *func.block = wrapped;

    // Suppress the unused-import warning that appears when `TokenStream2` is
    // only referenced inside the `quote!` expansion.
    let _ = TokenStream2::new();

    quote::quote!(#func).into()
}

/// Scans `tests/programs/` at compile time and emits one `#[test]` per `.ap`
/// file carrying a `# expected:` annotation (in its first 10 lines):
///
/// ```text
/// # expected: 42                          # compile & run; assert main's i32 return == 42
/// # expected: "fragment1", "fragment2"    # compile only; assert error contains each fragment
/// # run_args: "arg1", "arg2"             # optional: forwarded as argv[1..] to main
/// # compile_args: "-I", "lib"            # optional: compiler flags (-D/-I), mirroring the CLI
/// # requires_arch: ARCH_X86_64           # optional: compile this test only on that host arch
/// ```
///
/// Runtime tests JIT in-process at both `-O0` and `-O2`, and the two must agree.
/// Files without a `# expected:` line are skipped.
#[proc_macro]
pub fn generate_tests(input: TokenStream) -> TokenStream {
    generate_tests::generate_tests_impl(input)
}

/// Derives `fn position(&self) -> Option<crate::lexer::Position>` for an error
/// enum. Per variant the canonical position is:
/// * the `#[position]`-annotated field (delegating via `field.position()` when
///   it is a nested error, not a `Position`);
/// * otherwise the sole `Position` field, or — with several — the one named
///   `pos`, else `position`, else the first;
/// * `None` when the variant carries no position.
#[proc_macro_derive(ErrorPosition, attributes(position))]
pub fn derive_error_position(item: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(item as syn::DeriveInput);
    error_position::derive_error_position(input).into()
}
