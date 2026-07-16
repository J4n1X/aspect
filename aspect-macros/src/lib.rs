mod expand;
mod generate_tests;

use expand::DslRewriter;
use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use syn::visit_mut::VisitMut;

/// Attribute macro for parser rule functions.
///
/// Two things happen to the decorated function:
/// 1. DSL macro calls (`pos!`, `kw!`, `token!`, etc.) in the body are rewritten to
///    their full Rust expansions.
/// 2. The body is wrapped in a context push/pop so that `Parser.context_stack` always
///    reflects which rule is currently executing.
#[proc_macro_attribute]
pub fn parse_rule(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let mut func: syn::ItemFn =
        syn::parse(item).expect("#[parse_rule] can only be applied to functions");

    // Derive a human-readable label from the function name.
    // "parse_return_statement" -> "return statement"
    // "do_parse_program"       -> "program"
    let raw_name = func.sig.ident.to_string();
    let label: String = raw_name
        .strip_prefix("do_parse_")
        .or_else(|| raw_name.strip_prefix("parse_"))
        .unwrap_or(&raw_name)
        .replace('_', " ");

    // Step 1: rewrite DSL macros in the function body.
    DslRewriter.visit_item_fn_mut(&mut func);

    // Step 2: wrap the rewritten body in a context push/pop IIFE.
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

/// Function-like macro that scans `tests/programs/` at compile time and emits
/// one `#[test]` function per `.ap` file that carries a `# expected:` annotation.
///
/// Annotation format (in the first 10 lines of the `.ap` file):
///
/// ```text
/// # expected: 42                          # compile & run; assert exit code == 42
/// # expected: "fragment1", "fragment2"    # compile only; assert error contains each fragment
/// # run_args: "arg1", "arg2"             # optional: argv passed to lli-19
/// ```
///
/// Files without a `# expected:` line are silently skipped.
#[proc_macro]
pub fn generate_tests(input: TokenStream) -> TokenStream {
    generate_tests::generate_tests_impl(input)
}
