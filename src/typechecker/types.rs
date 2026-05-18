use crate::lexer::{LangType, TypeBase};

/// Check if `from` can be implicitly coerced to `to` for non-literal expressions.
///
/// Rules:
/// - Exact match -> compatible
/// - Array-to-pointer decay -> compatible
/// - Void is only compatible with void
/// - Pointer depth must match (after decay)
/// - Integer family (SInt/UInt): widening or equal width only
/// - Float family: widening or equal width only
/// - Integer ↔ Float: NOT compatible (requires explicit cast)
#[must_use]
pub fn types_coercible(from: &LangType, to: &LangType) -> bool {
    // Exact match (ignoring const)
    if from == to {
        return true;
    }

    // Array-to-pointer decay
    let decayed_from = if from.is_array() {
        from.decay_to_pointer()
    } else {
        *from
    };
    let decayed_to = if to.is_array() {
        to.decay_to_pointer()
    } else {
        *to
    };
    if decayed_from == *to || decayed_to == *from || decayed_from == decayed_to {
        return true;
    }

    // Void is only compatible with void
    if from.base == TypeBase::Void || to.base == TypeBase::Void {
        return from.base == TypeBase::Void && to.base == TypeBase::Void;
    }

    // Pointer depth must match after decay
    if decayed_from.pointer_depth != decayed_to.pointer_depth {
        return false;
    }

    // Pointer-to-pointer with matching depth: always compatible
    if decayed_from.pointer_depth > 0 {
        return true;
    }

    // Non-pointer numeric types: widening (or equal) within same family only
    match (&decayed_from.base, &decayed_to.base) {
        (TypeBase::SInt | TypeBase::UInt, TypeBase::SInt | TypeBase::UInt) => {
            decayed_from.size_bits <= decayed_to.size_bits
        }
        (TypeBase::SFloat, TypeBase::SFloat) => decayed_from.size_bits <= decayed_to.size_bits,
        _ => false,
    }
}

/// Check if an integer literal value `val` can be used as type `to`.
///
/// Returns `true` when `val` fits in the value range of `to`.
#[must_use]
pub fn literal_int_fits(val: i64, to: &LangType) -> bool {
    if to.pointer_depth > 0 || to.is_array() {
        return false;
    }
    match to.base {
        TypeBase::SInt => {
            if to.size_bits >= 64 {
                true
            } else {
                let min = -(1i64 << (to.size_bits - 1));
                let max = (1i64 << (to.size_bits - 1)) - 1;
                val >= min && val <= max
            }
        }
        TypeBase::UInt => {
            if val < 0 {
                return false;
            }
            if to.size_bits >= 64 {
                true
            } else {
                (val as u64) < (1u64 << to.size_bits)
            }
        }
        _ => false,
    }
}

/// Check if a float literal is compatible with `to`.
///
/// Float literals are accepted by any float type; precision loss is permitted.
#[must_use]
pub fn literal_float_compatible(to: &LangType) -> bool {
    matches!(to.base, TypeBase::SFloat) && to.pointer_depth == 0 && !to.is_array()
}

/// Check if an explicit `as` cast from `from` to `to` is valid.
#[must_use]
pub fn cast_valid(from: &LangType, to: &LangType) -> bool {
    if from.pointer_depth > 0 || to.pointer_depth > 0 {
        // ptr ↔ integer: valid when the integer side is SInt or UInt
        if (from.pointer_depth > 0 && to.pointer_depth == 0)
            || (from.pointer_depth == 0 && to.pointer_depth > 0)
        {
            return matches!(to.base, TypeBase::SInt | TypeBase::UInt)
                || matches!(from.base, TypeBase::SInt | TypeBase::UInt);
        }
        // ptr -> ptr: always valid
        return true;
    }
    // All numeric-to-numeric casts are valid
    true
}
