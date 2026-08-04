use super::TypeChecker;
use crate::lexer::{LangType, Position};
use crate::parser::Expression;
use crate::symbol::module::Visibility;
use crate::typechecker::errors::TypeCheckError;

impl TypeChecker {
    pub(crate) fn synth_struct_literal(
        &mut self,
        struct_id: u32,
        fields: &mut [(String, Expression)],
        pos: Position,
    ) -> LangType {
        // Snapshot declared fields to avoid holding a `self.symbols`
        // borrow across the per-field `check_expression` calls.
        let declared: Vec<(String, LangType, Visibility)> = self
            .symbols
            .struct_info(struct_id)
            .fields
            .iter()
            .map(|f| (f.name.clone(), f.ty, f.vis))
            .collect();
        let type_name = self.symbols.struct_info(struct_id).name.clone();
        let inside_methods = self.is_inside_struct_methods(struct_id);

        let mut named: Vec<String> = Vec::with_capacity(fields.len());
        for (fname, fexpr) in fields.iter_mut() {
            named.push(fname.clone());
            if let Some((_, fty, vis)) = declared.iter().find(|(n, _, _)| n == fname) {
                let fty = *fty;
                if *vis == Visibility::Private && !inside_methods {
                    self.errors.push(TypeCheckError::InaccessibleField {
                        field: fname.clone(),
                        type_name: type_name.clone(),
                        position: pos,
                    });
                }
                self.check_expression(fexpr, &fty);
            } else {
                self.errors.push(TypeCheckError::UnknownField {
                    field: fname.clone(),
                    type_name: type_name.clone(),
                    position: pos,
                });
                self.synth_expression(fexpr);
            }
        }

        let missing: Vec<&str> = declared
            .iter()
            .map(|(n, _, _)| n.as_str())
            .filter(|n| !named.iter().any(|m| m == n))
            .collect();
        if !missing.is_empty() {
            self.errors.push(TypeCheckError::MissingStructFields {
                type_name,
                missing: missing.join(", "),
                position: pos,
            });
        }

        LangType::struct_type(struct_id)
    }

    pub(crate) fn synth_sum_construct(
        &mut self,
        sum_id: u32,
        variant: u32,
        args: &mut [Expression],
    ) -> LangType {
        // Snapshot the payload field types — same borrow dance as struct
        // literals (no `self.symbols` borrow across the per-argument
        // `check_expression` calls). Arity was enforced by the parser, so a
        // plain `zip` pairs them exactly.
        let field_tys: Vec<LangType> = self.symbols.sum_info(sum_id).variants[variant as usize]
            .fields
            .iter()
            .map(|(_, ty)| *ty)
            .collect();
        for (arg, fty) in args.iter_mut().zip(field_tys) {
            self.check_expression(arg, &fty);
        }
        LangType::sum_type(sum_id)
    }
}
