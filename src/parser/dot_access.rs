use crate::lexer::{LangType, Position, TokenKind, TypeBase};
use crate::parser::{ExprKind, Expression, Parser, ParserError};

impl Parser {
    /// The `.` was already consumed. Distinguishes `base.method(args)` (a
    /// method call desugared to `FunctionCall` with mangled name `Type$method`)
    /// from `base.field` (a `FieldAccess`).
    pub(crate) fn parse_dot_postfix(&mut self, base: Expression) -> Result<Expression, ParserError> {
        let pos = base.pos;
        let name = self.parse_ident("field or method name")?;

        // Method call only when `name` is actually a method of the base's type;
        // otherwise (e.g. `.callback(` on a fn-pointer *field*) fall through to
        // field access and let the postfix loop emit an indirect call.
        if self.check(&TokenKind::OpenParen) && self.identifier_is_method_of_base(&base, &name) {
            self.advance();
            let args = self.parse_comma_separated(&TokenKind::CloseParen, Self::parse_expression)?;
            return self.build_method_call(base, &name, args, pos);
        }

        if let Some(result) = self.try_enum_variant_value(&base, &name, pos) {
            return result;
        }
        if let Some(result) = self.try_sum_construct(&base, &name, pos) {
            return result;
        }
        if let Some(result) = self.try_static_method_ref(&base, &name, pos) {
            return result;
        }

        let field_type = match base.expr_type.base {
            TypeBase::Struct(id) => self
                .module
                .field(id, &name)
                .map_or_else(|| LangType::VOID, |(_, f)| f.ty),
            _ => LangType::VOID,
        };

        Ok(Expression::new(
            ExprKind::FieldAccess {
                base: Box::new(base),
                field: name,
            },
            field_type,
            pos,
        ))
    }

    /// `base` names a known top-level item (struct/enum/sum) only when it is
    /// a bare identifier not shadowed by a local variable — the guard shared
    /// by all four `TypeName.foo` resolution sites (enum-variant value,
    /// sum-construct, static-method-value, static-call).
    fn unshadowed_named_ref<'a>(&self, base: &'a Expression) -> Option<&'a str> {
        let ExprKind::Variable(var_name) = &base.kind else {
            return None;
        };
        if self.symbol_table.lookup_variable(var_name).is_some() {
            return None;
        }
        Some(var_name)
    }

    /// Enum variant value `EnumName.Variant`: resolves to a compile-time
    /// constant (the variant's index). `None` when `base` doesn't name a
    /// known enum, letting the caller fall through to the next trial.
    fn try_enum_variant_value(
        &self,
        base: &Expression,
        name: &str,
        pos: Position,
    ) -> Option<Result<Expression, ParserError>> {
        let var_name = self.unshadowed_named_ref(base)?;
        let id = self.module.enum_id(var_name)?;
        if let Err(e) = self.check_enum_visibility(id, pos) {
            return Some(Err(e));
        }
        match self.module.enum_variant_index(id, name) {
            Some(idx) => {
                let ty = LangType::enum_type(id);
                Some(Ok(Expression::new(
                    ExprKind::EnumValue {
                        enum_id: id,
                        value: idx as i64,
                    },
                    ty,
                    pos,
                )))
            }
            None => {
                let enum_name = self.module.enum_info(id).name.clone();
                Some(Err(ParserError::UnknownVariant {
                    enum_name,
                    variant: name.to_string(),
                    pos,
                }))
            }
        }
    }

    /// Sum construction `SumName.Variant(args…)` / bare `SumName.Variant`:
    /// the variant is resolved (and arity checked) here, like enum variants;
    /// argument *types* are the checker's job. `None` when `base` doesn't
    /// name a known sum.
    fn try_sum_construct(
        &mut self,
        base: &Expression,
        name: &str,
        pos: Position,
    ) -> Option<Result<Expression, ParserError>> {
        let var_name = self.unshadowed_named_ref(base)?;
        let id = self.module.sum_id(var_name)?;
        if let Err(e) = self.check_sum_visibility(id, pos) {
            return Some(Err(e));
        }
        let sum_name = self.module.sum_info(id).name.clone();
        let Some(idx) = self.module.sum_variant_index(id, name) else {
            return Some(Err(ParserError::UnknownSumVariant {
                sum_name,
                variant: name.to_string(),
                pos,
            }));
        };
        let field_count = self.module.sum_info(id).variants[idx].fields.len();
        let ty = LangType::sum_type(id);
        let variant = u32::try_from(idx).expect("variant index fits u32");

        if self.match_token(&[TokenKind::OpenParen]) {
            if field_count == 0 {
                return Some(Err(ParserError::UnexpectedToken(
                    format!(
                        "variant '{name}' of sum '{sum_name}' carries no payload — construct it as a bare name: {sum_name}.{name}"
                    ),
                    pos,
                )));
            }
            let args = match self.parse_comma_separated(&TokenKind::CloseParen, Self::parse_expression) {
                Ok(args) => args,
                Err(e) => return Some(Err(e)),
            };
            if args.len() != field_count {
                return Some(Err(ParserError::ArgumentCountMismatch(
                    format!("{sum_name}.{name}"),
                    field_count,
                    args.len(),
                    pos,
                )));
            }
            return Some(Ok(Expression::new(
                ExprKind::SumConstruct {
                    sum_id: id,
                    variant,
                    args,
                },
                ty,
                pos,
            )));
        }
        if field_count > 0 {
            return Some(Err(ParserError::UnexpectedToken(
                format!(
                    "variant '{name}' of sum '{sum_name}' carries a payload — construct it with arguments: {sum_name}.{name}(…)"
                ),
                pos,
            )));
        }
        Some(Ok(Expression::new(
            ExprKind::SumConstruct {
                sum_id: id,
                variant,
                args: Vec::new(),
            },
            ty,
            pos,
        )))
    }

    /// Static method as a function-pointer *value*: `Type.method` with no
    /// following call. Typed from the mangled function's *actual* signature
    /// — whose first parameter is already the receiver `Type*` — so the
    /// value is `fn(Type*, ...) -> R`. The bound form (`instance.method` as
    /// a value) is out of scope and falls through to field access. `None`
    /// when `base` doesn't name a struct with a method by this name.
    fn try_static_method_ref(
        &mut self,
        base: &Expression,
        name: &str,
        pos: Position,
    ) -> Option<Result<Expression, ParserError>> {
        let var_name = self.unshadowed_named_ref(base)?;
        let id = self.module.struct_id(var_name)?;
        if !self.module.struct_info(id).methods.contains_key(name) {
            return None;
        }
        if let Err(e) = self.check_struct_visibility(id, pos) {
            return Some(Err(e));
        }
        let type_name = self.module.struct_info(id).name.clone();
        let mangled = crate::symbol::module::mangle_method(&type_name, name);
        let (params, return_type) = self.module.lookup_function(&mangled).map_or_else(
            || (Vec::new(), LangType::VOID),
            |f| {
                (
                    f.params.iter().map(|(t, _)| *t).collect::<Vec<_>>(),
                    f.return_type,
                )
            },
        );
        let fnptr_id = self.module.intern_fnptr(params, return_type);
        let ty = LangType::fnptr_type(fnptr_id);
        Some(Ok(Expression::new(ExprKind::FunctionRef(mangled), ty, pos)))
    }

    /// Build a method-call expression for `obj.method(args)` or
    /// `Type.method(args)`. Resolves the mangled name (`Type$method`), picks
    /// instance-vs-static, and autorefs value receivers.
    pub(crate) fn build_method_call(
        &mut self,
        base: Expression,
        method_name: &str,
        args: Vec<Expression>,
        pos: Position,
    ) -> Result<Expression, ParserError> {
        // Static call: `TypeName.method(args)` — `base` is `Variable(TypeName)`
        // for a known struct *and* there is no local variable shadowing it.
        if let Some(var_name) = self.unshadowed_named_ref(&base)
            && let Some(id) = self.module.struct_id(var_name)
        {
            return self.build_static_method_call(id, method_name, args, pos);
        }

        self.build_instance_method_call(base, method_name, args, pos)
    }

    fn build_static_method_call(
        &mut self,
        id: u32,
        method_name: &str,
        args: Vec<Expression>,
        pos: Position,
    ) -> Result<Expression, ParserError> {
        let (mangled, return_type) = self.resolve_method_target(id, method_name, true, pos)?;
        Ok(Expression::new(
            ExprKind::FunctionCall {
                name: mangled,
                args,
            },
            return_type,
            pos,
        ))
    }

    fn build_instance_method_call(
        &mut self,
        base: Expression,
        method_name: &str,
        args: Vec<Expression>,
        pos: Position,
    ) -> Result<Expression, ParserError> {
        let bt = base.expr_type;
        let id = match bt.base {
            TypeBase::Struct(id) => id,
            _ => {
                return Err(ParserError::TypeMismatch(
                    "type-struct".to_string(),
                    format!("{bt}"),
                    pos,
                ));
            }
        };
        let (mangled, return_type) = self.resolve_method_target(id, method_name, false, pos)?;
        let receiver = autoref_receiver(base, pos)?;

        let mut all_args = Vec::with_capacity(args.len() + 1);
        all_args.push(receiver);
        all_args.extend(args);

        Ok(Expression::new(
            ExprKind::FunctionCall {
                name: mangled,
                args: all_args,
            },
            return_type,
            pos,
        ))
    }

    /// Resolves `id.method_name` to its mangled free-function name and return
    /// type. A private type's methods are at most module-visible, however
    /// `public` the member itself is, so this is gated on the type's own
    /// visibility exactly like naming it. `want_static` enforces the strict
    /// static/instance split: a static method (no `this`) must be called as
    /// `Type.method(...)`, an instance method as `<receiver>.method(...)` —
    /// each syntactic form must resolve to its matching kind.
    fn resolve_method_target(
        &mut self,
        id: u32,
        method_name: &str,
        want_static: bool,
        pos: Position,
    ) -> Result<(String, LangType), ParserError> {
        self.check_struct_visibility(id, pos)?;
        let type_name = self.module.struct_info(id).name.clone();
        if let Some(sig) = self.module.struct_info(id).methods.get(method_name)
            && sig.is_static != want_static
        {
            let msg = if sig.is_static {
                format!(
                    "'{type_name}.{method_name}' is a static method; \
                     call it as `{type_name}.{method_name}(...)` without a receiver"
                )
            } else {
                format!(
                    "'{type_name}.{method_name}' is an instance method; \
                     call it as `<receiver>.{method_name}(...)`"
                )
            };
            return Err(ParserError::MethodCallForm(msg, pos));
        }
        let mangled = crate::symbol::module::mangle_method(&type_name, method_name);
        let return_type = self
            .module
            .lookup_function(&mangled)
            .map_or(LangType::VOID, |f| f.return_type);
        Ok((mangled, return_type))
    }
}

/// Receiver of an instance method call: autoref a value, pass a pointer
/// as-is; deeper pointers fail. `pos` is the call site, not `base.pos` —
/// they differ when the receiver is a sub-expression like a field access.
fn autoref_receiver(base: Expression, pos: Position) -> Result<Expression, ParserError> {
    let bt = base.expr_type;
    match bt.pointer_depth {
        0 => {
            let ref_ty = LangType {
                base: bt.base,
                size_bits: bt.size_bits,
                pointer_depth: 1,
                is_const: bt.is_const,
                array_size: None,
            };
            let base_pos = base.pos;
            Ok(Expression::new(ExprKind::Reference(Box::new(base)), ref_ty, base_pos))
        }
        1 => Ok(base),
        _ => Err(ParserError::TypeMismatch(
            "type-struct or pointer-to-type-struct".to_string(),
            format!("{bt}"),
            pos,
        )),
    }
}
