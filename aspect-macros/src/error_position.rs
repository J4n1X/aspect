use proc_macro2::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Field, Fields, Ident, Type, Variant};

/// How a variant sources its `Position`.
#[derive(Clone)]
enum Binding {
    /// A named struct field, bound by its identifier.
    Named(Ident),
    /// An unnamed tuple field, bound positionally at this index.
    Indexed(usize),
}

/// The chosen position source for one variant.
struct Selected {
    binding: Binding,
    /// `true` when the chosen field is *not* itself a `Position` but a nested
    /// error carrying its own `position()` — the arm delegates via
    /// `field.position()` instead of `Some(*field)`.
    delegate: bool,
}

/// Generate `impl <Enum> { pub fn position(&self) -> Option<Position> }` whose
/// body mirrors the hand-written matchers: return the canonical `Position` for
/// variants that carry one, `None` otherwise.
pub fn derive_error_position(input: DeriveInput) -> TokenStream {
    let enum_name = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let Data::Enum(data) = &input.data else {
        panic!("#[derive(ErrorPosition)] can only be applied to enums");
    };

    let arms = data.variants.iter().map(|variant| build_arm(enum_name, variant));

    quote! {
        impl #impl_generics #enum_name #ty_generics #where_clause {
            /// Extract the source position from this error, if any.
            #[must_use]
            pub fn position(&self) -> Option<crate::lexer::Position> {
                match self {
                    #(#arms)*
                }
            }
        }
    }
}

/// Build the `match` arm for a single variant.
fn build_arm(enum_name: &Ident, variant: &Variant) -> TokenStream {
    let vname = &variant.ident;
    let selection = select_position_field(&variant.fields);

    match (&variant.fields, selection) {
        // Named struct variant with a chosen field.
        (
            Fields::Named(_),
            Some(Selected {
                binding: Binding::Named(field),
                delegate,
            }),
        ) => {
            if delegate {
                quote! { #enum_name::#vname { #field, .. } => #field.position(), }
            } else {
                quote! { #enum_name::#vname { #field, .. } => Some(*#field), }
            }
        }
        // Tuple variant with a chosen field.
        (
            Fields::Unnamed(fields),
            Some(Selected {
                binding: Binding::Indexed(index),
                delegate,
            }),
        ) => {
            let pats = (0..fields.unnamed.len()).map(|i| {
                if i == index {
                    quote!(__pos)
                } else {
                    quote!(_)
                }
            });
            if delegate {
                quote! { #enum_name::#vname(#(#pats),*) => __pos.position(), }
            } else {
                quote! { #enum_name::#vname(#(#pats),*) => Some(*__pos), }
            }
        }
        // No position source: `None` arm, ignoring any payload.
        (Fields::Named(_), None) => quote! { #enum_name::#vname { .. } => None, },
        (Fields::Unnamed(_), None) => quote! { #enum_name::#vname(..) => None, },
        (Fields::Unit, _) => quote! { #enum_name::#vname => None, },
        // Binding kind can never mismatch the field kind.
        _ => unreachable!("field-kind / binding mismatch in ErrorPosition derive"),
    }
}

/// Pick the canonical `Position` field for a variant, reproducing the
/// hand-written matchers' choice:
///
/// * an explicit `#[position]` field wins (and delegates when it is a nested
///   error rather than a `Position`);
/// * otherwise, among `Position`-typed fields: the sole one, else the field
///   named `pos`, else `position`, else the first.
///
/// Returns `None` when the variant carries no position.
fn select_position_field(fields: &Fields) -> Option<Selected> {
    let bound: Vec<(Binding, &Field)> = match fields {
        Fields::Named(f) => f
            .named
            .iter()
            .map(|fl| {
                (
                    Binding::Named(fl.ident.clone().expect("named field has an ident")),
                    fl,
                )
            })
            .collect(),
        Fields::Unnamed(f) => f
            .unnamed
            .iter()
            .enumerate()
            .map(|(i, fl)| (Binding::Indexed(i), fl))
            .collect(),
        Fields::Unit => return None,
    };

    // 1. Explicit `#[position]` override / delegation marker.
    if let Some((binding, field)) = bound
        .iter()
        .find(|(_, fl)| fl.attrs.iter().any(|a| a.path().is_ident("position")))
    {
        return Some(Selected {
            binding: binding.clone(),
            delegate: !type_is_position(&field.ty),
        });
    }

    // 2. Position-typed fields, with the naming preference for ties.
    let pos_fields: Vec<&(Binding, &Field)> = bound
        .iter()
        .filter(|(_, fl)| type_is_position(&fl.ty))
        .collect();

    let chosen = match pos_fields.as_slice() {
        [] => return None,
        [only] => only,
        many => {
            let named = |want: &str| {
                many.iter().copied().find(|(b, _)| match b {
                    Binding::Named(id) => id == want,
                    Binding::Indexed(_) => false,
                })
            };
            named("pos").or_else(|| named("position")).unwrap_or(many[0])
        }
    };

    Some(Selected {
        binding: chosen.0.clone(),
        delegate: false,
    })
}

/// A field is a position source when its type's final path segment is
/// `Position` (matching `Position`, `lexer::Position`, `crate::lexer::Position`).
fn type_is_position(ty: &Type) -> bool {
    matches!(ty, Type::Path(tp) if tp.path.segments.last().is_some_and(|seg| seg.ident == "Position"))
}
