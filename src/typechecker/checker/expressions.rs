use super::TypeChecker;
use crate::lexer::{LangType, TypeBase};
use crate::parser::{BinaryOp, ComparisonOp, ExprKind, Expression, LiteralValue};
use crate::symbol::module::Visibility;
use crate::typechecker::elaborate::Obligation;
use crate::typechecker::errors::TypeCheckError;
use crate::typechecker::types::{
    cast_valid, literal_float_compatible, literal_int_fits, types_coercible,
};

impl TypeChecker {
    /// Synthesise every argument for error recovery, discarding the results —
    /// used after an arity/lookup failure so each argument's own errors surface.
    fn synth_all(&mut self, args: &mut [Expression]) {
        for arg in args.iter_mut() {
            self.synth_expression(arg);
        }
    }

    /// Synthesise the type of `expr` with no contextual expectation (callee
    /// resolution, indices, conditions, cast/dereference operands).
    pub(crate) fn synth_expression(&mut self, expr: &mut Expression) -> LangType {
        let pos = expr.pos;
        // `MethodCall` is rewritten *before* the match because it replaces the
        // whole node — which a `match &mut expr.kind` arm cannot do. The
        // checker's one in-place lowering; see `resolve_method_call`.
        if matches!(expr.kind, ExprKind::MethodCall { .. }) {
            return self.resolve_method_call(expr);
        }
        let default_type = expr.expr_type;
        match &mut expr.kind {
            ExprKind::Literal(_) => default_type,

            // Synthesises to exactly the enum type, never a bare integer —
            // which is what keeps enum ↔ int implicit coercion impossible.
            ExprKind::EnumValue { enum_id, .. } => {
                let ty = LangType::enum_type(*enum_id);
                expr.expr_type = ty;
                ty
            }

            ExprKind::Variable(name) => {
                if let Some(ty) = self.lookup_var(name) {
                    ty
                } else {
                    self.errors
                        .push(TypeCheckError::UndefinedVariable(name.clone(), pos));
                    default_type
                }
            }

            ExprKind::Binary { left, op, right } => {
                let left_type = self.synth_expression(left);
                let right_type = self.synth_expression(right);

                if !Self::binary_op_types_valid(&left_type, &right_type, op) {
                    self.errors.push(TypeCheckError::InvalidBinaryOperation {
                        operator: format!("{op:?}"),
                        left: left_type,
                        right: right_type,
                        position: pos,
                    });
                }
                if matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr) {
                    // Logical `&&`/`||` yield a boolean regardless of operand type.
                    let bool_ty = LangType::BOOL;
                    expr.expr_type = bool_ty;
                    bool_ty
                } else {
                    Self::wider_type(&left_type, &right_type)
                }
            }

            ExprKind::Comparison { left, op, right } => {
                let left_type = self.synth_expression(left);
                let right_type = self.synth_expression(right);

                // Enums compare only to the *same* enum, and only `==`/`!=`
                // (there is no ordering). Enum-vs-int or enum-vs-other-enum is a
                // type error — the point of a distinct enum type.
                let valid = if matches!(left_type.base, TypeBase::Enum(_))
                    || matches!(right_type.base, TypeBase::Enum(_))
                {
                    matches!(
                        (left_type.base, right_type.base),
                        (TypeBase::Enum(a), TypeBase::Enum(b)) if a == b
                    ) && left_type.pointer_depth == 0
                        && right_type.pointer_depth == 0
                        && matches!(op, ComparisonOp::Equal | ComparisonOp::NotEqual)
                } else {
                    Self::comparison_operands_valid(&left_type, &right_type)
                };
                if !valid {
                    self.errors.push(TypeCheckError::InvalidBinaryOperation {
                        operator: "comparison".to_string(),
                        left: left_type,
                        right: right_type,
                        position: pos,
                    });
                }
                // A literal operand may adopt its *sibling's* narrower integer
                // type so codegen compares at that width instead of widening
                // both to the literal's default `i32`. The boolean result is
                // unaffected since the literal fits the sibling's exact type.
                Self::narrow_literal_to_sibling(left, right_type);
                Self::narrow_literal_to_sibling(right, left_type);
                let bool_ty = LangType::BOOL;
                expr.expr_type = bool_ty;
                bool_ty
            }

            ExprKind::Reference(inner) => {
                self.synth_expression(inner);
                default_type
            }

            ExprKind::Dereference(inner) => {
                let inner_type = self.synth_expression(inner);
                // Arrays and pointers are both valid dereference targets.
                // Array subscript `arr[i]` is lowered to `*(arr + i)` by the
                // parser, so array types must be accepted here.
                if !inner_type.is_pointer_like() {
                    self.errors
                        .push(TypeCheckError::InvalidDereference(inner_type, pos));
                }
                // `u0*` is opaque: its pointee is a void value, so it cannot be
                // dereferenced (or subscripted) without a cast to a sized
                // pointer first.
                if inner_type.is_opaque_ptr() {
                    self.errors.push(TypeCheckError::OpaqueDereference(pos));
                }
                // Recompute the pointee from the synthesized inner type, not the
                // parser's best-effort stamp: only the checker knows the base's
                // const-ness, so a stale stamp would let `*const_ptr = x` slip
                // past the write-through-const check. Const propagates downward.
                let result = if inner_type.is_array() {
                    inner_type.element_type()
                } else if inner_type.pointer_depth > 0 {
                    inner_type.with_pointer_depth(inner_type.pointer_depth - 1)
                } else {
                    default_type
                };
                expr.expr_type = result;
                result
            }

            ExprKind::FunctionCall { name, args } => {
                self.check_call(name, args, pos);
                default_type
            }

            ExprKind::Cast {
                expr: inner,
                target_type,
            } => {
                let from_type = self.synth_expression(inner);
                if !cast_valid(&from_type, target_type) {
                    self.errors.push(TypeCheckError::InvalidCast {
                        from: from_type,
                        to: *target_type,
                        position: pos,
                    });
                }
                *target_type
            }

            ExprKind::Alloc { alloc_type, count } => {
                self.reject_void_value(*alloc_type, pos);
                let count_pos = count.pos;
                let count_type = self.synth_expression(count);
                if !matches!(count_type.base, TypeBase::SInt | TypeBase::UInt)
                    || count_type.pointer_depth > 0
                {
                    self.errors.push(TypeCheckError::TypeMismatch {
                        expected: LangType::U64,
                        found: count_type,
                        position: count_pos,
                    });
                }
                default_type
            }

            ExprKind::UnaryNot(inner) => {
                let inner_type = self.synth_expression(inner);
                // `!p` is a null test and works for any pointer, `u0*`
                // included; only void *values* are rejected.
                if inner_type.is_void_value() {
                    self.errors.push(TypeCheckError::InvalidUnaryOperation {
                        operator: "!".to_string(),
                        operand: inner_type,
                        position: pos,
                    });
                }
                let bool_ty = LangType::BOOL;
                expr.expr_type = bool_ty;
                bool_ty
            }

            ExprKind::BitwiseNot(inner) => {
                let inner_type = self.synth_expression(inner);
                // Bit-twiddling an opaque pointer deserves an explicit cast,
                // so `u0*` stays rejected here (unlike `!` above).
                if inner_type.base == TypeBase::Void {
                    self.errors.push(TypeCheckError::InvalidUnaryOperation {
                        operator: "~".to_string(),
                        operand: inner_type,
                        position: pos,
                    });
                }
                default_type
            }

            ExprKind::ListInitializer(elements) => {
                for elem in elements.iter_mut() {
                    self.synth_expression(elem);
                }
                default_type
            }

            ExprKind::FieldAccess { base, field } => {
                let base_type = self.synth_expression(base);
                let field = field.clone();
                let field_type = self.resolve_field(&base_type, &field, pos);
                expr.expr_type = field_type;
                field_type
            }

            ExprKind::StructLiteral { struct_id, fields } => {
                let struct_id = *struct_id;
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
                    if let Some((_, fty, vis)) =
                        declared.iter().find(|(n, _, _)| n == fname)
                    {
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

                let struct_ty = LangType::struct_type(struct_id);
                expr.expr_type = struct_ty;
                struct_ty
            }

            // The parser stamped the FnPtr type; nothing to check. An unknown
            // name would have stayed `Variable` with a `void` stamp.
            ExprKind::FunctionRef(_) => default_type,

            // Synth the callee, validate it's a `FnPtr`, then `check` each arg
            // against the declared parameter type (mirrors `check_call`).
            ExprKind::IndirectCall { callee, args } => {
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
                    if params.len() != args.len() {
                        self.errors.push(TypeCheckError::ArgumentCountMismatch {
                            name: "<indirect call>".to_string(),
                            expected: params.len(),
                            found: args.len(),
                            position: pos,
                        });
                        self.synth_all(args);
                    } else {
                        for (pty, arg) in params.iter().zip(args.iter_mut()) {
                            self.check_expression(arg, pty);
                        }
                    }
                } else {
                    self.synth_all(args);
                }
                default_type
            }

            // `sizeof(T)` is a compile-time `u64` constant; the parser
            // already stamped the expression type at construction.
            ExprKind::SizeOf(_) => default_type,

            // `null` carries the parser-stamped `u8*` placeholder when used in
            // a context that doesn't constrain its type (e.g. `null == p`).
            // Pointer-to-pointer coercion handles the rest at the boundary.
            ExprKind::Null => default_type,

            // Value-block with no contextual target: the first `return`
            // inside synthesizes the block's type, later ones check against
            // it (see the `Return` arm of `check_statement`).
            ExprKind::ValueBlock(stmts) => {
                let ty = self.check_value_block(stmts, None, pos);
                expr.expr_type = ty;
                ty
            }

            // Resolved-and-rewritten before this match (see the guard at the
            // top of `synth_expression`), so the node is never a `MethodCall`
            // by the time control reaches here.
            ExprKind::MethodCall { .. } => unreachable!("MethodCall resolved before the match"),
        }
    }

    /// Emits an error and returns a `void` placeholder when the base is not a
    /// type-struct or the field is unknown. A single-level pointer-to-struct
    /// auto-dereferences.
    fn resolve_field(
        &mut self,
        base_type: &LangType,
        field: &str,
        pos: crate::lexer::Position,
    ) -> LangType {
        // A poisoned base has no fields to resolve; propagate the sentinel
        // instead of a spurious `UnknownField`.
        if base_type.base == TypeBase::Unresolved {
            return LangType::UNRESOLVED;
        }
        if let TypeBase::Struct(id) = base_type.base
            && base_type.pointer_depth <= 1
        {
            if let Some((_, finfo)) = self.symbols.field(id, field) {
                let vis = finfo.vis;
                // A const struct (or `*const Struct`) propagates const-ness
                // to its fields, so assignment-through `this.field = ...` in a
                // `const fn` body lands on the existing AssignmentToConst path.
                let mut fty = finfo.ty;
                if base_type.is_const {
                    fty.is_const = true;
                }
                // Private fields are accessible only from the type's own
                // methods (M4 encapsulation).
                if vis == Visibility::Private && !self.is_inside_struct_methods(id) {
                    let type_name = self.type_name(base_type);
                    self.errors.push(TypeCheckError::InaccessibleField {
                        field: field.to_string(),
                        type_name,
                        position: pos,
                    });
                }
                return fty;
            }
            let type_name = self.type_name(base_type);
            self.errors.push(TypeCheckError::UnknownField {
                field: field.to_string(),
                type_name,
                position: pos,
            });
            return LangType::VOID;
        }
        self.errors.push(TypeCheckError::NotAStruct {
            found: *base_type,
            position: pos,
        });
        LangType::VOID
    }

    /// `true` when the function being checked is a method of the given
    /// type-struct (its mangled name begins with `"<TypeName>$"`).
    fn is_inside_struct_methods(&self, struct_id: u32) -> bool {
        let Some(current) = self.current_function.as_deref() else {
            return false;
        };
        let prefix =
            crate::symbol::module::method_owner_prefix(&self.symbols.struct_info(struct_id).name);
        current.starts_with(&prefix)
    }

    /// Human-readable name for a type, resolving type-struct ids to their
    /// declared names (which `LangType`'s `Display` cannot reach).
    pub(crate) fn type_name(&self, ty: &LangType) -> String {
        if let TypeBase::Struct(id) = ty.base {
            let stars = "*".repeat(ty.pointer_depth as usize);
            format!("{}{}", self.symbols.struct_info(id).name, stars)
        } else if let TypeBase::Enum(id) = ty.base {
            let stars = "*".repeat(ty.pointer_depth as usize);
            format!("{}{}", self.symbols.enum_info(id).name, stars)
        } else {
            format!("{ty}")
        }
    }

    /// A private method is callable only from within its own type's methods.
    /// `name` is the mangled target (`Type$method`); a name with no `$` is an
    /// ordinary free function, always accessible.
    fn check_method_access(&mut self, name: &str, pos: crate::lexer::Position) {
        let Some((type_name, method_name)) = name.split_once('$') else {
            return;
        };
        let Some(id) = self.symbols.struct_id(type_name) else {
            return;
        };
        let vis = match self.symbols.struct_info(id).methods.get(method_name) {
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

    /// Validates callee, arity, and argument types. Each argument is *checked*
    /// against its parameter type, pushing that type into literal arguments.
    fn check_call(
        &mut self,
        name: &str,
        args: &mut [Expression],
        pos: crate::lexer::Position,
    ) {
        self.check_method_access(name, pos);
        if let Some(sig) = self.symbols.lookup_function(name).cloned() {
            if sig.params.len() != args.len() {
                self.errors.push(TypeCheckError::ArgumentCountMismatch {
                    name: name.to_string(),
                    expected: sig.params.len(),
                    found: args.len(),
                    position: pos,
                });
                // Still synthesise the arguments so their own errors surface.
                self.synth_all(args);
            } else {
                for ((param_ty, _), arg_expr) in sig.params.iter().zip(args.iter_mut()) {
                    self.check_expression(arg_expr, param_ty);
                }
            }
        } else {
            self.errors
                .push(TypeCheckError::UndefinedFunction(name.to_string(), pos));
            self.synth_all(args);
        }
    }

    /// Rewrite an `ExprKind::MethodCall` in place into a `FunctionCall` (method)
    /// or `IndirectCall` (fn-pointer field), then return its type by re-checking.
    ///
    /// Exists so metaprogram-generated AST (with no parse-time receiver types)
    /// can defer method dispatch to type-checking. A *one-shot* lowering — the
    /// result is a plain call, so re-checking is stable. The per-method privacy
    /// gate is enforced for free via the rewritten `FunctionCall` → `check_call`;
    /// the `public type` cross-module gate is deliberately **not** reproduced
    /// (the checker has no `file_id → module` map), an accepted carve-out for
    /// metaprogram-generated code.
    fn resolve_method_call(&mut self, expr: &mut Expression) -> LangType {
        let pos = expr.pos;
        let (base, name, args) = match std::mem::replace(&mut expr.kind, ExprKind::Null) {
            ExprKind::MethodCall { base, name, args } => (*base, name, args),
            _ => unreachable!("resolve_method_call called on a non-MethodCall node"),
        };
        *expr = self.build_method_call_node(base, name, args, pos);
        // Re-check the rewritten node: `FunctionCall` runs `check_call`
        // (arity/arg types + the per-method gate), `IndirectCall` validates the
        // callee signature.
        self.synth_expression(expr)
    }

    /// Build the resolved call `Expression` for `base.name(args)`. Returns a
    /// `FunctionCall` (static or instance method), an `IndirectCall` (fn-pointer
    /// field), or a `void` `Null` placeholder after pushing a diagnostic.
    fn build_method_call_node(
        &mut self,
        mut base: Expression,
        name: String,
        args: Vec<Expression>,
        pos: crate::lexer::Position,
    ) -> Expression {
        // Static form: `TypeName.method(args)` — `base` is `Variable(TypeName)`
        // naming a known type-struct not shadowed by a local.
        if let ExprKind::Variable(var_name) = &base.kind
            && let Some(id) = self.symbols.struct_id(var_name)
            && self.lookup_var(var_name).is_none()
        {
            return self.build_static_method_call(id, &name, args, pos);
        }

        // Instance form: synth the receiver, which must be a type-struct value
        // or single-level pointer-to-struct.
        let base_type = self.synth_expression(&mut base);
        let TypeBase::Struct(id) = base_type.base else {
            self.errors.push(TypeCheckError::InvalidMethodReceiver {
                found: base_type,
                position: pos,
            });
            return Expression::new(ExprKind::Null, LangType::VOID, pos);
        };
        let type_name = self.symbols.struct_info(id).name.clone();

        // Method vs fn-pointer field. Snapshot the needed facts before any
        // `self.errors` borrow.
        let method_is_static = self
            .symbols
            .struct_info(id)
            .methods
            .get(&name)
            .map(|sig| sig.is_static);

        if let Some(is_static) = method_is_static {
            // An instance call must resolve to an instance method.
            if is_static {
                self.errors.push(TypeCheckError::MethodCallForm {
                    message: format!(
                        "'{type_name}.{name}' is a static method; call it as \
                         `{type_name}.{name}(...)` without a receiver"
                    ),
                    position: pos,
                });
                return Expression::new(ExprKind::Null, LangType::VOID, pos);
            }
            let mangled = crate::symbol::module::mangle_method(&type_name, &name);
            let return_type = self
                .symbols
                .lookup_function(&mangled)
                .map_or(LangType::VOID, |f| f.return_type);
            // Autoref a value, pass a pointer as-is, reject deeper pointers.
            // Const propagates into the reference type, so a mutating method on
            // a const receiver is rejected downstream by `check_call`.
            let receiver = match base_type.pointer_depth {
                0 => {
                    let ref_ty = base_type.with_pointer_depth(1);
                    let base_pos = base.pos;
                    Expression::new(ExprKind::Reference(Box::new(base)), ref_ty, base_pos)
                }
                1 => base,
                _ => {
                    self.errors.push(TypeCheckError::InvalidMethodReceiver {
                        found: base_type,
                        position: pos,
                    });
                    return Expression::new(ExprKind::Null, LangType::VOID, pos);
                }
            };
            let mut all_args = Vec::with_capacity(args.len() + 1);
            all_args.push(receiver);
            all_args.extend(args);
            return Expression::new(
                ExprKind::FunctionCall {
                    name: mangled,
                    args: all_args,
                },
                return_type,
                pos,
            );
        }

        // Not a method: `name` may be a fn-pointer *field*, callable through an
        // indirect call. Anything else is a diagnostic.
        let field_ty = self.symbols.field(id, &name).map(|(_, f)| f.ty);
        match field_ty {
            Some(fty) if matches!(fty.base, TypeBase::FnPtr(_)) && fty.pointer_depth == 0 => {
                let TypeBase::FnPtr(fid) = fty.base else {
                    unreachable!("guarded by the match arm")
                };
                let return_type = self.symbols.fnptr_sig(fid).return_type;
                let base_pos = base.pos;
                let field_access = Expression::new(
                    ExprKind::FieldAccess {
                        base: Box::new(base),
                        field: name,
                    },
                    fty,
                    base_pos,
                );
                Expression::new(
                    ExprKind::IndirectCall {
                        callee: Box::new(field_access),
                        args,
                    },
                    return_type,
                    pos,
                )
            }
            Some(fty) => {
                self.errors.push(TypeCheckError::NotCallable {
                    name,
                    type_name,
                    found: fty,
                    position: pos,
                });
                Expression::new(ExprKind::Null, LangType::VOID, pos)
            }
            None => {
                self.errors.push(TypeCheckError::UnknownField {
                    field: name,
                    type_name,
                    position: pos,
                });
                Expression::new(ExprKind::Null, LangType::VOID, pos)
            }
        }
    }

    /// Resolve a static-form `Type.method(args)` call node.
    fn build_static_method_call(
        &mut self,
        id: u32,
        name: &str,
        args: Vec<Expression>,
        pos: crate::lexer::Position,
    ) -> Expression {
        let type_name = self.symbols.struct_info(id).name.clone();
        let method_is_static = self
            .symbols
            .struct_info(id)
            .methods
            .get(name)
            .map(|sig| sig.is_static);
        match method_is_static {
            // A static call must resolve to a static method.
            Some(false) => {
                self.errors.push(TypeCheckError::MethodCallForm {
                    message: format!(
                        "'{type_name}.{name}' is an instance method; call it as \
                         `<receiver>.{name}(...)`"
                    ),
                    position: pos,
                });
                Expression::new(ExprKind::Null, LangType::VOID, pos)
            }
            Some(true) => {
                let mangled = crate::symbol::module::mangle_method(&type_name, name);
                let return_type = self
                    .symbols
                    .lookup_function(&mangled)
                    .map_or(LangType::VOID, |f| f.return_type);
                Expression::new(
                    ExprKind::FunctionCall {
                        name: mangled,
                        args,
                    },
                    return_type,
                    pos,
                )
            }
            None => {
                self.errors.push(TypeCheckError::UnknownField {
                    field: name.to_string(),
                    type_name,
                    position: pos,
                });
                Expression::new(ExprKind::Null, LangType::VOID, pos)
            }
        }
    }

    /// Stamps `expr.expr_type` and pushes `target` into children where the
    /// child's type *is* the parent's (arithmetic operands, bitwise-not,
    /// reference/dereference, list elements). Emits a single `TypeMismatch` (or
    /// a more specific literal-fit error) on failure.
    pub(crate) fn check_expression(&mut self, expr: &mut Expression, target: &LangType) {
        let pos = expr.pos;
        match &mut expr.kind {
            ExprKind::Literal(LiteralValue::Integer(val)) => {
                let val = *val;
                if literal_int_fits(val, target) {
                    expr.expr_type = *target;
                } else if !types_coercible(&expr.expr_type, target) {
                    self.errors.push(TypeCheckError::TypeMismatch {
                        expected: *target,
                        found: expr.expr_type,
                        position: pos,
                    });
                }
            }

            ExprKind::Literal(LiteralValue::Float(_)) => {
                if literal_float_compatible(target) {
                    expr.expr_type = *target;
                } else if !types_coercible(&expr.expr_type, target) {
                    self.errors.push(TypeCheckError::TypeMismatch {
                        expected: *target,
                        found: expr.expr_type,
                        position: pos,
                    });
                }
            }

            ExprKind::Literal(LiteralValue::String(_)) => {
                self.assert_coercible(expr.expr_type, target, pos);
            }

            // Plain-numeric target: propagate it into both operands. Logical
            // `&&`/`||` are excluded — they yield a boolean, so they fall
            // through to the synth arm below.
            ExprKind::Binary { left, op, right }
                if target.is_plain_numeric()
                    && !matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr) =>
            {
                self.check_expression(left, target);
                self.check_expression(right, target);
                let left_type = left.expr_type;
                let right_type = right.expr_type;
                if !Self::binary_op_types_valid(&left_type, &right_type, op) {
                    self.errors.push(TypeCheckError::InvalidBinaryOperation {
                        operator: format!("{op:?}"),
                        left: left_type,
                        right: right_type,
                        position: pos,
                    });
                }
                expr.expr_type = *target;
            }

            // Bitwise-not preserves its operand type: propagate the target inward.
            ExprKind::BitwiseNot(inner) => {
                self.check_expression(inner, target);
                let inner_type = inner.expr_type;
                if inner_type.base == TypeBase::Void {
                    self.errors.push(TypeCheckError::InvalidUnaryOperation {
                        operator: "~".to_string(),
                        operand: inner_type,
                        position: pos,
                    });
                }
                expr.expr_type = *target;
            }

            // A Reference may produce a const-pointer to a non-const value
            // (`const T* p = &t`), so the inner need not carry the pointee's
            // const-ness.
            ExprKind::Reference(inner) => {
                // Against an opaque `u0*` target the pointee is `u0`, which no
                // value has — synthesise instead (any `&lvalue` coerces to u0*).
                let opaque_target =
                    target.base == TypeBase::Void && target.pointer_depth == 1;
                if target.pointer_depth > 0 && !opaque_target {
                    let mut inner_target = *target;
                    inner_target.pointer_depth -= 1;
                    inner_target.is_const = false;
                    self.check_expression(inner, &inner_target);
                } else {
                    self.synth_expression(inner);
                }
                self.assert_coercible(expr.expr_type, target, pos);
            }

            // Dereference: synthesise (the operand is a pointer/array, not the
            // target type), then assert the produced type is coercible.
            ExprKind::Dereference(_) => {
                let found = self.synth_expression(expr);
                self.assert_coercible(found, target, pos);
            }

            // Value-block in a checked position: the target is pushed into
            // every `return` inside the block, and the block adopts it.
            ExprKind::ValueBlock(stmts) => {
                self.check_value_block(stmts, Some(*target), pos);
                expr.expr_type = *target;
            }

            ExprKind::ListInitializer(elements) => {
                let elem_target = target.element_type();
                for elem in elements.iter_mut() {
                    self.check_expression(elem, &elem_target);
                }
            }

            // `null` adopts the context's pointer type: the parser's `u8*`
            // placeholder only survives in synth position, so upgrade it here
            // (structural depth otherwise blocks it from coercing into a deeper
            // or function pointer). A non-pointer/array target falls through to
            // the coercibility check, which rejects it.
            ExprKind::Null
                if !target.is_array()
                    && (target.pointer_depth > 0
                        || matches!(target.base, TypeBase::FnPtr(_))) =>
            {
                expr.expr_type = *target;
            }

            // Everything else: the expression's type is not the target's, so
            // synthesise and assert coercibility at the boundary.
            _ => {
                let found = self.synth_expression(expr);
                // A failed coercion is a repair demand site: consult a transform
                // handler before erroring.
                if !types_coercible(&found, target)
                    && let Some(rewrite) =
                        self.try_repair(&Obligation::Coerce { from: found, to: *target })
                {
                    *expr = rewrite; // re-checked next round; obligation discharged
                    return;
                }
                self.assert_coercible(found, target, pos);
            }
        }
    }

    /// Consult a transform handler to repair a stuck demand site, returning a
    /// rewritten node if a handler claims the obligation. Returns `None` when
    /// none does, and the caller falls back to erroring.
    fn try_repair(&mut self, obl: &Obligation) -> Option<Expression> {
        if self.handlers.is_empty() {
            return None;
        }
        // No handler dispatch yet; the registry is never populated, so this is
        // currently unreachable.
        let _ = obl;
        None
    }

    /// Emit a `TypeMismatch` unless `found` is coercible to `target`; otherwise
    /// warn if the (accepted) implicit conversion changes signedness.
    fn assert_coercible(&mut self, found: LangType, target: &LangType, pos: crate::lexer::Position) {
        if !types_coercible(&found, target) {
            self.errors.push(TypeCheckError::TypeMismatch {
                expected: *target,
                found,
                position: pos,
            });
            return;
        }
        self.warn_signedness_change(found, target, pos);
    }

    /// Warns on an *implicit* integer conversion that changes signedness
    /// (`i32 -> u32`, `i32 -> u64`) — both silently reinterpret the sign bit.
    /// Only fires on the implicit path (`assert_coercible`), so an explicit `as`
    /// cast silences it. Same-sign widening (`i32 -> i64`) is not flagged.
    fn warn_signedness_change(
        &mut self,
        found: LangType,
        target: &LangType,
        pos: crate::lexer::Position,
    ) {
        if found.is_plain_int() && target.is_plain_int() && found.base != target.base {
            self.warnings.push(crate::typechecker::errors::TypeWarning {
                message: format!(
                    "implicit conversion from '{found}' to '{target}' changes signedness \
                     (cast with `as` to silence)"
                ),
                position: pos,
            });
        }
    }

    /// Restamps an integer-literal `operand` to `sibling` when it fits, so a
    /// comparison like `u8 i; i < 10` compares at `u8` rather than widening `i`
    /// to the literal's default `i32`. Fit-restricted, so the result is unchanged.
    fn narrow_literal_to_sibling(operand: &mut Expression, sibling: LangType) {
        if let ExprKind::Literal(LiteralValue::Integer(val)) = operand.kind
            && sibling.is_plain_int()
            && literal_int_fits(val, &sibling)
        {
            operand.expr_type = sibling;
        }
    }

    /// Two pointers compare regardless of pointee type (comparing addresses is
    /// not aliasing) — kept decoupled from [`types_coercible`] so tightening
    /// `T* -> U*` binding doesn't also reject `Point* a == Node* b`. Requires
    /// matching depth after decay, or a `u0*` on either side; non-pointer
    /// operands fall back to arithmetic validity.
    fn comparison_operands_valid(left: &LangType, right: &LangType) -> bool {
        let l = if left.is_array() { left.decay_to_pointer() } else { *left };
        let r = if right.is_array() { right.decay_to_pointer() } else { *right };
        if l.is_pointer_like() && r.is_pointer_like() {
            let l_opaque = l.base == TypeBase::Void && l.pointer_depth == 1;
            let r_opaque = r.base == TypeBase::Void && r.pointer_depth == 1;
            return l.pointer_depth == r.pointer_depth || l_opaque || r_opaque;
        }
        Self::binary_op_types_valid(left, right, &BinaryOp::Add)
    }

    fn binary_op_types_valid(left: &LangType, right: &LangType, op: &BinaryOp) -> bool {
        // No arithmetic/bitwise/shift on enums (their equality goes through the
        // `Comparison` arm, never here).
        if matches!(left.base, TypeBase::Enum(_)) || matches!(right.base, TypeBase::Enum(_)) {
            return false;
        }

        // Pointer arithmetic: `ptr ± int` and `int + ptr` (`int - ptr` is
        // meaningless). A `u0*` pointee is unsized — GEP can't scale by
        // sizeof(u0), so cast to `u8*` for byte offsets.
        let ptr_int = left.is_pointer_like() && right.is_plain_int();
        let int_ptr = left.is_plain_int() && right.is_pointer_like();
        if (matches!(op, BinaryOp::Add | BinaryOp::Sub) && ptr_int)
            || (matches!(op, BinaryOp::Add) && int_ptr)
        {
            return !(left.is_opaque_ptr() || right.is_opaque_ptr());
        }

        // Both same family — either side can widen to the other
        types_coercible(left, right) || types_coercible(right, left)
    }

    /// Return the "wider" of two types (for binary-op result typing).
    /// Pointer arithmetic yields the pointer side regardless of operand
    /// order; falls back to `left` when types are incomparable.
    fn wider_type(left: &LangType, right: &LangType) -> LangType {
        if left.is_pointer_like() {
            return *left;
        }
        if right.is_pointer_like() {
            return *right;
        }
        if left.size_bits >= right.size_bits {
            *left
        } else {
            *right
        }
    }
}
