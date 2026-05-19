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

use crate::codegen::generator::CodeGenerator;
use crate::codegen::types::{float_cmp_pred, int_cmp_pred, LangTypeExt};
use crate::codegen::value_emitter::{ConstantEmitter, RuntimeEmitter, ValueEmitter};
use crate::codegen::CodegenError;
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

/// Dispatch a non-pointer binary operation through the right emitter.
fn emit_binary_dispatch<'ctx, E: ValueEmitter<'ctx>>(
    e: &E,
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

/// Dispatch a cast through the right emitter.
fn emit_cast_dispatch<'ctx, E: ValueEmitter<'ctx>>(
    e: &E,
    val: BasicValueEnum<'ctx>,
    target_llvm: BasicTypeEnum<'ctx>,
    src_lang: &LangType,
    dst_lang: &LangType,
    pos: Position,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    e.emit_cast(val, target_llvm, src_lang, dst_lang, pos)
}

// ─── Main walker ──────────────────────────────────────────────────────────────

/// Recursively evaluate `expr`.
///
/// `mode` controls whether leaf operations emit IR (`Runtime`) or fold
/// constants in Rust (`Constant`).  Callers that need the runtime path
/// should call `generate_expression` instead.
pub(crate) fn walk_expression<'ctx>(
    expr: &Expression,
    gen: &mut CodeGenerator<'ctx>,
    mode: EmitMode,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    match &expr.kind {
        // ── Literals ──────────────────────────────────────────────────────
        ExprKind::Literal(lit) => match lit {
            LiteralValue::Integer(val) => match mode {
                EmitMode::Runtime => RuntimeEmitter {
                    builder: &gen.builder,
                    context: gen.context,
                }
                .emit_int_literal(*val, &expr.expr_type, expr.pos),
                EmitMode::Constant => ConstantEmitter {
                    context: gen.context,
                }
                .emit_int_literal(*val, &expr.expr_type, expr.pos),
            },
            LiteralValue::Float(val) => match mode {
                EmitMode::Runtime => RuntimeEmitter {
                    builder: &gen.builder,
                    context: gen.context,
                }
                .emit_float_literal(*val, &expr.expr_type, expr.pos),
                EmitMode::Constant => ConstantEmitter {
                    context: gen.context,
                }
                .emit_float_literal(*val, &expr.expr_type, expr.pos),
            },
            LiteralValue::String(index) => {
                let string_name = format!(".str.{index}");
                let ptr = gen
                    .scope
                    .lookup_global(&string_name)
                    .expect("Internal error: String literal global not found")
                    .ptr;
                let i8_ptr_type = gen.context.ptr_type(AddressSpace::default());
                match mode {
                    EmitMode::Runtime => Ok(gen
                        .builder
                        .build_pointer_cast(ptr, i8_ptr_type, "str")?
                        .into()),
                    EmitMode::Constant => Ok(ptr.const_cast(i8_ptr_type).into()),
                }
            }
        },

        // ── Variable load ─────────────────────────────────────────────────
        ExprKind::Variable(name) => match mode {
            EmitMode::Runtime => {
                let (ptr, llvm_type, lang_type, const_value) = {
                    let v = gen
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

                let loaded = gen.builder.build_load(llvm_type, ptr, name)?;

                if lang_type.is_const {
                    let kind_id = gen.context.get_kind_id("invariant.load");
                    let md = gen.context.metadata_node(&[]);
                    let instr = match loaded {
                        BasicValueEnum::IntValue(v) => v.as_instruction_value(),
                        BasicValueEnum::FloatValue(v) => v.as_instruction_value(),
                        BasicValueEnum::PointerValue(v) => v.as_instruction_value(),
                        _ => None,
                    };
                    if let Some(instr) = instr {
                        let _ = instr.set_metadata(md, kind_id);
                    }
                }

                Ok(loaded)
            }
            EmitMode::Constant => {
                // Check local scope first (const locals store their folded value).
                for scope in gen.scope.iter_scopes() {
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
                let global_val = gen
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
            // Evaluate sub-expressions first (recursive, needs &mut gen).
            let left_val = walk_expression(left, gen, mode)?;
            let right_val = walk_expression(right, gen, mode)?;

            if left.expr_type.pointer_depth > 0 {
                if mode == EmitMode::Constant {
                    return Err(CodegenError::InvalidOperation(
                        "pointer arithmetic not supported in constant expressions".to_string(),
                        expr.pos,
                    ));
                }
                if right.expr_type.pointer_depth > 0 {
                    return Err(CodegenError::InvalidOperation(
                        "pointer arithmetic only allowed with integers".to_string(),
                        left.pos,
                    ));
                }
                let left_ptr = left_val.into_pointer_value();
                let right_int = right_val.into_int_value();
                let pointee_type = left.expr_type.pointee().to_llvm(gen.context)?;
                return match op {
                    BinaryOp::Add => unsafe {
                        Ok(gen
                            .builder
                            .build_gep(pointee_type, left_ptr, &[right_int], "ptr_add")
                            .map(Into::into)?)
                    },
                    BinaryOp::Sub => {
                        let neg = gen.builder.build_int_neg(right_int, "neg")?;
                        unsafe {
                            Ok(gen
                                .builder
                                .build_gep(pointee_type, left_ptr, &[neg], "ptr_sub")
                                .map(Into::into)?)
                        }
                    }
                    _ => Err(CodegenError::InvalidOperation(
                        format!("operator {op:?} not supported for pointers"),
                        left.pos,
                    )),
                };
            }

            // Scalar: dispatch through the right emitter (emitter created after recursive calls).
            match mode {
                EmitMode::Runtime => {
                    let e = RuntimeEmitter {
                        builder: &gen.builder,
                        context: gen.context,
                    };
                    emit_binary_dispatch(
                        &e,
                        left_val,
                        right_val,
                        op,
                        &left.expr_type,
                        &right.expr_type,
                        expr.pos,
                    )
                }
                EmitMode::Constant => {
                    let e = ConstantEmitter {
                        context: gen.context,
                    };
                    emit_binary_dispatch(
                        &e,
                        left_val,
                        right_val,
                        op,
                        &left.expr_type,
                        &right.expr_type,
                        expr.pos,
                    )
                }
            }
        }

        // ── Comparison (runtime only) ─────────────────────────────────────
        ExprKind::Comparison { left, op, right } => {
            if mode == EmitMode::Constant {
                return Err(CodegenError::InvalidOperation(
                    "comparison not supported in constant expressions".to_string(),
                    expr.pos,
                ));
            }
            let left_val = walk_expression(left, gen, mode)?;
            let right_val = walk_expression(right, gen, mode)?;

            if left.expr_type.pointer_depth > 0 && right.expr_type.pointer_depth > 0 {
                return Ok(gen
                    .builder
                    .build_int_compare(int_cmp_pred(op, false), left_val.into_pointer_value(), right_val.into_pointer_value(), "ptr_cmp")?
                    .into());
            } else if matches!(left.expr_type.base, TypeBase::SFloat) {
                let lf = left_val.into_float_value();
                let rf = right_val.into_float_value();
                let (lf, rf) = {
                    let e = RuntimeEmitter {
                        builder: &gen.builder,
                        context: gen.context,
                    };
                    e.emit_widen_floats(lf, rf)?
                };
                Ok(gen
                    .builder
                    .build_float_compare(float_cmp_pred(op), lf, rf, "fcmp")?
                    .into())
            } else {
                let is_signed = matches!(left.expr_type.base, TypeBase::SInt);
                let right_signed = matches!(right.expr_type.base, TypeBase::SInt);
                let li = left_val.into_int_value();
                let ri = right_val.into_int_value();
                let (li, ri) = {
                    let e = RuntimeEmitter {
                        builder: &gen.builder,
                        context: gen.context,
                    };
                    e.emit_widen_ints(li, is_signed, ri, right_signed)?
                };
                Ok(gen
                    .builder
                    .build_int_compare(int_cmp_pred(op, is_signed), li, ri, "icmp")?
                    .into())
            }
        }

        // ── Address-of ────────────────────────────────────────────────────
        ExprKind::Reference(inner) => match &inner.kind {
            ExprKind::Variable(name) => match mode {
                EmitMode::Runtime => {
                    let ptr = gen
                        .scope
                        .lookup_any(name)
                        .ok_or_else(|| CodegenError::UndefinedVariable(name.clone(), inner.pos))?
                        .ptr();
                    Ok(ptr.into())
                }
                EmitMode::Constant => {
                    let ptr = gen
                        .scope
                        .lookup_global(name.as_str())
                        .ok_or_else(|| CodegenError::UndefinedVariable(name.clone(), inner.pos))?
                        .ptr;
                    Ok(ptr.into())
                }
            },
            ExprKind::Dereference(inner2) => walk_expression(inner2, gen, mode),
            _ => Err(CodegenError::InvalidOperation(
                "Cannot take address of non-lvalue".to_string(),
                inner.pos,
            )),
        },

        // ── Dereference (runtime only) ────────────────────────────────────
        ExprKind::Dereference(inner_expr) => {
            if mode == EmitMode::Constant {
                return Err(CodegenError::InvalidOperation(
                    "dereference not supported in constant expressions".to_string(),
                    expr.pos,
                ));
            }
            let ptr = walk_expression(inner_expr, gen, mode)?;
            let derefed_type = if inner_expr.expr_type.pointer_depth == 0 {
                return Err(CodegenError::TypeError(
                    "Cannot dereference a non-pointer type".to_string(),
                    expr.pos,
                ));
            } else {
                LangType {
                    base: inner_expr.expr_type.base,
                    size_bits: inner_expr.expr_type.size_bits,
                    pointer_depth: inner_expr.expr_type.pointer_depth - 1,
                    is_const: inner_expr.expr_type.is_const,
                    array_size: None,
                }
            };
            let pointee_type = derefed_type.to_llvm(gen.context)?;
            Ok(gen
                .builder
                .build_load(pointee_type, ptr.into_pointer_value(), "deref")?)
        }

        // ── Function call (runtime only) ──────────────────────────────────
        ExprKind::FunctionCall { name, args } => {
            if mode == EmitMode::Constant {
                return Err(CodegenError::InvalidOperation(
                    "function calls not supported in constant expressions".to_string(),
                    expr.pos,
                ));
            }
            gen.generate_function_call(name, args, expr.pos)
        }

        // ── Cast ──────────────────────────────────────────────────────────
        ExprKind::Cast {
            expr: inner,
            target_type,
        } => {
            let val = walk_expression(inner, gen, mode)?;
            let target_llvm = target_type.to_llvm(gen.context)?;
            match mode {
                EmitMode::Runtime => {
                    let e = RuntimeEmitter {
                        builder: &gen.builder,
                        context: gen.context,
                    };
                    emit_cast_dispatch(
                        &e,
                        val,
                        target_llvm,
                        &inner.expr_type,
                        target_type,
                        inner.pos,
                    )
                }
                EmitMode::Constant => {
                    let e = ConstantEmitter {
                        context: gen.context,
                    };
                    emit_cast_dispatch(
                        &e,
                        val,
                        target_llvm,
                        &inner.expr_type,
                        target_type,
                        inner.pos,
                    )
                }
            }
        }

        // ── Alloc (both modes — context determines stack vs global) ───────
        ExprKind::Alloc { alloc_type, count } => gen.generate_alloc(alloc_type, count),

        // ── Logical / bitwise NOT ─────────────────────────────────────────
        ExprKind::UnaryNot(inner) => {
            let val = walk_expression(inner, gen, mode)?.into_int_value();
            match mode {
                EmitMode::Runtime => {
                    let zero = val.get_type().const_zero();
                    let cmp =
                        gen.builder
                            .build_int_compare(IntPredicate::EQ, val, zero, "nottmp")?;
                    Ok(gen
                        .builder
                        .build_int_z_extend(cmp, gen.context.i32_type(), "nottmp_ext")?
                        .into())
                }
                EmitMode::Constant => {
                    let n = val.get_zero_extended_constant().ok_or_else(|| {
                        CodegenError::InvalidOperation(
                            "constant integer not representable as u64".to_string(),
                            inner.pos,
                        )
                    })?;
                    Ok(gen
                        .context
                        .i32_type()
                        .const_int(u64::from(n == 0), false)
                        .into())
                }
            }
        }

        ExprKind::BitwiseNot(inner) => {
            let val = walk_expression(inner, gen, mode)?.into_int_value();
            match mode {
                EmitMode::Runtime => Ok(gen.builder.build_not(val, "bnottmp")?.into()),
                EmitMode::Constant => Ok(val.const_not().into()),
            }
        }

        // ── List initializer (always invalid as a standalone expression) ──
        ExprKind::ListInitializer(_) => Err(CodegenError::InvalidOperation(
            "list initializer is only valid in a variable declaration".to_string(),
            expr.pos,
        )),
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

    /// Generate an expression, coercing it to `target` when the types differ.
    ///
    /// Integer/float literals assigned to a concrete target type are checked for
    /// overflow at this stage and emitted directly at the target width.
    pub(crate) fn generate_coerced_value(
        &mut self,
        expr: &Expression,
        target: Option<&LangType>,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        // Fast path: literal assigned to a scalar target — emit at target type with overflow check.
        if let Some(target_ty) = target {
            if target_ty.pointer_depth == 0 && !target_ty.is_array() {
                if let ExprKind::Literal(
                    lit @ (LiteralValue::Integer(_) | LiteralValue::Float(_)),
                ) = &expr.kind
                {
                    return self.generate_literal_typed(lit, target_ty, expr.pos);
                }
            }
        }

        let val = self.generate_expression(expr)?;

        // Auto-widen to target if types differ.
        if let Some(target_ty) = target {
            if target_ty.pointer_depth == 0 && !target_ty.is_array() {
                let target_llvm = target_ty.to_llvm(self.context)?;
                if val.get_type() != target_llvm {
                    let e = RuntimeEmitter {
                        builder: &self.builder,
                        context: self.context,
                    };
                    return e.emit_cast(val, target_llvm, &expr.expr_type, target_ty, expr.pos);
                }
            }
        }

        Ok(val)
    }

    /// Generate a literal with an explicit target type and overflow checking.
    #[allow(clippy::cast_sign_loss)]
    pub(crate) fn generate_literal_typed(
        &self,
        lit: &LiteralValue,
        ty: &LangType,
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        match lit {
            LiteralValue::Integer(val) => {
                let llvm_type = ty.to_llvm(self.context)?;
                match llvm_type {
                    BasicTypeEnum::IntType(int_ty) => {
                        let bits = int_ty.get_bit_width();
                        if bits < 64 {
                            let fits = if matches!(ty.base, TypeBase::SInt) {
                                let min = -(1i64 << (bits - 1));
                                let max = (1i64 << (bits - 1)) - 1;
                                *val >= min && *val <= max
                            } else {
                                *val >= 0 && (*val as u64) < (1u64 << bits)
                            };
                            if !fits {
                                return Err(CodegenError::TypeError(
                                    format!("integer literal {} overflows {}", val, ty),
                                    pos,
                                ));
                            }
                        }
                        Ok(int_ty.const_int(*val as u64, true).into())
                    }
                    _ => Err(CodegenError::TypeError(
                        "integer literal must have integer type".to_string(),
                        pos,
                    )),
                }
            }
            LiteralValue::Float(val) => {
                let llvm_type = ty.to_llvm(self.context)?;
                match llvm_type {
                    BasicTypeEnum::FloatType(float_ty) => Ok(float_ty.const_float(*val).into()),
                    _ => Err(CodegenError::TypeError(
                        "float literal must have float type".to_string(),
                        pos,
                    )),
                }
            }
            LiteralValue::String(index) => {
                let string_name = format!(".str.{index}");
                let ptr = self
                    .scope
                    .lookup_global(&string_name)
                    .expect("Internal error: String literal global not found")
                    .ptr;
                let i8_ptr_type = self.context.ptr_type(AddressSpace::default());
                Ok(self
                    .builder
                    .build_pointer_cast(ptr, i8_ptr_type, "str")?
                    .into())
            }
        }
    }

    pub(crate) fn generate_list_initializer(
        &mut self,
        array_ptr: PointerValue<'ctx>,
        var_type: &LangType,
        elements: &[Expression],
    ) -> Result<(), CodegenError> {
        let elem_lang_type = var_type.element_type();
        let elem_llvm_type = elem_lang_type.to_llvm(self.context)?;
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
            let const_val = self.generate_constant_array_value(var_type, elements)?;
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
                self.builder.build_gep(
                    array_llvm_type,
                    array_ptr,
                    &[zero, index],
                    &format!("list_init.{i}"),
                )?
            };
            let value = self.generate_coerced_value(elem_expr, Some(&elem_lang_type))?;
            self.builder.build_store(elem_ptr, value)?;
        }

        // Zero-fill any remaining slots
        let zero_val = elem_llvm_type.const_zero();
        for i in elements.len()..array_size as usize {
            let zero = self.context.i64_type().const_int(0, false);
            let index = self.context.i64_type().const_int(i as u64, false);
            let elem_ptr = unsafe {
                self.builder.build_gep(
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
                    let llvm_type = alloc_type.to_llvm(self.context)?;
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
            let llvm_type = alloc_type.to_llvm(self.context)?;
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
