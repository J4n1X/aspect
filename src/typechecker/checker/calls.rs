use super::TypeChecker;
use crate::lexer::{LangType, Position, TypeBase};
use crate::parser::Expression;
use crate::symbol::module::Visibility;
use crate::typechecker::errors::TypeCheckError;

impl TypeChecker {
    /// Synthesise every argument for error recovery, discarding the results —
    /// used after an arity/lookup failure so each argument's own errors surface.
    fn synth_all(&mut self, args: &mut [Expression]) {
        for arg in args.iter_mut() {
            self.synth_expression(arg);
        }
    }

    /// A private method is callable only from within its own type's methods.
    /// `name` is the mangled target (`Type$method`); a name with no `$` is an
    /// ordinary free function, always accessible.
    fn check_method_access(&mut self, name: &str, pos: Position) {
        let Some((type_name, method_name)) = name.split_once('$') else {
            return;
        };
        let Some(id) = self.symbols.struct_id(type_name) else {
            return;
        };
        let vis = match self.symbols.type_def(id).as_struct().methods.get(method_name) {
            Some(sig) => sig.vis,
            None => return,
        };
        if vis == Visibility::Private && !self.is_inside_struct_methods(id) {
            self.errors.push(TypeCheckError::InaccessibleMethod {
                method: method_name.to_string(),
                type_name: type_name.to_string(),
                position: pos,
            });
        }
    }

    /// Shared arity-check-then-zip for both direct and indirect calls: on a
    /// mismatch, pushes `ArgumentCountMismatch` and synthesises every argument
    /// for error recovery; otherwise checks each argument against its
    /// parameter type.
    fn check_call_args(
        &mut self,
        params: impl ExactSizeIterator<Item = LangType>,
        args: &mut [Expression],
        callee_name_for_error: &str,
        pos: Position,
    ) {
        if params.len() != args.len() {
            self.errors.push(TypeCheckError::ArgumentCountMismatch {
                name: callee_name_for_error.to_string(),
                expected: params.len(),
                found: args.len(),
                position: pos,
            });
            self.synth_all(args);
        } else {
            for (param_ty, arg_expr) in params.zip(args.iter_mut()) {
                self.check_expression(arg_expr, &param_ty);
            }
        }
    }

    /// Validates callee, arity, and argument types. Each argument is *checked*
    /// against its parameter type, pushing that type into literal arguments.
    pub(crate) fn check_call(&mut self, name: &str, args: &mut [Expression], pos: Position) {
        self.check_method_access(name, pos);
        if let Some(sig) = self.symbols.lookup_function(name).cloned() {
            self.check_call_args(sig.params.iter().map(|(t, _)| *t), args, name, pos);
        } else {
            self.errors
                .push(TypeCheckError::UndefinedFunction(name.to_string(), pos));
            self.synth_all(args);
        }
    }

    /// Synth the callee, validate it's a `FnPtr`, then `check` each arg
    /// against the declared parameter type (mirrors `check_call`).
    pub(crate) fn synth_indirect_call(
        &mut self,
        callee: &mut Expression,
        args: &mut [Expression],
        pos: Position,
    ) {
        let callee_type = self.synth_expression(callee);
        let sig_params: Option<Vec<LangType>> = match callee_type.base {
            TypeBase::FnPtr(id) if callee_type.pointer_depth == 0 => {
                Some(self.symbols.fnptr_sig(id).params.clone())
            }
            _ => {
                self.errors.push(TypeCheckError::TypeMismatch {
                    expected: LangType::VOID,
                    found: callee_type,
                    position: pos,
                });
                None
            }
        };
        if let Some(params) = sig_params {
            self.check_call_args(params.into_iter(), args, "<indirect call>", pos);
        } else {
            self.synth_all(args);
        }
    }
}
