use crate::codegen::TypeLoweringError;
use crate::lexer::{LangType, TypeBase};
use crate::parser::ComparisonOp;
use inkwell::builder::{Builder, BuilderError};
use inkwell::context::Context;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{FloatValue, IntValue};
use inkwell::AddressSpace;
use inkwell::FloatPredicate;
use inkwell::IntPredicate;

// ─── LangTypeExt trait ───────────────────────────────────────────────────────

/// Extension methods on `LangType` for codegen use.
///
/// `LangType` lives in `src/lexer/`; this trait adds codegen-specific helpers
/// without modifying that crate.
pub trait LangTypeExt {
    fn is_void(&self) -> bool;

    /// Array types decay to `ptr` (same as pointers); for the backing array
    /// type use the code generator's `lang_type_to_llvm_array`. Returns a
    /// position-less [`TypeLoweringError`]; the caller attaches a position via
    /// `with_pos`.
    fn to_llvm<'ctx>(&self, ctx: &'ctx Context)
        -> Result<BasicTypeEnum<'ctx>, TypeLoweringError>;

    /// Return a `LangType` one pointer-depth less (the pointee type).
    fn pointee(&self) -> LangType;
}

impl LangTypeExt for LangType {
    fn is_void(&self) -> bool {
        matches!(self.base, TypeBase::Void) && self.pointer_depth == 0
    }

    fn to_llvm<'ctx>(&self, ctx: &'ctx Context) -> Result<BasicTypeEnum<'ctx>, TypeLoweringError> {
        if self.pointer_depth > 0 || self.array_size.is_some() {
            return Ok(ctx.ptr_type(AddressSpace::default()).into());
        }
        Ok(match self.base {
            // `bool` is stored as i8 (0 or 1); its register form is i1, produced
            // by comparisons and narrowed at load/condition sites.
            TypeBase::Bool => ctx.i8_type().into(),
            TypeBase::SInt | TypeBase::UInt => match self.size_bits {
                8 => ctx.i8_type().into(),
                16 => ctx.i16_type().into(),
                32 => ctx.i32_type().into(),
                64 => ctx.i64_type().into(),
                _ => {
                    return Err(TypeLoweringError(format!(
                        "Invalid integer size: {}",
                        self.size_bits
                    )))
                }
            },
            TypeBase::SFloat => match self.size_bits {
                32 => ctx.f32_type().into(),
                64 => ctx.f64_type().into(),
                _ => {
                    return Err(TypeLoweringError(format!(
                        "Invalid float size: {}",
                        self.size_bits
                    )))
                }
            },
            TypeBase::Void => {
                return Err(TypeLoweringError(
                    "Void type cannot be used as a value type".to_string(),
                ))
            }
            // Struct *values* need the cached named `StructType`, which this
            // trait can't reach — use `lang_type_to_llvm`. (Pointers decay above.)
            TypeBase::Struct(id) => {
                return Err(TypeLoweringError(format!(
                    "struct#{id} value must be lowered via lang_type_to_llvm"
                )))
            }
            // `fn(...) -> R` *is* a pointer — opaque `ptr` in LLVM. The
            // signature is needed only at call sites (resolved via the FnPtr id).
            TypeBase::FnPtr(_) => ctx.ptr_type(AddressSpace::default()).into(),
            // An enum's underlying representation is a 32-bit integer; the
            // nominal enum type carries no distinct LLVM shape.
            TypeBase::Enum(_) => ctx.i32_type().into(),
            // Never reaches codegen — an unresolved obligation is a fatal error.
            TypeBase::Unresolved => {
                unreachable!("unresolved type reached codegen — an unresolved obligation escaped the checker")
            }
        })
    }

    fn pointee(&self) -> LangType {
        LangType {
            base: self.base,
            size_bits: self.size_bits,
            pointer_depth: self.pointer_depth.saturating_sub(1),
            is_const: self.is_const,
            array_size: None,
        }
    }
}

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
    ($builder:expr_2021, $is_signed:expr_2021, $signed:ident, $unsigned:ident, $($arg:expr_2021),+) => {
        if $is_signed {
            $builder.$signed($($arg),+)
        } else {
            $builder.$unsigned($($arg),+)
        }
    };
}

// ─── Width-matching helpers ───────────────────────────────────────────────────

/// `sext` for signed values, `zext` for unsigned; matching widths pass through.
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
    let a_is_f64 = a.get_type() == context.f64_type();
    if a_is_f64 {
        let b_wide = builder.build_float_ext(b, a.get_type(), "fpwiden")?;
        Ok((a, b_wide))
    } else {
        let a_wide = builder.build_float_ext(a, b.get_type(), "fpwiden")?;
        Ok((a_wide, b))
    }
}

/// LLVM 19 removed most `LLVMConst*` functions, so this extracts the Rust value
/// with `get_{zero,sign}_extended_constant` and rebuilds the constant at the
/// wider type. Values with matching widths are returned unchanged.
pub fn const_widen_ints_to_match<'ctx>(
    a: IntValue<'ctx>,
    a_signed: bool,
    b: IntValue<'ctx>,
    b_signed: bool,
) -> (IntValue<'ctx>, IntValue<'ctx>) {
    let a_bits = a.get_type().get_bit_width();
    let b_bits = b.get_type().get_bit_width();
    if a_bits > b_bits {
        let raw = if b_signed {
            b.get_sign_extended_constant().unwrap_or(0) as u64
        } else {
            b.get_zero_extended_constant().unwrap_or(0)
        };
        let b_wide = a.get_type().const_int(raw, b_signed);
        (a, b_wide)
    } else if b_bits > a_bits {
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

/// Signed uses `S`-prefixed predicates, unsigned `U`-prefixed; `EQ`/`NE` are
/// signedness-independent.
#[must_use]
pub fn int_cmp_pred(op: &ComparisonOp, is_signed: bool) -> IntPredicate {
    match op {
        ComparisonOp::Equal => IntPredicate::EQ,
        ComparisonOp::NotEqual => IntPredicate::NE,
        ComparisonOp::Less => {
            if is_signed {
                IntPredicate::SLT
            } else {
                IntPredicate::ULT
            }
        }
        ComparisonOp::Greater => {
            if is_signed {
                IntPredicate::SGT
            } else {
                IntPredicate::UGT
            }
        }
        ComparisonOp::LessEqual => {
            if is_signed {
                IntPredicate::SLE
            } else {
                IntPredicate::ULE
            }
        }
        ComparisonOp::GreaterEqual => {
            if is_signed {
                IntPredicate::SGE
            } else {
                IntPredicate::UGE
            }
        }
    }
}

/// Return the IEEE-754-correct `FloatPredicate` for a comparison operation.
///
/// `<`, `>`, `<=`, `>=`, `==` use the *ordered* predicates: any comparison
/// involving a NaN returns false — matches C / IEEE semantics. `!=` is the
/// exception: it uses the *unordered* `UNE` predicate so that NaN inequality
/// (including `NaN != NaN`) is true, again matching C. Using `ONE` for `!=`
/// would silently break NaN-detection idioms like `if x != x { ... }`.
#[must_use]
pub fn float_cmp_pred(op: &ComparisonOp) -> FloatPredicate {
    match op {
        ComparisonOp::Equal => FloatPredicate::OEQ,
        ComparisonOp::NotEqual => FloatPredicate::UNE,
        ComparisonOp::Less => FloatPredicate::OLT,
        ComparisonOp::Greater => FloatPredicate::OGT,
        ComparisonOp::LessEqual => FloatPredicate::OLE,
        ComparisonOp::GreaterEqual => FloatPredicate::OGE,
    }
}
