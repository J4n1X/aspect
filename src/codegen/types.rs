use inkwell::AddressSpace;
use inkwell::context::Context;
use inkwell::types::{BasicType, BasicTypeEnum, IntType, FloatType, ArrayType};
use inkwell::values::{FloatValue, IntValue};
use inkwell::builder::{Builder, BuilderError};
use inkwell::IntPredicate;
use inkwell::FloatPredicate;
use crate::lexer::{LangType, TypeBase};
use crate::codegen::CodegenError;
use crate::parser::ComparisonOp;

// ─── Sign-dispatch macros ─────────────────────────────────────────────────────

/// Dispatch to the signed or unsigned variant of a builder method.
///
/// Usage: `signed_op!(builder, is_signed, signed_method, unsigned_method, arg1, arg2, ...)`
///
/// Example:
/// ```ignore
/// let val = signed_op!(self.builder, is_signed,
///     build_int_signed_div, build_int_unsigned_div,
///     left, right, "div")?;
/// ```
#[macro_export]
macro_rules! signed_op {
    ($builder:expr, $is_signed:expr, $signed:ident, $unsigned:ident, $($arg:expr),+) => {
        if $is_signed {
            $builder.$signed($($arg),+)
        } else {
            $builder.$unsigned($($arg),+)
        }
    };
}

/// Dispatch to the signed or unsigned const method on an `IntValue` (no builder needed).
///
/// Usage: `const_signed_op!(int_value, is_signed, const_signed_div, const_unsigned_div, rhs)`
#[macro_export]
macro_rules! const_signed_op {
    ($val:expr, $is_signed:expr, $signed:ident, $unsigned:ident, $arg:expr) => {
        if $is_signed { $val.$signed($arg) } else { $val.$unsigned($arg) }
    };
}

// ─── Width-matching helpers ───────────────────────────────────────────────────

/// Widen the narrower of two integer values so both have the same bit-width.
///
/// Uses `sext` for signed values and `zext` for unsigned.
/// If widths already match, returns the values unchanged.
///
/// # Errors
/// Propagates any `BuilderError` from the underlying LLVM builder.
pub fn widen_ints_to_match<'ctx>(
    builder: &Builder<'ctx>,
    a: IntValue<'ctx>,
    a_signed: bool,
    b: IntValue<'ctx>,
    b_signed: bool,
) -> Result<(IntValue<'ctx>, IntValue<'ctx>), BuilderError> {
    let a_bits = a.get_type().get_bit_width();
    let b_bits = b.get_type().get_bit_width();

    if a_bits > b_bits {
        let b_wide = if b_signed {
            builder.build_int_s_extend(b, a.get_type(), "widen")?
        } else {
            builder.build_int_z_extend(b, a.get_type(), "widen")?
        };
        Ok((a, b_wide))
    } else if b_bits > a_bits {
        let a_wide = if a_signed {
            builder.build_int_s_extend(a, b.get_type(), "widen")?
        } else {
            builder.build_int_z_extend(a, b.get_type(), "widen")?
        };
        Ok((a_wide, b))
    } else {
        Ok((a, b))
    }
}

/// Widen the narrower of two float values so both share the same type.
///
/// # Errors
/// Propagates any `BuilderError` from the underlying LLVM builder.
pub fn widen_floats_to_match<'ctx>(
    context: &'ctx Context,
    builder: &Builder<'ctx>,
    a: FloatValue<'ctx>,
    b: FloatValue<'ctx>,
) -> Result<(FloatValue<'ctx>, FloatValue<'ctx>), BuilderError> {
    if a.get_type() == b.get_type() {
        return Ok((a, b));
    }
    // Determine which is wider by bit-width of the LLVM type
    // f64 > f32; compare via display name length as a heuristic-free approach
    let a_is_f64 = a.get_type() == context.f64_type();
    if a_is_f64 {
        let b_wide = builder.build_float_ext(b, a.get_type(), "fpwiden")?;
        Ok((a, b_wide))
    } else {
        let a_wide = builder.build_float_ext(a, b.get_type(), "fpwiden")?;
        Ok((a_wide, b))
    }
}

/// Widen the narrower of two integer constant values so both have the same bit-width.
///
/// LLVM 19 removed most `LLVMConst*` functions, so this is done by extracting the Rust
/// value with `get_zero_extended_constant` / `get_sign_extended_constant` and reconstructing
/// the constant at the wider type.
/// If widths already match, returns the values unchanged.
pub fn const_widen_ints_to_match<'ctx>(
    a: IntValue<'ctx>,
    a_signed: bool,
    b: IntValue<'ctx>,
    b_signed: bool,
) -> (IntValue<'ctx>, IntValue<'ctx>) {
    let a_bits = a.get_type().get_bit_width();
    let b_bits = b.get_type().get_bit_width();
    if a_bits > b_bits {
        // Widen b to a's type
        let raw = if b_signed {
            b.get_sign_extended_constant().unwrap_or(0) as u64
        } else {
            b.get_zero_extended_constant().unwrap_or(0)
        };
        let b_wide = a.get_type().const_int(raw, b_signed);
        (a, b_wide)
    } else if b_bits > a_bits {
        // Widen a to b's type
        let raw = if a_signed {
            a.get_sign_extended_constant().unwrap_or(0) as u64
        } else {
            a.get_zero_extended_constant().unwrap_or(0)
        };
        let a_wide = b.get_type().const_int(raw, a_signed);
        (a_wide, b)
    } else {
        (a, b)
    }
}

// ─── Comparison predicate helpers ────────────────────────────────────────────

/// Return the correct `IntPredicate` for a comparison operation.
///
/// Signed operations use `S`-prefixed predicates; unsigned use `U`-prefixed.
/// `EQ` and `NE` are the same regardless of signedness.
#[must_use]
pub fn int_cmp_pred(op: &ComparisonOp, is_signed: bool) -> IntPredicate {
    match op {
        ComparisonOp::Equal        => IntPredicate::EQ,
        ComparisonOp::NotEqual     => IntPredicate::NE,
        ComparisonOp::Less         => if is_signed { IntPredicate::SLT } else { IntPredicate::ULT },
        ComparisonOp::Greater      => if is_signed { IntPredicate::SGT } else { IntPredicate::UGT },
        ComparisonOp::LessEqual    => if is_signed { IntPredicate::SLE } else { IntPredicate::ULE },
        ComparisonOp::GreaterEqual => if is_signed { IntPredicate::SGE } else { IntPredicate::UGE },
    }
}

/// Return the ordered `FloatPredicate` for a comparison operation.
#[must_use]
pub fn float_cmp_pred(op: &ComparisonOp) -> FloatPredicate {
    match op {
        ComparisonOp::Equal        => FloatPredicate::OEQ,
        ComparisonOp::NotEqual     => FloatPredicate::ONE,
        ComparisonOp::Less         => FloatPredicate::OLT,
        ComparisonOp::Greater      => FloatPredicate::OGT,
        ComparisonOp::LessEqual    => FloatPredicate::OLE,
        ComparisonOp::GreaterEqual => FloatPredicate::OGE,
    }
}


/// Convert a `LangType` to an LLVM type
/// 
/// For array types (e.g., `u32[4]`), this returns a pointer type since
/// array variables decay to pointers. Use `lang_type_to_llvm_array` to get
/// the actual array type for allocation.
/// 
/// # Errors
/// Returns `CodegenError::TypeError` if the type is invalid
pub fn lang_type_to_llvm<'ctx>(
    context: &'ctx Context,
    lang_type: &LangType,
) -> Result<BasicTypeEnum<'ctx>, CodegenError> {

    // If it's a pointer, then we just have to make that, LLVM does not differentiate (anymore)
    if lang_type.pointer_depth > 0 {
        return Ok(context.ptr_type(AddressSpace::default()).into());
    }

    // Array types are represented as pointers (they decay to pointers)
    if lang_type.array_size.is_some() {
        return Ok(context.ptr_type(AddressSpace::default()).into());
    }

    // Get the base type
    Ok(match lang_type.base {
        TypeBase::SInt => match lang_type.size_bits {
            8 => context.i8_type().into(),
            16 => context.i16_type().into(),
            32 => context.i32_type().into(),
            64 => context.i64_type().into(),
            _ => {
                return Err(CodegenError::TypeError(
                    format!("Invalid signed integer size: {}", lang_type.size_bits),
                    crate::lexer::Position::new(0, 0),
                ))
            }
        },
        TypeBase::UInt => match lang_type.size_bits {
            8 => context.i8_type().into(),
            16 => context.i16_type().into(),
            32 => context.i32_type().into(),
            64 => context.i64_type().into(),
            _ => {
                return Err(CodegenError::TypeError(
                    format!("Invalid unsigned integer size: {}", lang_type.size_bits),
                    crate::lexer::Position::new(0, 0),
                ))
            }
        },
        TypeBase::SFloat => match lang_type.size_bits {
            32 => context.f32_type().into(),
            64 => context.f64_type().into(),
            _ => {
                return Err(CodegenError::TypeError(
                    format!("Invalid float size: {}", lang_type.size_bits),
                    crate::lexer::Position::new(0, 0),
                ))
            }
        },
        TypeBase::Void => {
            // Void can't be a basic type directly, but we handle it specially
            // For now, return i8 and the caller should check for void
            return Err(CodegenError::TypeError(
                "Void type cannot be used as a value type".to_string(),
                crate::lexer::Position::new(0, 0),
            ));
        }
    })
}

/// Check if a type is void
#[must_use]
pub fn is_void_type(lang_type: &LangType) -> bool {
    matches!(lang_type.base, TypeBase::Void) && lang_type.pointer_depth == 0
}

/// Get LLVM integer type for a given bit width
/// # Errors
/// Returns `CodegenError::TypeError` if the bit width is invalid
pub fn get_int_type(context: &'_ Context, bits: u32) -> Result<IntType<'_>, CodegenError> {
    match bits {
        8 => Ok(context.i8_type()),
        16 => Ok(context.i16_type()),
        32 => Ok(context.i32_type()),
        64 => Ok(context.i64_type()),
        _ => Err(CodegenError::TypeError(
            format!("Invalid integer size: {bits}"),
            crate::lexer::Position::new(0, 0),
        )),
    }
}

/// Get LLVM float type for a given bit width
/// # Errors
/// Returns `CodegenError::TypeError` if the bit width is invalid
pub fn get_float_type(context: &'_ Context, bits: u32) -> Result<FloatType<'_>, CodegenError> {
    match bits {
        32 => Ok(context.f32_type()),
        64 => Ok(context.f64_type()),
        _ => Err(CodegenError::TypeError(
            format!("Invalid float size: {bits}"),
            crate::lexer::Position::new(0, 0),
        )),
    }
}

/// Get the element type for a `LangType`
/// 
/// This returns the base type without array or pointer modifiers.
/// For `u32[4]`, this returns `i32`. For `u32*`, this returns `i32`.
/// For `u32`, this returns `i32`.
/// 
/// # Errors
/// Returns `CodegenError::TypeError` if the type is invalid
pub fn lang_type_element_to_llvm<'ctx>(
    context: &'ctx Context,
    lang_type: &LangType,
) -> Result<BasicTypeEnum<'ctx>, CodegenError> {
    // Get the base type without pointer/array modifiers
    Ok(match lang_type.base {
        TypeBase::SInt | TypeBase::UInt => match lang_type.size_bits {
            8 => context.i8_type().into(),
            16 => context.i16_type().into(),
            32 => context.i32_type().into(),
            64 => context.i64_type().into(),
            _ => {
                return Err(CodegenError::TypeError(
                    format!("Invalid integer size: {}", lang_type.size_bits),
                    crate::lexer::Position::new(0, 0),
                ))
            }
        },
        TypeBase::SFloat => match lang_type.size_bits {
            32 => context.f32_type().into(),
            64 => context.f64_type().into(),
            _ => {
                return Err(CodegenError::TypeError(
                    format!("Invalid float size: {}", lang_type.size_bits),
                    crate::lexer::Position::new(0, 0),
                ))
            }
        },
        TypeBase::Void => {
            return Err(CodegenError::TypeError(
                "Void type cannot be used as a value type".to_string(),
                crate::lexer::Position::new(0, 0),
            ));
        }
    })
}

/// Get the LLVM array type for a preallocated array
/// 
/// # Errors
/// Returns `CodegenError::TypeError` if the type is not an array or is invalid
pub fn lang_type_to_llvm_array<'ctx>(
    context: &'ctx Context,
    lang_type: &LangType,
) -> Result<ArrayType<'ctx>, CodegenError> {
    let array_size = lang_type.array_size.ok_or_else(|| {
        CodegenError::TypeError(
            "Expected array type".to_string(),
            crate::lexer::Position::new(0, 0),
        )
    })?;

    let element_type = lang_type_element_to_llvm(context, lang_type)?;
    Ok(element_type.array_type(array_size))
}
