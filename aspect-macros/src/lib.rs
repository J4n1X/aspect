mod error_position;
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
/// # expected: 42                          # compile & run; assert main's i32 return == 42
/// # expected: "fragment1", "fragment2"    # compile only; assert error contains each fragment
/// # run_args: "arg1", "arg2"             # optional: forwarded as argv[1..] to main
/// # compile_args: "-I", "lib"            # optional: compiler flags (-D/-I), mirroring the CLI
/// # requires_arch: ARCH_X86_64           # optional: compile this test only on that host arch
/// ```
///
/// Runtime tests JIT in-process via Inkwell's `ExecutionEngine` — no `lli` binary
/// is involved. Each program runs at both `-O0` and `-O2`, and the two must agree.
///
/// Files without a `# expected:` line are silently skipped.
#[proc_macro]
pub fn generate_tests(input: TokenStream) -> TokenStream {
    generate_tests::generate_tests_impl(input)
}

/// Derive `fn position(&self) -> Option<crate::lexer::Position>` for an error
/// enum, replacing the hand-written `match` matchers.
///
/// For each variant the canonical position is chosen as:
/// * the field annotated `#[position]` — which delegates via `field.position()`
///   when that field is a nested error rather than a `Position`;
/// * otherwise the sole `Position`-typed field, or, when several are present,
///   the one named `pos`, else `position`, else the first;
/// * `None` when the variant carries no position.
///
/// The generated method matches the previous hand-written signature exactly
/// (`#[must_use] pub fn position`).
#[proc_macro_derive(ErrorPosition, attributes(position))]
pub fn derive_error_position(item: TokenStream) -> TokenStream {
    let input = syn::parse_macro_input!(item as syn::DeriveInput);
    error_position::derive_error_position(input).into()
}
