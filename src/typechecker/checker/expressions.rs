use super::TypeChecker;
use crate::lexer::{LangType, TypeBase};
use crate::parser::{BinaryOp, ComparisonOp, ExprKind, Expression, LiteralValue};
use crate::symbol::module::Visibility;
use crate::typechecker::errors::TypeCheckError;
use crate::typechecker::types::{
    cast_valid, literal_float_compatible, literal_int_fits, types_coercible,
};

impl TypeChecker {
    // ── Expression type resolution (synthesis mode) ──────────────────────────

    /// Synthesise the type of `expr` with no contextual expectation.
    ///
    /// Walks the expression, emits any type errors found, and returns its
    /// resolved type. Used at sites where nothing constrains the type: callee
    /// resolution, indices, conditions, cast/dereference operands.
    pub(crate) fn synth_expression(&mut self, expr: &mut Expression) -> LangType {
        let pos = expr.pos;
        // `MethodCall` is resolved and rewritten *before* the main match: it
        // replaces the whole node (method → `FunctionCall`, fn-ptr field →
        // `IndirectCall`), which a `match &mut expr.kind` arm cannot do. This is
        // the checker's one in-place lowering — see `resolve_method_call`.
        if matches!(expr.kind, ExprKind::MethodCall { .. }) {
            return self.resolve_method_call(expr);
        }
        let default_type = expr.expr_type;
        match &mut expr.kind {
            ExprKind::Literal(_) => default_type,

            // An enum variant value: the parser already resolved the variant and
            // stamped the enum type. It synthesises to exactly that enum type —
            // never to a bare integer — which is what keeps enum ↔ int implicit
            // coercion impossible.
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
                    // Result type: the wider of the two operand types (or left if equal)
                    Self::wider_type(&left_type, &right_type)
                }
            }

            ExprKind::Comparison { left, op, right } => {
                let left_type = self.synth_expression(left);
                let right_type = self.synth_expression(right);

                // Enums are a nominal name set: two operands are comparable only
                // when they are the *same* enum, and only `==`/`!=` are defined
                // (there is no ordering — a variant's numeric value is an
                // implementation detail). Enum-vs-int or enum-vs-other-enum is a
                // type error, which is the whole point of a distinct enum type.
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
                // A comparison never propagates its own (`i32`) result type into
                // its operands, but a literal operand may adopt its *sibling's*
                // narrower integer type so codegen compares at that width instead
                // of widening both sides to the literal's default `i32`. The
                // boolean result is unaffected because the literal fits the
                // sibling's exact type.
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
                // Recompute the pointee from the just-synthesized inner type
                // rather than trusting the parser's best-effort stamp: only the
                // checker knows the base's const-ness (propagated through
                // `resolve_field`), so a stale stamp would let `*const_ptr = x`
                // and `*this.next = n` slip past the write-through-const check
                // in `DerefAssign`. Const propagates downward — a const pointer
                // yields a const pointee.
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
                if alloc_type.is_void_value() {
                    self.errors.push(TypeCheckError::InvalidVoidValue(pos));
                }
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
                // Logical negation yields a boolean.
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

            // A bare function name (or `&func` collapsed) — the parser stamped
            // the FnPtr type from the registry. Nothing to check; just hand it
            // back. An unknown function name would have stayed `Variable` with
            // a `void` stamp, so it never reaches this arm.
            ExprKind::FunctionRef(_) => default_type,

            // Indirect call through a function-pointer value: synth the callee,
            // validate it's a `FnPtr`, then `check` each arg against the
            // declared parameter type (mirrors `check_call`'s pattern).
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
                        for arg in args.iter_mut() {
                            self.synth_expression(arg);
                        }
                    } else {
                        for (pty, arg) in params.iter().zip(args.iter_mut()) {
                            self.check_expression(arg, pty);
                        }
                    }
                } else {
                    for arg in args.iter_mut() {
                        self.synth_expression(arg);
                    }
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

    /// Resolve a field access on a base type, emitting an error and returning a
    /// `void` placeholder when the base is not a type-struct or the field is
    /// unknown. A single-level pointer-to-struct auto-dereferences.
    fn resolve_field(
        &mut self,
        base_type: &LangType,
        field: &str,
        pos: crate::lexer::Position,
    ) -> LangType {
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

    /// Enforce method encapsulation: a private method is callable only from
    /// within its own type's methods. `name` is the call's mangled target
    /// (`Type$method`); a name with no `$` is an ordinary free function and is
    /// always accessible. The private-method twin of [`Self::resolve_field`]'s
    /// private-field rule — the two syntactic call forms (`obj.m()`, `T.m()`)
    /// have both already been lowered to this mangled name by the parser.
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

    /// Resolve a function call: validate the callee, arity, and argument types.
    ///
    /// Each argument is *checked* against its declared parameter type, which
    /// pushes the parameter type into literal arguments.
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
                for arg in args.iter_mut() {
                    self.synth_expression(arg);
                }
            } else {
                for ((param_ty, _), arg_expr) in sig.params.iter().zip(args.iter_mut()) {
                    self.check_expression(arg_expr, param_ty);
                }
            }
        } else {
            self.errors
                .push(TypeCheckError::UndefinedFunction(name.to_string(), pos));
            for arg in args.iter_mut() {
                self.synth_expression(arg);
            }
        }
    }

    // ── MethodCall resolution (checker-side lowering) ────────────────────────

    /// Resolve an `ExprKind::MethodCall` in place: build the concrete call node
    /// (`FunctionCall` for a method, `IndirectCall` for a fn-pointer field),
    /// install it on `expr`, and return the call's result type by re-checking
    /// the rewritten node.
    ///
    /// This is the checker analogue of the parser's `build_method_call`; it
    /// exists so metaprogram-generated AST (Three-Hook-Metasystem Phases 3/4),
    /// which carries no parse-time receiver types, can defer method dispatch to
    /// type-checking. It is a *one-shot* lowering — the resulting node is a
    /// plain call, so re-checking it is stable — and must not be counted as a
    /// handler rewrite by any future rounds driver (§14.1).
    ///
    /// The per-method privacy gate (`MethodSig.vis`) is enforced for free when
    /// the rewritten `FunctionCall` flows through `check_call` →
    /// `check_method_access`. The `public type` cross-module gate is
    /// deliberately **not** reproduced here — the checker has no
    /// `file_id → module` map — matching §14.2's accepted carve-out that
    /// metaprogram-generated code bypasses import visibility. When the parser
    /// eventually migrates to emit `MethodCall`, that gate (and the module maps
    /// it needs) must move with it.
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
            // Receiver: autoref a value, pass a pointer as-is, reject deeper
            // pointers. Const propagates into the reference type, so calling a
            // mutating (non-`const fn`) method on a const receiver is rejected
            // downstream by `check_call`'s coercion of the receiver arg.
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

    // ── Expression type checking (checking mode) ─────────────────────────────

    /// Check `expr` against the expected `target` type.
    ///
    /// Stamps `expr.expr_type` and pushes the target into children where the
    /// child's type *is* the parent's type (arithmetic operands, bitwise-not,
    /// reference/dereference, list-initialiser elements). Emits a single
    /// `TypeMismatch` (or a more specific literal-fit error) on failure.
    pub(crate) fn check_expression(&mut self, expr: &mut Expression, target: &LangType) {
        let pos = expr.pos;
        match &mut expr.kind {
            // Integer literal: validate value-fit against the target and stamp it.
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

            // Float literal: any float target accepts it; stamp the target.
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

            // String literal: type is fixed; verify coercibility only.
            ExprKind::Literal(LiteralValue::String(_)) => {
                self.assert_coercible(expr.expr_type, target, pos);
            }

            // Binary arithmetic with a plain numeric target: propagate the
            // target into both operands; the operation shares its result type.
            // Logical `&&`/`||` are excluded — they yield a boolean, not the
            // target type, so they fall through to the synth arm below.
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

            // Reference: the inner expression's target is the pointee type.
            // A Reference may produce a const-pointer to a non-const value
            // (C-style `const T* p = &t`), so the inner itself need not carry
            // the pointee's const-ness.
            ExprKind::Reference(inner) => {
                // Against an opaque `u0*` target the pointee is `u0` — no
                // value has that type, so nothing useful can be pushed
                // inward; synthesise instead (any `&lvalue` coerces to u0*).
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

            // List initialiser: decay the target to its element type and check
            // every element against it.
            ExprKind::ListInitializer(elements) => {
                let elem_target = target.element_type();
                for elem in elements.iter_mut() {
                    self.check_expression(elem, &elem_target);
                }
            }

            // `null` adopts whatever pointer type the context demands. The
            // parser stamps a `u8*` placeholder that only survives in synth
            // position (no contextual target); here we upgrade it to the
            // target so `u8** p = null`, `Point* q = null`, and
            // `fn() -> R f = null` all yield a null of the correct type —
            // structural depth otherwise blocks the `u8*` placeholder from
            // coercing into a deeper or function pointer. A non-pointer (or
            // array) target falls through to the coercibility check, which
            // rejects it.
            ExprKind::Null
                if !target.is_array()
                    && (target.pointer_depth > 0
                        || matches!(target.base, TypeBase::FnPtr(_))) =>
            {
                expr.expr_type = *target;
            }

            // Comparison, unary-not, cast, function call, variable, alloc, and
            // binary ops with a non-numeric (pointer) target: the expression's
            // type is not the target's type, so synthesise and assert
            // coercibility at the boundary.
            _ => {
                let found = self.synth_expression(expr);
                self.assert_coercible(found, target, pos);
            }
        }
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

    /// Warn on an *implicit* integer conversion that changes signedness —
    /// whether it widens (`i32 -> u64`, case b) or keeps the same width
    /// (`i32 -> u32`, case c). Both silently reinterpret the sign bit of a
    /// runtime value. This runs only on the implicit-coercion path
    /// (`assert_coercible`); an explicit `as` cast does not pass through here,
    /// so a cast is the way to silence the warning per-site. Same-sign widening
    /// (`i32 -> i64`) is deliberately not flagged.
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

    /// If `operand` is an integer literal that fits the concrete integer type
    /// `sibling`, restamp the literal to that type.
    ///
    /// Used for comparison operands: `u8 i; ... i < 10` compares at `i8` rather
    /// than zero-extending `i` to `i32` to meet the literal's default width.
    /// Restricted to literals that fit `sibling`, so the comparison's result is
    /// unchanged.
    fn narrow_literal_to_sibling(operand: &mut Expression, sibling: LangType) {
        if let ExprKind::Literal(LiteralValue::Integer(val)) = operand.kind
            && sibling.is_plain_int()
            && literal_int_fits(val, &sibling)
        {
            operand.expr_type = sibling;
        }
    }

    // ── Binary op helpers ────────────────────────────────────────────────────

    /// Whether two operand types may be *compared* (`==`, `!=`, `<`, …).
    ///
    /// Pointer comparison keeps the permissive rule that assignment coercion
    /// dropped in Proposal C: two pointers compare regardless of pointee type,
    /// because comparing addresses is not aliasing. Kept decoupled from
    /// [`types_coercible`] so tightening `T* -> U*` binding does not also reject
    /// `Point* a == Node* b`. Same depth (after decay), or a `u0*` on either
    /// side (which includes the `null` placeholder, a `u8*`); non-pointer
    /// operands fall back to the ordinary arithmetic/coercion validity.
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

    /// Check if two operand types are valid for the given binary operation.
    fn binary_op_types_valid(left: &LangType, right: &LangType, op: &BinaryOp) -> bool {
        // Enums are a name set, not a number: no arithmetic, bitwise, or shift
        // operation is defined on them. (Their equality is handled entirely in
        // the `Comparison` arm and never routes through this helper.)
        if matches!(left.base, TypeBase::Enum(_)) || matches!(right.base, TypeBase::Enum(_)) {
            return false;
        }

        // Pointer arithmetic: `ptr ± int` and `int + ptr` (`int - ptr` has no
        // meaning). A `u0*` has an unsized pointee: no arithmetic (GEP cannot
        // scale by sizeof(u0)) — cast to `u8*` for byte offsets.
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
