//! Expression code-generation: `walk_expression` + env/emitter wiring.
//!
//! `walk_expression` is the single recursive expression tree walker.
//! It is parameterised by `EmitMode`, which selects either:
//! - `EmitMode::Runtime` — uses `RuntimeEmitter` (builder-based LLVM IR emission).
//! - `EmitMode::Constant` — uses `ConstantEmitter` (Rust-level constant folding).
//!
//! The public entry-points on `CodeGenerator` are:
//! - `generate_expression`         → walk with `EmitMode::Runtime`
//! - `generate_constant_expression`→ walk with `EmitMode::Constant` (step 11)

use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{BasicValue, BasicValueEnum, PointerValue};
use inkwell::{AddressSpace, IntPredicate};

use crate::codegen::CodegenError;
use crate::codegen::generator::CodeGenerator;
use crate::codegen::types::{
    LangTypeExt, float_cmp_pred, int_cmp_pred, widen_floats_to_match, widen_ints_to_match,
};
use crate::codegen::value_emitter::{ConstantEmitter, RuntimeEmitter, ValueEmitter};
use crate::lexer::{LangType, Position, TypeBase};
use crate::parser::{BinaryOp, ExprKind, Expression, LiteralValue};

// ─── EmitMode ─────────────────────────────────────────────────────────────────

/// Selects how leaf operations are materialised in `walk_expression`.
#[derive(Copy, Clone, PartialEq, Eq)]
pub(crate) enum EmitMode {
    /// Emit LLVM IR via the builder (normal function bodies).
    Runtime,
    /// Fold constants in Rust and reconstruct LLVM constants (global initialisers).
    Constant,
}

// ─── Leaf-level helpers ───────────────────────────────────────────────────────

/// Construct the mode-appropriate emitter for leaf operations.
fn emitter<'a, 'ctx>(
    cg: &'a CodeGenerator<'ctx>,
    mode: EmitMode,
) -> Box<dyn ValueEmitter<'ctx> + 'a> {
    match mode {
        EmitMode::Runtime => Box::new(RuntimeEmitter {
            builder: &cg.builder,
            context: cg.context,
        }),
        EmitMode::Constant => Box::new(ConstantEmitter {
            context: cg.context,
        }),
    }
}

/// Reject `what` in `Constant` mode — for operations that have no
/// constant-folding path.
fn require_runtime(mode: EmitMode, what: &str, pos: Position) -> Result<(), CodegenError> {
    if mode == EmitMode::Constant {
        return Err(CodegenError::InvalidOperation(
            format!("{what} not supported in constant expressions"),
            pos,
        ));
    }
    Ok(())
}

/// Dispatch a non-pointer binary operation through the given emitter.
fn emit_binary_dispatch<'ctx>(
    e: &dyn ValueEmitter<'ctx>,
    left_val: BasicValueEnum<'ctx>,
    right_val: BasicValueEnum<'ctx>,
    op: &BinaryOp,
    left_ty: &LangType,
    right_ty: &LangType,
    pos: Position,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    let is_float = matches!(left_ty.base, TypeBase::SFloat);
    if is_float {
        let lf = left_val.into_float_value();
        let rf = right_val.into_float_value();
        let (lf, rf) = e.emit_widen_floats(lf, rf)?;
        e.emit_float_binary(op, lf, rf, pos)
    } else {
        let is_signed = matches!(left_ty.base, TypeBase::SInt);
        let right_is_signed = matches!(right_ty.base, TypeBase::SInt);
        let li = left_val.into_int_value();
        let ri = right_val.into_int_value();
        let (li, ri) = e.emit_widen_ints(li, is_signed, ri, right_is_signed)?;
        e.emit_int_binary(op, li, ri, is_signed, pos)
    }
}

/// Lower `ptr ± int` (or `int + ptr`) to an in-bounds GEP scaled by the
/// pointee type. Runtime only — the caller has already rejected `Constant`
/// mode.
fn emit_pointer_arithmetic<'ctx>(
    cg: &mut CodeGenerator<'ctx>,
    op: &BinaryOp,
    left: &Expression,
    right: &Expression,
    left_val: BasicValueEnum<'ctx>,
    right_val: BasicValueEnum<'ctx>,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    let left_is_ptr = left.expr_type.pointer_depth > 0;
    if left_is_ptr && right.expr_type.pointer_depth > 0 {
        return Err(CodegenError::InvalidOperation(
            "pointer arithmetic only allowed with integers".to_string(),
            left.pos,
        ));
    }

    let (ptr_expr, ptr_val, int_val) = if left_is_ptr {
        (left, left_val, right_val)
    } else {
        (right, right_val, left_val)
    };
    let ptr = ptr_val.into_pointer_value();
    let int = int_val.into_int_value();
    // `lang_type_to_llvm` (not the context-only `to_llvm`) so the GEP scales
    // correctly when the pointee is a type-struct.
    let pointee_type = cg.lang_type_to_llvm(&ptr_expr.expr_type.pointee(), ptr_expr.pos)?;

    match op {
        BinaryOp::Add => unsafe {
            Ok(cg
                .builder
                .build_in_bounds_gep(pointee_type, ptr, &[int], "ptr_add")
                .map(Into::into)?)
        },
        BinaryOp::Sub if left_is_ptr => {
            let neg = cg.builder.build_int_neg(int, "neg")?;
            unsafe {
                Ok(cg
                    .builder
                    .build_in_bounds_gep(pointee_type, ptr, &[neg], "ptr_sub")
                    .map(Into::into)?)
            }
        }
        _ => Err(CodegenError::InvalidOperation(
            format!("operator {op:?} not supported for pointers"),
            left.pos,
        )),
    }
}

// ─── Main walker ──────────────────────────────────────────────────────────────

/// Recursively evaluate `expr`.
///
/// `mode` controls whether leaf operations emit IR (`Runtime`) or fold
/// constants in Rust (`Constant`).  Callers that need the runtime path
/// should call `generate_expression` instead.
pub(crate) fn walk_expression<'ctx>(
    expr: &Expression,
    cg: &mut CodeGenerator<'ctx>,
    mode: EmitMode,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    match &expr.kind {
        // ── Literals ──────────────────────────────────────────────────────
        ExprKind::Literal(lit) => match lit {
            LiteralValue::Integer(val) => {
                emitter(cg, mode).emit_int_literal(*val, &expr.expr_type, expr.pos)
            }
            LiteralValue::Float(val) => {
                emitter(cg, mode).emit_float_literal(*val, &expr.expr_type, expr.pos)
            }
            LiteralValue::String(index) => cg.emit_string_ptr(*index, mode),
            // Boolean literal: an i1 value (zero-extended to i8 when stored).
            LiteralValue::Bool(b) => Ok(cg
                .context
                .bool_type()
                .const_int(u64::from(*b), false)
                .into()),
        },

        // ── Variable load ─────────────────────────────────────────────────
        ExprKind::Variable(name) => match mode {
            EmitMode::Runtime => {
                let (ptr, llvm_type, lang_type, const_value) = {
                    let v = cg
                        .scope
                        .lookup_any(name)
                        .ok_or_else(|| CodegenError::UndefinedVariable(name.clone(), expr.pos))?;
                    (v.ptr(), v.llvm_type(), v.lang_type(), v.const_value())
                };

                if let Some(const_val) = const_value {
                    return Ok(const_val);
                }

                if lang_type.is_array() {
                    return Ok(ptr.into());
                }

                let loaded = cg.builder.build_load(llvm_type, ptr, name)?;

                // A `bool` is stored as i8 but only ever holds 0 or 1. Tagging
                // the load with `!range !{i8 0, i8 2}` lets LLVM fold branches
                // and selects that test it.
                if lang_type.base == TypeBase::Bool
                    && let BasicValueEnum::IntValue(v) = loaded
                    && let Some(instr) = v.as_instruction_value()
                {
                    let i8t = cg.context.i8_type();
                    let md = cg.context.metadata_node(&[
                        i8t.const_int(0, false).into(),
                        i8t.const_int(2, false).into(),
                    ]);
                    let kind_id = cg.context.get_kind_id("range");
                    let _ = instr.set_metadata(md, kind_id);
                }

                if lang_type.is_const {
                    let instr = match loaded {
                        BasicValueEnum::IntValue(v) => v.as_instruction_value(),
                        BasicValueEnum::FloatValue(v) => v.as_instruction_value(),
                        BasicValueEnum::PointerValue(v) => v.as_instruction_value(),
                        _ => None,
                    };
                    if let Some(instr) = instr {
                        let kind_id = cg.context.get_kind_id("invariant.load");
                        let md = cg.context.metadata_node(&[]);
                        let _ = instr.set_metadata(md, kind_id);
                    }
                }

                Ok(loaded)
            }
            EmitMode::Constant => {
                // Check local scope first (const locals store their folded value).
                for scope in cg.scope.iter_scopes() {
                    if let Some(var) = scope.get(name) {
                        return var.const_value.ok_or_else(|| {
                            CodegenError::InvalidOperation(
                                format!("variable '{name}' is not a compile-time constant"),
                                expr.pos,
                            )
                        });
                    }
                }
                // Fall back to global initializer.
                let global_val = cg
                    .module
                    .get_global(name)
                    .ok_or_else(|| CodegenError::UndefinedVariable(name.clone(), expr.pos))?;
                global_val.get_initializer().ok_or_else(|| {
                    CodegenError::InvalidOperation(
                        format!(
                            "global '{name}' has no constant initializer; \
                         declare it before referencing it in another global initializer"
                        ),
                        expr.pos,
                    )
                })
            }
        },

        // ── Binary operations ─────────────────────────────────────────────
        ExprKind::Binary { left, op, right } => {
            // Evaluate sub-expressions first (recursive, needs &mut cg).
            let left_val = walk_expression(left, cg, mode)?;
            let right_val = walk_expression(right, cg, mode)?;

            // Pointer arithmetic lowers to a GEP; everything else is scalar.
            if left.expr_type.pointer_depth > 0 || right.expr_type.pointer_depth > 0 {
                require_runtime(mode, "pointer arithmetic", expr.pos)?;
                return emit_pointer_arithmetic(cg, op, left, right, left_val, right_val);
            }

            emit_binary_dispatch(
                &*emitter(cg, mode),
                left_val,
                right_val,
                op,
                &left.expr_type,
                &right.expr_type,
                expr.pos,
            )
        }

        // ── Comparison (runtime only) ─────────────────────────────────────
        ExprKind::Comparison { left, op, right } => {
            require_runtime(mode, "comparison", expr.pos)?;
            let left_val = walk_expression(left, cg, mode)?;
            let right_val = walk_expression(right, cg, mode)?;

            if left.expr_type.pointer_depth > 0 && right.expr_type.pointer_depth > 0 {
                Ok(cg
                    .builder
                    .build_int_compare(
                        int_cmp_pred(op, false),
                        left_val.into_pointer_value(),
                        right_val.into_pointer_value(),
                        "ptr_cmp",
                    )?
                    .into())
            } else if matches!(left.expr_type.base, TypeBase::SFloat) {
                let lf = left_val.into_float_value();
                let rf = right_val.into_float_value();
                let (lf, rf) = widen_floats_to_match(cg.context, &cg.builder, lf, rf)?;
                Ok(cg
                    .builder
                    .build_float_compare(float_cmp_pred(op), lf, rf, "fcmp")?
                    .into())
            } else {
                let is_signed = matches!(left.expr_type.base, TypeBase::SInt);
                let right_signed = matches!(right.expr_type.base, TypeBase::SInt);
                let li = left_val.into_int_value();
                let ri = right_val.into_int_value();
                let (li, ri) = widen_ints_to_match(&cg.builder, li, is_signed, ri, right_signed)?;
                Ok(cg
                    .builder
                    .build_int_compare(int_cmp_pred(op, is_signed), li, ri, "icmp")?
                    .into())
            }
        }

        // ── Address-of ────────────────────────────────────────────────────
        ExprKind::Reference(inner) => match &inner.kind {
            ExprKind::Variable(name) => match mode {
                EmitMode::Runtime => {
                    let ptr = cg
                        .scope
                        .lookup_any(name)
                        .ok_or_else(|| CodegenError::UndefinedVariable(name.clone(), inner.pos))?
                        .ptr();
                    Ok(ptr.into())
                }
                EmitMode::Constant => {
                    let ptr = cg
                        .scope
                        .lookup_global(name.as_str())
                        .ok_or_else(|| CodegenError::UndefinedVariable(name.clone(), inner.pos))?
                        .ptr;
                    Ok(ptr.into())
                }
            },
            ExprKind::Dereference(inner2) => walk_expression(inner2, cg, mode),
            ExprKind::FieldAccess { .. } => {
                require_runtime(mode, "address-of field", inner.pos)?;
                let (ptr, _) = cg.emit_address(inner)?;
                Ok(ptr.into())
            }
            _ => {
                // An rvalue struct (e.g. a method-call receiver in
                // `make(...).method()` or `&SomeLiteral{...}`) is materialised
                // into a temporary slot so its address can be taken.
                if mode == EmitMode::Runtime
                    && inner.expr_type.pointer_depth == 0
                    && matches!(inner.expr_type.base, TypeBase::Struct(_))
                {
                    let val = walk_expression(inner, cg, mode)?;
                    let struct_ty = cg.lang_type_to_llvm(&inner.expr_type, inner.pos)?;
                    let tmp = cg.builder.build_alloca(struct_ty, "ref.tmp")?;
                    cg.builder.build_store(tmp, val)?;
                    return Ok(tmp.into());
                }
                Err(CodegenError::InvalidOperation(
                    "Cannot take address of non-lvalue".to_string(),
                    inner.pos,
                ))
            }
        },

        // ── Dereference (runtime only) ────────────────────────────────────
        ExprKind::Dereference(inner_expr) => {
            require_runtime(mode, "dereference", expr.pos)?;
            let ptr = walk_expression(inner_expr, cg, mode)?;
            if inner_expr.expr_type.pointer_depth == 0 {
                return Err(CodegenError::TypeError(
                    "Cannot dereference a non-pointer type".to_string(),
                    expr.pos,
                ));
            }
            // Cache-aware lowering: `*(Pair*)` loads a struct value, which the
            // context-only `to_llvm` cannot resolve.
            let pointee_type = cg.lang_type_to_llvm(&inner_expr.expr_type.pointee(), inner_expr.pos)?;
            Ok(cg
                .builder
                .build_load(pointee_type, ptr.into_pointer_value(), "deref")?)
        }

        // ── Function call (runtime only) ──────────────────────────────────
        ExprKind::FunctionCall { name, args } => {
            require_runtime(mode, "function calls", expr.pos)?;
            cg.generate_function_call(name, args, expr.pos)
        }

        // ── Cast ──────────────────────────────────────────────────────────
        ExprKind::Cast {
            expr: inner,
            target_type,
        } => {
            let val = walk_expression(inner, cg, mode)?;
            let target_llvm = target_type.to_llvm(cg.context, expr.pos)?;
            emitter(cg, mode).emit_cast(val, target_llvm, &inner.expr_type, target_type, inner.pos)
        }

        // ── Alloc (both modes — context determines stack vs global) ───────
        ExprKind::Alloc { alloc_type, count } => cg.generate_alloc(alloc_type, count),

        // ── Logical / bitwise NOT ─────────────────────────────────────────
        ExprKind::UnaryNot(inner) => {
            let raw = walk_expression(inner, cg, mode)?;
            // `!p` on a pointer is a null test: compare the address as an
            // integer against zero (runtime only — pointers don't fold).
            if raw.is_pointer_value() {
                require_runtime(mode, "logical NOT of a pointer", inner.pos)?;
                let addr = cg.builder.build_ptr_to_int(
                    raw.into_pointer_value(),
                    cg.context.i64_type(),
                    "ptr_addr",
                )?;
                let zero = addr.get_type().const_zero();
                return Ok(cg
                    .builder
                    .build_int_compare(IntPredicate::EQ, addr, zero, "nottmp")?
                    .into());
            }
            let val = raw.into_int_value();
            // Logical NOT yields an i1 boolean; callers extend if they need a
            // wider integer.
            match mode {
                EmitMode::Runtime => {
                    let zero = val.get_type().const_zero();
                    Ok(cg
                        .builder
                        .build_int_compare(IntPredicate::EQ, val, zero, "nottmp")?
                        .into())
                }
                EmitMode::Constant => {
                    let n = val.get_zero_extended_constant().ok_or_else(|| {
                        CodegenError::InvalidOperation(
                            "constant integer not representable as u64".to_string(),
                            inner.pos,
                        )
                    })?;
                    Ok(cg
                        .context
                        .bool_type()
                        .const_int(u64::from(n == 0), false)
                        .into())
                }
            }
        }

        ExprKind::BitwiseNot(inner) => {
            let val = walk_expression(inner, cg, mode)?.into_int_value();
            match mode {
                EmitMode::Runtime => Ok(cg.builder.build_not(val, "bnottmp")?.into()),
                EmitMode::Constant => Ok(val.const_not().into()),
            }
        }

        // ── List initializer (always invalid as a standalone expression) ──
        ExprKind::ListInitializer(_) => Err(CodegenError::InvalidOperation(
            "list initializer is only valid in a variable declaration".to_string(),
            expr.pos,
        )),

        // ── Field access `base.field` (runtime only) ──────────────────────
        ExprKind::FieldAccess { .. } => {
            require_runtime(mode, "field access", expr.pos)?;
            let (field_ptr, field_ty) = cg.emit_address(expr)?;
            // Arrays decay to a pointer (matching the variable-load rule);
            // scalars and nested struct values are loaded.
            if field_ty.is_array() {
                return Ok(field_ptr.into());
            }
            let field_llvm = cg.lang_type_to_llvm(&field_ty, expr.pos)?;
            Ok(cg.builder.build_load(field_llvm, field_ptr, "field")?)
        }

        // ── Struct literal `Name { f = v, ... }` (runtime only) ───────────
        ExprKind::StructLiteral { struct_id, fields } => {
            //require_runtime(mode, "struct literal", expr.pos)?;
            let struct_ty = *cg.struct_types.get(struct_id).ok_or_else(|| {
                CodegenError::TypeError(
                    format!("unregistered type-struct id {struct_id}"),
                    expr.pos,
                )
            })?;

            match mode {
                EmitMode::Constant => {
                    let layout = cg.struct_fields[struct_id].clone();
                    let mut vals = Vec::with_capacity(layout.len());
                    for (fname, fty) in &layout {
                        match fields.iter().find(|(n, _)| n == fname) {
                            Some((_, fexpr)) => {
                                vals.push(cg.generate_coerced_value(fexpr, Some(fty), mode)?)
                            }
                            None => {
                                return Err(CodegenError::TypeError(
                                    format!(
                                        "missing field '{fname}' in struct literal for type-struct id {struct_id}"
                                    ),
                                    expr.pos,
                                ));
                            }
                        }
                    }
                    Ok(struct_ty.const_named_struct(&vals).into())
                }
                EmitMode::Runtime => {
                    // Build the aggregate value field-by-field via insertvalue.
                    // TODO: If we can ensure the initializer is constant, we can build a constant value
                    // and store it directly, which is more efficient than insertvalue.
                    let mut agg = struct_ty.get_undef();
                    for (fname, fexpr) in fields {
                        // TODO: This is insanely inefficient. We should have a function that gives us this ordered in a Vector.
                        let (idx, field_ty) =
                            cg.struct_field(*struct_id, fname).ok_or_else(|| {
                                CodegenError::TypeError(
                                    format!(
                                        "unknown field '{fname}' on type-struct id {struct_id}"
                                    ),
                                    expr.pos,
                                )
                            })?;
                        let fval = cg.generate_coerced_value(fexpr, Some(&field_ty), mode)?;
                        agg = cg
                            .builder
                            .build_insert_value(
                                agg,
                                fval,
                                u32::try_from(idx).expect("field index out of range"),
                                "structlit",
                            )?
                            .into_struct_value();
                    }
                    Ok(agg.into())
                }
            }
        }

        // ── Function reference: bare function name as a value. Function
        // addresses are LLVM-level constants (resolved at link time), so the
        // same emission works in both Runtime and Constant modes.
        ExprKind::FunctionRef(name) => {
            let function = cg
                .functions
                .get(name)
                .copied()
                .ok_or_else(|| CodegenError::UndefinedFunction(name.clone(), expr.pos))?;
            Ok(function.as_global_value().as_pointer_value().into())
        }

        // ── Indirect call through a function-pointer value (runtime only).
        ExprKind::IndirectCall { callee, args } => {
            require_runtime(mode, "indirect call", expr.pos)?;
            cg.generate_indirect_call(callee, args, expr.pos)
        }

        // ── `sizeof(T)` — emit a `u64` constant from the target data layout.
        // Works in both Runtime and Constant modes since the value is fixed
        // at codegen time and doesn't depend on any runtime state.
        ExprKind::SizeOf(ty) => {
            let bytes = cg.sizeof_lang_type(ty, expr.pos)?;
            Ok(cg.context.i64_type().const_int(bytes, false).into())
        }

        // ── `null` — opaque-ptr null constant. Works in both Runtime and
        // Constant modes; all pointer types lower to LLVM `ptr` anyway, so
        // one constant covers every target pointer type.
        ExprKind::Null => Ok(cg
            .context
            .ptr_type(inkwell::AddressSpace::default())
            .const_null()
            .into()),

        // ── Value-block `{ ...; return v }` — runtime only: it executes
        // statements, so it can never fold to a compile-time constant
        // (the Constant-mode Err makes `try_fold` fall back to the
        // runtime path for initializers).
        ExprKind::ValueBlock(stmts) => match mode {
            EmitMode::Runtime => cg.generate_value_block(stmts, expr.expr_type, expr.pos),
            EmitMode::Constant => Err(CodegenError::InvalidOperation(
                "value block is not a compile-time constant".to_string(),
                expr.pos,
            )),
        },
    }
}

// ─── impl CodeGenerator — expression entry-points ────────────────────────────

impl<'ctx> CodeGenerator<'ctx> {
    /// Generate code for an expression (runtime path).
    pub(crate) fn generate_expression(
        &mut self,
        expr: &Expression,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        walk_expression(expr, self, EmitMode::Runtime)
    }

    /// Look up the interned global for string literal `index` (registered by
    /// `generate_string_literal`) and cast it to `u8*`.
    pub(crate) fn emit_string_ptr(
        &self,
        index: usize,
        mode: EmitMode,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let ptr = self
            .scope
            .lookup_global(&Self::string_literal_name(index))
            .expect("Internal error: String literal global not found")
            .ptr;
        let i8_ptr_type = self.context.ptr_type(AddressSpace::default());
        match mode {
            EmitMode::Runtime => Ok(self
                .builder
                .build_pointer_cast(ptr, i8_ptr_type, "str")?
                .into()),
            EmitMode::Constant => Ok(ptr.const_cast(i8_ptr_type).into()),
        }
    }

    /// Generate an expression, coercing it to `target` when the types differ.
    ///
    /// Integer/float literals assigned to a concrete target type are checked for
    /// overflow at this stage and emitted directly at the target width.
    /// Coerce `expr` to `target`, emitting in `mode`.
    ///
    /// In `Constant` mode a non-constant operand fails here — callers rely on
    /// that failure to fall back to runtime. Coercing at runtime where a
    /// constant is required puts instructions inside a constant aggregate,
    /// which only the IR verifier catches.
    pub(crate) fn generate_coerced_value(
        &mut self,
        expr: &Expression,
        target: Option<&LangType>,
        mode: EmitMode,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        // Fast path: literal assigned to a scalar target — emit at target type
        // with overflow check.
        if let Some(target_ty) = target
            && target_ty.pointer_depth == 0
            && !target_ty.is_array()
            && let ExprKind::Literal(lit @ (LiteralValue::Integer(_) | LiteralValue::Float(_))) =
                &expr.kind
        {
            return self.generate_literal_typed(lit, target_ty, expr.pos);
        }

        let val = walk_expression(expr, self, mode)?;

        // Auto-widen to target if types differ. Struct values are aggregates —
        // they are never scalar-cast; the value is stored/copied as-is.
        if let Some(target_ty) = target
            && target_ty.pointer_depth == 0
            && !target_ty.is_array()
            && !matches!(target_ty.base, TypeBase::Struct(_))
        {
            let target_llvm = target_ty.to_llvm(self.context, expr.pos)?;
            if val.get_type() != target_llvm {
                return emitter(self, mode).emit_cast(
                    val,
                    target_llvm,
                    &expr.expr_type,
                    target_ty,
                    expr.pos,
                );
            }
        }

        Ok(val)
    }

    /// Generate a literal with an explicit target type and overflow checking.
    pub(crate) fn generate_literal_typed(
        &self,
        lit: &LiteralValue,
        ty: &LangType,
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        match lit {
            LiteralValue::Integer(val) => {
                let llvm_type = ty.to_llvm(self.context, pos)?;
                match llvm_type {
                    BasicTypeEnum::IntType(int_ty) => {
                        let bits = int_ty.get_bit_width();
                        if matches!(ty.base, TypeBase::SInt) {
                            if bits < 64 {
                                let min = -(1i64 << (bits - 1));
                                let max = (1i64 << (bits - 1)) - 1;
                                if *val < min || *val > max {
                                    return Err(CodegenError::TypeError(
                                        format!("integer literal {} overflows {}", val, ty),
                                        pos,
                                    ));
                                }
                            }

                            // Preserve two's-complement bit pattern explicitly.
                            let signed_bits = u64::from_ne_bytes(val.to_ne_bytes());
                            Ok(int_ty.const_int(signed_bits, true).into())
                        } else {
                            let unsigned = u64::try_from(*val).map_err(|_| {
                                CodegenError::TypeError(
                                    format!("integer literal {} overflows {}", val, ty),
                                    pos,
                                )
                            })?;

                            if bits < 64 && unsigned >= (1u64 << bits) {
                                return Err(CodegenError::TypeError(
                                    format!("integer literal {} overflows {}", val, ty),
                                    pos,
                                ));
                            }

                            Ok(int_ty.const_int(unsigned, false).into())
                        }
                    }
                    _ => Err(CodegenError::TypeError(
                        "integer literal must have integer type".to_string(),
                        pos,
                    )),
                }
            }
            LiteralValue::Float(val) => {
                let llvm_type = ty.to_llvm(self.context, pos)?;
                match llvm_type {
                    BasicTypeEnum::FloatType(float_ty) => Ok(float_ty.const_float(*val).into()),
                    _ => Err(CodegenError::TypeError(
                        "float literal must have float type".to_string(),
                        pos,
                    )),
                }
            }
            LiteralValue::Bool(b) => match ty.to_llvm(self.context, pos)? {
                BasicTypeEnum::IntType(int_ty) => Ok(int_ty.const_int(u64::from(*b), false).into()),
                _ => Err(CodegenError::TypeError(
                    "boolean literal must have integer type".to_string(),
                    pos,
                )),
            },
            LiteralValue::String(index) => self.emit_string_ptr(*index, EmitMode::Runtime),
        }
    }

    pub(crate) fn generate_list_initializer(
        &mut self,
        array_ptr: PointerValue<'ctx>,
        var_type: &LangType,
        elements: &[Expression],
        pos: Position,
    ) -> Result<(), CodegenError> {
        let elem_lang_type = var_type.element_type();
        let elem_llvm_type = elem_lang_type.to_llvm(self.context, pos)?;
        let array_size = var_type.array_size.unwrap_or(0);
        let array_llvm_type = elem_llvm_type.array_type(array_size);

        // Empty initializer: zero the whole array
        if elements.is_empty() {
            self.builder
                .build_store(array_ptr, array_llvm_type.const_zero())?;
            return Ok(());
        }

        // Fast path: all elements are integer/float literals -> emit a single ConstantArray store
        let all_const = elements.iter().all(|e| {
            matches!(
                e.kind,
                ExprKind::Literal(LiteralValue::Integer(_) | LiteralValue::Float(_))
            )
        });

        if all_const {
            let const_val = self.generate_constant_array_value(var_type, elements, pos)?;
            self.builder.build_store(array_ptr, const_val)?;
            return Ok(());
        }

        // Runtime path: store each element via two-index GEP [0, i]
        // This correctly addresses into a [N x elem] array pointer.
        // i.e gep(array_ptr, [0, i]) = &(*array_ptr)[i]
        for (i, elem_expr) in elements.iter().enumerate() {
            let zero = self.context.i64_type().const_int(0, false);
            let index = self.context.i64_type().const_int(i as u64, false);
            let elem_ptr = unsafe {
                self.builder.build_in_bounds_gep(
                    array_llvm_type,
                    array_ptr,
                    &[zero, index],
                    &format!("list_init.{i}"),
                )?
            };
            let value = self.generate_coerced_value(elem_expr, Some(&elem_lang_type), EmitMode::Runtime)?;
            self.builder.build_store(elem_ptr, value)?;
        }

        // Zero-fill any remaining slots
        let zero_val = elem_llvm_type.const_zero();
        for i in elements.len()..array_size as usize {
            let zero = self.context.i64_type().const_int(0, false);
            let index = self.context.i64_type().const_int(i as u64, false);
            let elem_ptr = unsafe {
                self.builder.build_in_bounds_gep(
                    array_llvm_type,
                    array_ptr,
                    &[zero, index],
                    &format!("list_init_zero.{i}"),
                )?
            };
            self.builder.build_store(elem_ptr, zero_val)?;
        }
        Ok(())
    }

    /// Allocate memory — stack-allocated inside a function, globally allocated at module scope.
    pub(crate) fn generate_alloc(
        &mut self,
        alloc_type: &LangType,
        count: &Expression,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        if self.current_function.is_none() {
            // Global alloc: count must be a compile-time integer literal.
            match count.kind {
                ExprKind::Literal(LiteralValue::Integer(val)) => {
                    let llvm_type = alloc_type.to_llvm(self.context, count.pos)?;
                    let array_size = u32::try_from(val).map_err(|_| {
                        CodegenError::InvalidOperation(
                            "global allocation size too large".to_string(),
                            count.pos,
                        )
                    })?;
                    let array_type = llvm_type.array_type(array_size);
                    let global = self.module.add_global(array_type, None, ".global_alloc");
                    global.set_initializer(&array_type.const_zero());
                    Ok(global.as_pointer_value().into())
                }
                _ => Err(CodegenError::InvalidOperation(
                    "global allocation count must be a constant integer".to_string(),
                    count.pos,
                )),
            }
        } else {
            // Local alloc: evaluate count at runtime.
            let count_int = self.generate_expression(count)?.into_int_value();
            let llvm_type = alloc_type.to_llvm(self.context, count.pos)?;
            let alloca = self
                .builder
                .build_array_alloca(llvm_type, count_int, "alloca")
                .map_err(|_| {
                    CodegenError::InvalidOperation("failed to build alloca".to_string(), count.pos)
                })?;
            Ok(alloca.into())
        }
    }
}
