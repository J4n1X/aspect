//! `ValueEmitter` trait — the runtime/constant abstraction layer.
//!
//! Both `RuntimeEmitter` and `ConstantEmitter` implement `ValueEmitter<'ctx>`.
//! They perform identical arithmetic but materialise results differently:
//! - `RuntimeEmitter` emits LLVM IR instructions via the builder.
//! - `ConstantEmitter` folds values in Rust and reconstructs LLVM constants.
//!
//! Steps 10–12 of the refactoring plan route `generate_expression` and
//! `generate_constant_expression` through this trait so the duplicated logic
//! can be deleted.

use inkwell::{
    builder::Builder,
    context::Context,
    types::BasicTypeEnum,
    values::{BasicValueEnum, FloatValue, IntValue},
};

use crate::{
    codegen::types::{
        const_widen_ints_to_match, widen_floats_to_match, widen_ints_to_match, LangTypeExt,
    },
    codegen::CodegenError,
    lexer::{LangType, Position, TypeBase},
    parser::BinaryOp,
};

// ─── Trait ───────────────────────────────────────────────────────────────────

pub trait ValueEmitter<'ctx> {
    fn context(&self) -> &'ctx Context;

    /// Emit a binary operation on two `IntValue`s.
    ///
    /// The caller is responsible for widening both values to the same bit-width
    /// first (via `emit_widen_ints`).
    fn emit_int_binary(
        &self,
        op: &BinaryOp,
        lhs: IntValue<'ctx>,
        rhs: IntValue<'ctx>,
        is_signed: bool,
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError>;

    /// Emit a binary operation on two `FloatValue`s.
    ///
    /// The caller is responsible for widening both values first (via `emit_widen_floats`).
    fn emit_float_binary(
        &self,
        op: &BinaryOp,
        lhs: FloatValue<'ctx>,
        rhs: FloatValue<'ctx>,
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError>;

    /// Emit a type cast.
    fn emit_cast(
        &self,
        value: BasicValueEnum<'ctx>,
        target_llvm: BasicTypeEnum<'ctx>,
        src_lang: &LangType,
        dst_lang: &LangType,
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError>;

    /// Emit an integer literal at the given language type.
    ///
    /// Literals are LLVM constants in both modes, so this shared default
    /// (which only needs `context()`) serves both emitters.
    fn emit_int_literal(
        &self,
        val: i64,
        ty: &LangType,
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        match ty.to_llvm(self.context())? {
            BasicTypeEnum::IntType(int_ty) => Ok(int_ty.const_int(val as u64, true).into()),
            _ => Err(CodegenError::TypeError(
                "integer literal must have integer type".to_string(),
                pos,
            )),
        }
    }

    /// Emit a float literal at the given language type (shared default, as
    /// for [`ValueEmitter::emit_int_literal`]).
    fn emit_float_literal(
        &self,
        val: f64,
        ty: &LangType,
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        match ty.to_llvm(self.context())? {
            BasicTypeEnum::FloatType(float_ty) => Ok(float_ty.const_float(val).into()),
            _ => Err(CodegenError::TypeError(
                "float literal must have float type".to_string(),
                pos,
            )),
        }
    }

    /// Widen two integer values to the same bit-width.
    fn emit_widen_ints(
        &self,
        a: IntValue<'ctx>,
        a_signed: bool,
        b: IntValue<'ctx>,
        b_signed: bool,
    ) -> Result<(IntValue<'ctx>, IntValue<'ctx>), CodegenError>;

    /// Widen two float values to the same type.
    fn emit_widen_floats(
        &self,
        a: FloatValue<'ctx>,
        b: FloatValue<'ctx>,
    ) -> Result<(FloatValue<'ctx>, FloatValue<'ctx>), CodegenError>;
}

// ─── RuntimeEmitter ──────────────────────────────────────────────────────────

/// Emits LLVM IR instructions via the Inkwell builder.
pub struct RuntimeEmitter<'a, 'ctx> {
    pub builder: &'a Builder<'ctx>,
    pub context: &'ctx Context,
}

impl<'ctx> ValueEmitter<'ctx> for RuntimeEmitter<'_, 'ctx> {
    fn context(&self) -> &'ctx Context {
        self.context
    }

    fn emit_int_binary(
        &self,
        op: &BinaryOp,
        lhs: IntValue<'ctx>,
        rhs: IntValue<'ctx>,
        is_signed: bool,
        _pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let res = match op {
            // Signed arithmetic carries `nsw`: signed overflow is undefined in
            // TJLB, which lets LLVM assume `a + 1 > a` etc. Unsigned arithmetic
            // is defined to wrap, so it stays plain.
            BinaryOp::Add if is_signed => self
                .builder
                .build_int_nsw_add(lhs, rhs, "add")
                .map(Into::into)?,
            BinaryOp::Add => self
                .builder
                .build_int_add(lhs, rhs, "add")
                .map(Into::into)?,
            BinaryOp::Sub if is_signed => self
                .builder
                .build_int_nsw_sub(lhs, rhs, "sub")
                .map(Into::into)?,
            BinaryOp::Sub => self
                .builder
                .build_int_sub(lhs, rhs, "sub")
                .map(Into::into)?,
            BinaryOp::Mul if is_signed => self
                .builder
                .build_int_nsw_mul(lhs, rhs, "mul")
                .map(Into::into)?,
            BinaryOp::Mul => self
                .builder
                .build_int_mul(lhs, rhs, "mul")
                .map(Into::into)?,
            BinaryOp::Div => crate::signed_op!(
                self.builder,
                is_signed,
                build_int_signed_div,
                build_int_unsigned_div,
                lhs,
                rhs,
                "div"
            )
            .map(Into::into)?,
            BinaryOp::Mod => crate::signed_op!(
                self.builder,
                is_signed,
                build_int_signed_rem,
                build_int_unsigned_rem,
                lhs,
                rhs,
                "rem"
            )
            .map(Into::into)?,
            BinaryOp::And => self.builder.build_and(lhs, rhs, "and").map(Into::into)?,
            BinaryOp::Or => self.builder.build_or(lhs, rhs, "or").map(Into::into)?,
            BinaryOp::Xor => self.builder.build_xor(lhs, rhs, "xor").map(Into::into)?,
            BinaryOp::LeftShift => self
                .builder
                .build_left_shift(lhs, rhs, "shl")
                .map(Into::into)?,
            BinaryOp::RightShift => self
                .builder
                .build_right_shift(lhs, rhs, is_signed, "shr")
                .map(Into::into)?,
            BinaryOp::LogicalAnd => {
                let is_zero = self.builder.build_int_compare(
                    inkwell::IntPredicate::EQ,
                    lhs,
                    lhs.get_type().const_zero(),
                    "land_l",
                )?;
                let right_nonzero = self.builder.build_int_compare(
                    inkwell::IntPredicate::NE,
                    rhs,
                    rhs.get_type().const_zero(),
                    "land_r",
                )?;
                // Result is an i1 boolean; callers zero-extend if a wider type
                // is needed (e.g. storing into an integer variable).
                let i1_false = self.context.bool_type().const_int(0, false);
                self.builder
                    .build_select(is_zero, i1_false, right_nonzero, "land")?
            }
            BinaryOp::LogicalOr => {
                let is_nonzero = self.builder.build_int_compare(
                    inkwell::IntPredicate::NE,
                    lhs,
                    lhs.get_type().const_zero(),
                    "lor_l",
                )?;
                let right_nonzero = self.builder.build_int_compare(
                    inkwell::IntPredicate::NE,
                    rhs,
                    rhs.get_type().const_zero(),
                    "lor_r",
                )?;
                let i1_true = self.context.bool_type().const_int(1, false);
                self.builder
                    .build_select(is_nonzero, i1_true, right_nonzero, "lor")?
            }
        };
        Ok(res)
    }

    fn emit_float_binary(
        &self,
        op: &BinaryOp,
        lhs: FloatValue<'ctx>,
        rhs: FloatValue<'ctx>,
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        match op {
            BinaryOp::Add => Ok(self
                .builder
                .build_float_add(lhs, rhs, "fadd")
                .map(Into::into)?),
            BinaryOp::Sub => Ok(self
                .builder
                .build_float_sub(lhs, rhs, "fsub")
                .map(Into::into)?),
            BinaryOp::Mul => Ok(self
                .builder
                .build_float_mul(lhs, rhs, "fmul")
                .map(Into::into)?),
            BinaryOp::Div => Ok(self
                .builder
                .build_float_div(lhs, rhs, "fdiv")
                .map(Into::into)?),
            _ => Err(CodegenError::InvalidOperation(
                format!("operator {op:?} not supported for floats"),
                pos,
            )),
        }
    }

    fn emit_cast(
        &self,
        value: BasicValueEnum<'ctx>,
        target_llvm: BasicTypeEnum<'ctx>,
        src_lang: &LangType,
        dst_lang: &LangType,
        _pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        if value.get_type() == target_llvm {
            return Ok(value);
        }

        let target_is_pointer = matches!(target_llvm, BasicTypeEnum::PointerType(_));
        let target_is_float = matches!(target_llvm, BasicTypeEnum::FloatType(_));
        let target_is_int = matches!(target_llvm, BasicTypeEnum::IntType(_));

        if target_is_pointer {
            return if src_lang.pointer_depth == 0 {
                Ok(self
                    .builder
                    .build_int_to_ptr(
                        value.into_int_value(),
                        target_llvm.into_pointer_type(),
                        "inttoptr",
                    )?
                    .into())
            } else {
                Ok(self
                    .builder
                    .build_pointer_cast(
                        value.into_pointer_value(),
                        target_llvm.into_pointer_type(),
                        "ptrcast",
                    )?
                    .into())
            };
        }

        if target_is_float && value.is_int_value() {
            let int_val = value.into_int_value();
            let is_signed = matches!(src_lang.base, TypeBase::SInt);
            return Ok(if is_signed {
                self.builder
                    .build_signed_int_to_float(int_val, target_llvm.into_float_type(), "sitofp")?
                    .into()
            } else {
                self.builder
                    .build_unsigned_int_to_float(int_val, target_llvm.into_float_type(), "uitofp")?
                    .into()
            });
        }

        if target_is_int && value.is_float_value() {
            let float_val = value.into_float_value();
            let target_int_type = target_llvm.into_int_type();
            let target_signed = matches!(dst_lang.base, TypeBase::SInt);
            return Ok(if target_signed {
                self.builder
                    .build_float_to_signed_int(float_val, target_int_type, "fptosi")?
                    .into()
            } else {
                self.builder
                    .build_float_to_unsigned_int(float_val, target_int_type, "fptoui")?
                    .into()
            });
        }

        if target_is_int && value.is_pointer_value() {
            return Ok(self
                .builder
                .build_ptr_to_int(
                    value.into_pointer_value(),
                    target_llvm.into_int_type(),
                    "ptrtoint",
                )?
                .into());
        }

        if target_is_int && value.is_int_value() {
            let int_val = value.into_int_value();
            let target_int_type = target_llvm.into_int_type();
            let source_bits = int_val.get_type().get_bit_width();
            let target_bits = target_int_type.get_bit_width();
            let is_signed = matches!(src_lang.base, TypeBase::SInt);

            return match target_bits.cmp(&source_bits) {
                std::cmp::Ordering::Greater => {
                    let use_zext = source_bits == 1 || !is_signed;
                    Ok(if use_zext {
                        self.builder
                            .build_int_z_extend(int_val, target_int_type, "zext")?
                            .into()
                    } else {
                        self.builder
                            .build_int_s_extend(int_val, target_int_type, "sext")?
                            .into()
                    })
                }
                std::cmp::Ordering::Less => Ok(self
                    .builder
                    .build_int_truncate(int_val, target_int_type, "trunc")?
                    .into()),
                std::cmp::Ordering::Equal => Ok(value),
            };
        }

        Ok(value)
    }

    fn emit_widen_ints(
        &self,
        a: IntValue<'ctx>,
        a_signed: bool,
        b: IntValue<'ctx>,
        b_signed: bool,
    ) -> Result<(IntValue<'ctx>, IntValue<'ctx>), CodegenError> {
        Ok(widen_ints_to_match(self.builder, a, a_signed, b, b_signed)?)
    }

    fn emit_widen_floats(
        &self,
        a: FloatValue<'ctx>,
        b: FloatValue<'ctx>,
    ) -> Result<(FloatValue<'ctx>, FloatValue<'ctx>), CodegenError> {
        Ok(widen_floats_to_match(self.context, self.builder, a, b)?)
    }
}

// ─── ConstantEmitter ─────────────────────────────────────────────────────────

/// Folds values in Rust and reconstructs LLVM constants (no builder required).
///
/// Used for global initializers and `try_fold_constant_expression`.
pub struct ConstantEmitter<'ctx> {
    pub context: &'ctx Context,
}

impl<'ctx> ValueEmitter<'ctx> for ConstantEmitter<'ctx> {
    fn context(&self) -> &'ctx Context {
        self.context
    }

    fn emit_int_binary(
        &self,
        op: &BinaryOp,
        lhs: IntValue<'ctx>,
        rhs: IntValue<'ctx>,
        is_signed: bool,
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let result_type = if lhs.get_type().get_bit_width() >= rhs.get_type().get_bit_width() {
            lhs.get_type()
        } else {
            rhs.get_type()
        };

        let lv = lhs.get_zero_extended_constant().ok_or_else(|| {
            CodegenError::InvalidOperation(
                "constant integer not representable as u64".to_string(),
                pos,
            )
        })?;
        let rv = rhs.get_zero_extended_constant().ok_or_else(|| {
            CodegenError::InvalidOperation(
                "constant integer not representable as u64".to_string(),
                pos,
            )
        })?;

        let result: u64 = match op {
            BinaryOp::Add => lv.wrapping_add(rv),
            BinaryOp::Sub => lv.wrapping_sub(rv),
            BinaryOp::Mul => lv.wrapping_mul(rv),
            BinaryOp::Div => {
                if is_signed {
                    (lv as i64).checked_div(rv as i64).ok_or_else(|| {
                        CodegenError::InvalidOperation(
                            "division by zero in constant expression".to_string(),
                            pos,
                        )
                    })? as u64
                } else {
                    lv.checked_div(rv).ok_or_else(|| {
                        CodegenError::InvalidOperation(
                            "division by zero in constant expression".to_string(),
                            pos,
                        )
                    })?
                }
            }
            BinaryOp::Mod => {
                if is_signed {
                    (lv as i64).checked_rem(rv as i64).ok_or_else(|| {
                        CodegenError::InvalidOperation(
                            "division by zero in constant expression".to_string(),
                            pos,
                        )
                    })? as u64
                } else {
                    lv.checked_rem(rv).ok_or_else(|| {
                        CodegenError::InvalidOperation(
                            "division by zero in constant expression".to_string(),
                            pos,
                        )
                    })?
                }
            }
            BinaryOp::And => lv & rv,
            BinaryOp::Or => lv | rv,
            BinaryOp::Xor => lv ^ rv,
            BinaryOp::LeftShift => lv.wrapping_shl(rv as u32 % 64),
            BinaryOp::RightShift => {
                if is_signed {
                    ((lv as i64).wrapping_shr(rv as u32 % 64)) as u64
                } else {
                    lv >> (rv % 64)
                }
            }
            BinaryOp::LogicalAnd => u64::from(lv != 0 && rv != 0),
            BinaryOp::LogicalOr => u64::from(lv != 0 || rv != 0),
        };

        // Logical ops always produce i32 to match the runtime behaviour.
        if matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr) {
            Ok(self.context.i32_type().const_int(result, false).into())
        } else {
            Ok(result_type.const_int(result, is_signed).into())
        }
    }

    fn emit_float_binary(
        &self,
        op: &BinaryOp,
        lhs: FloatValue<'ctx>,
        rhs: FloatValue<'ctx>,
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let (lv, _) = lhs.get_constant().ok_or_else(|| {
            CodegenError::InvalidOperation("float constant not representable".to_string(), pos)
        })?;
        let (rv, _) = rhs.get_constant().ok_or_else(|| {
            CodegenError::InvalidOperation("float constant not representable".to_string(), pos)
        })?;

        let result: f64 = match op {
            BinaryOp::Add => lv + rv,
            BinaryOp::Sub => lv - rv,
            BinaryOp::Mul => lv * rv,
            BinaryOp::Div => lv / rv,
            _ => {
                return Err(CodegenError::InvalidOperation(
                    format!("operator {op:?} not supported for float constant expressions"),
                    pos,
                ))
            }
        };

        let result_type = if lhs.get_type() == self.context.f64_type()
            || rhs.get_type() == self.context.f64_type()
        {
            self.context.f64_type()
        } else {
            self.context.f32_type()
        };
        Ok(result_type.const_float(result).into())
    }

    fn emit_cast(
        &self,
        value: BasicValueEnum<'ctx>,
        target_llvm: BasicTypeEnum<'ctx>,
        src_lang: &LangType,
        dst_lang: &LangType,
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        if value.get_type() == target_llvm {
            return Ok(value);
        }

        let target_is_int = matches!(target_llvm, BasicTypeEnum::IntType(_));
        let target_is_float = matches!(target_llvm, BasicTypeEnum::FloatType(_));
        let target_is_pointer = matches!(target_llvm, BasicTypeEnum::PointerType(_));

        // int → int resize (LLVM 19: extract + reconstruct)
        if target_is_int && value.is_int_value() {
            let int_val = value.into_int_value();
            let target_int_type = target_llvm.into_int_type();
            let src_bits = int_val.get_type().get_bit_width();
            let dst_bits = target_int_type.get_bit_width();
            let sign_extend = matches!(src_lang.base, TypeBase::SInt) && src_bits > 1;
            return Ok(match dst_bits.cmp(&src_bits) {
                std::cmp::Ordering::Greater => {
                    let raw = if sign_extend {
                        int_val.get_sign_extended_constant().ok_or_else(|| {
                            CodegenError::InvalidOperation(
                                "integer constant not representable as i64 for widening cast"
                                    .to_string(),
                                pos,
                            )
                        })? as u64
                    } else {
                        int_val.get_zero_extended_constant().ok_or_else(|| {
                            CodegenError::InvalidOperation(
                                "integer constant not representable as u64 for widening cast"
                                    .to_string(),
                                pos,
                            )
                        })?
                    };
                    target_int_type.const_int(raw, sign_extend)
                }
                std::cmp::Ordering::Less => int_val.const_truncate(target_int_type),
                std::cmp::Ordering::Equal => int_val,
            }
            .into());
        }

        // int → float
        if target_is_float && value.is_int_value() {
            let int_val = value.into_int_value();
            let float_type = target_llvm.into_float_type();
            let is_signed = matches!(src_lang.base, TypeBase::SInt);
            let fval = if is_signed {
                int_val.get_sign_extended_constant().ok_or_else(|| {
                    CodegenError::InvalidOperation(
                        "integer constant not representable as i64 for cast".to_string(),
                        pos,
                    )
                })? as f64
            } else {
                int_val.get_zero_extended_constant().ok_or_else(|| {
                    CodegenError::InvalidOperation(
                        "integer constant not representable as u64 for cast".to_string(),
                        pos,
                    )
                })? as f64
            };
            return Ok(float_type.const_float(fval).into());
        }

        // float → int
        if target_is_int && value.is_float_value() {
            let float_val = value.into_float_value();
            let int_type = target_llvm.into_int_type();
            let target_signed = matches!(dst_lang.base, TypeBase::SInt);
            let (fval, _) = float_val.get_constant().ok_or_else(|| {
                CodegenError::InvalidOperation(
                    "float constant not representable for cast".to_string(),
                    pos,
                )
            })?;
            let bits = if target_signed {
                fval as i64 as u64
            } else {
                fval as u64
            };
            return Ok(int_type.const_int(bits, target_signed).into());
        }

        // float → float
        if target_is_float && value.is_float_value() {
            let float_val = value.into_float_value();
            let float_type = target_llvm.into_float_type();
            let (fval, _) = float_val.get_constant().ok_or_else(|| {
                CodegenError::InvalidOperation(
                    "float constant not representable for cast".to_string(),
                    pos,
                )
            })?;
            return Ok(float_type.const_float(fval).into());
        }

        // pointer → pointer (opaque ptrs are all the same LLVM type; no-op)
        if target_is_pointer && value.is_pointer_value() {
            return Ok(value);
        }

        // int → pointer
        if target_is_pointer && value.is_int_value() {
            return Ok(value
                .into_int_value()
                .const_to_pointer(target_llvm.into_pointer_type())
                .into());
        }

        // pointer → int
        if target_is_int && value.is_pointer_value() {
            return Ok(value
                .into_pointer_value()
                .const_to_int(target_llvm.into_int_type())
                .into());
        }

        Err(CodegenError::InvalidOperation(
            format!(
                "cast from {} to {} is not supported in constant expressions",
                value.get_type(),
                target_llvm
            ),
            pos,
        ))
    }

    fn emit_widen_ints(
        &self,
        a: IntValue<'ctx>,
        a_signed: bool,
        b: IntValue<'ctx>,
        b_signed: bool,
    ) -> Result<(IntValue<'ctx>, IntValue<'ctx>), CodegenError> {
        Ok(const_widen_ints_to_match(a, a_signed, b, b_signed))
    }

    fn emit_widen_floats(
        &self,
        a: FloatValue<'ctx>,
        b: FloatValue<'ctx>,
    ) -> Result<(FloatValue<'ctx>, FloatValue<'ctx>), CodegenError> {
        // No builder needed: pick the wider type and reconstruct.
        if a.get_type() == b.get_type() {
            return Ok((a, b));
        }
        let a_is_f64 = a.get_type() == self.context.f64_type();
        let (fval_a, _) = a.get_constant().unwrap_or((0.0, false));
        let (fval_b, _) = b.get_constant().unwrap_or((0.0, false));
        if a_is_f64 {
            Ok((a, self.context.f64_type().const_float(fval_b)))
        } else {
            Ok((self.context.f64_type().const_float(fval_a), b))
        }
    }
}
