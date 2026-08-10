use std::rc::Rc;

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
        let syms = Rc::clone(&self.symbols);
        let declared = &syms.type_def(struct_id).as_struct().fields;
        let type_name = syms.type_def(struct_id).name.clone();
        let inside_methods = self.is_inside_struct_methods(struct_id);

        let mut named: Vec<String> = Vec::with_capacity(fields.len());
        for (fname, fexpr) in fields.iter_mut() {
            named.push(fname.clone());
            if let Some(declared) = declared.iter().find(|f| &f.name == fname) {
                let fty = declared.ty;
                if declared.vis == Visibility::Private && !inside_methods {
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
            .map(|f| f.name.as_str())
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
        // Arity was enforced by the parser, so a plain `zip` pairs exactly.
        let syms = Rc::clone(&self.symbols);
        let declared = &syms.type_def(sum_id).as_sum().variants[variant as usize].fields;
        for (arg, (_, fty)) in args.iter_mut().zip(declared) {
            self.check_expression(arg, fty);
        }
        LangType::sum_type(sum_id)
    }
}
