use crate::lexer::{LangType, TypeBase};

/// Implicit coercion for non-literal expressions: exact match or array decay;
/// void only with void; matching pointer depth after decay; widening-only
/// within the integer or float family; integer ↔ float needs an explicit cast.
#[must_use]
pub fn types_coercible(from: &LangType, to: &LangType) -> bool {
    // A poisoned operand coerces to anything, suppressing secondary mismatches
    // downstream of a stuck demand site.
    if from.base == TypeBase::Unresolved || to.base == TypeBase::Unresolved {
        return true;
    }

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

    // Void *values* are only compatible with void (function-return contexts).
    let from_void_value = from.base == TypeBase::Void && decayed_from.pointer_depth == 0;
    let to_void_value = to.base == TypeBase::Void && decayed_to.pointer_depth == 0;
    if from_void_value || to_void_value {
        return from_void_value && to_void_value;
    }

    // `u0*` is the universal object pointer, but bridges implicitly only at
    // **depth 1**, both directions: `T* -> u0*` (erasure) and `u0* -> T*` (the
    // `malloc` idiom). `T**` and deeper need an explicit `as` cast.
    let from_opaque = from.base == TypeBase::Void && decayed_from.pointer_depth == 1;
    let to_opaque = to.base == TypeBase::Void && decayed_to.pointer_depth == 1;
    if (to_opaque && decayed_from.pointer_depth == 1)
        || (from_opaque && decayed_to.pointer_depth == 1)
    {
        return true;
    }

    if decayed_from.pointer_depth != decayed_to.pointer_depth {
        return false;
    }

    // Pointer-to-pointer of matching depth: the pointee must match exactly (a
    // different signedness like `i32* -> u32*` needs an `as` cast), and const
    // may be added implicitly but never removed. Pointer *comparisons* keep the
    // permissive rule via `comparison_operands_valid`, not this path.
    if decayed_from.pointer_depth > 0 {
        if decayed_from.base != decayed_to.base
            || decayed_from.size_bits != decayed_to.size_bits
        {
            return false;
        }
        return !(decayed_from.is_const && !decayed_to.is_const);
    }

    // Non-pointer numeric types: widening (or equal) within same family only
    match (&decayed_from.base, &decayed_to.base) {
        (TypeBase::SInt | TypeBase::UInt, TypeBase::SInt | TypeBase::UInt) => {
            decayed_from.size_bits <= decayed_to.size_bits
        }
        (TypeBase::SFloat, TypeBase::SFloat) => decayed_from.size_bits <= decayed_to.size_bits,
        // `bool` is a 0/1 value: it coerces to itself and widens into any
        // integer. Integers do NOT implicitly coerce to `bool` (that needs a
        // `!= 0` test, not a width cast), so the reverse direction is absent.
        (TypeBase::Bool, TypeBase::Bool | TypeBase::SInt | TypeBase::UInt) => true,
        // Nominal identity: an enum coerces only to the *same* enum (this arm
        // also lets a `const Enum` satisfy a non-const `Enum` target). Enum ↔
        // integer needs an `as` cast.
        (TypeBase::Enum(a), TypeBase::Enum(b)) => a == b,
        _ => false,
    }
}

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
        TypeBase::Bool => val == 0 || val == 1,
        _ => false,
    }
}

/// Any float type accepts a float literal; precision loss is permitted.
#[must_use]
pub fn literal_float_compatible(to: &LangType) -> bool {
    matches!(to.base, TypeBase::SFloat) && to.pointer_depth == 0 && !to.is_array()
}

#[must_use]
pub fn cast_valid(from: &LangType, to: &LangType) -> bool {
    // Type-struct *values* are aggregates: only the identical struct type
    // "casts" to itself. Pointer-to-struct casts fall through to the pointer
    // rules below.
    if (matches!(from.base, TypeBase::Struct(_)) && from.pointer_depth == 0)
        || (matches!(to.base, TypeBase::Struct(_)) && to.pointer_depth == 0)
    {
        return from == to;
    }
    // Enums share the `i32` repr, so they cast to/from integers and other enums
    // but never directly to/from a float or pointer (go through an integer).
    // `int as Enum` performs no range check (C-like).
    let from_is_enum = matches!(from.base, TypeBase::Enum(_)) && from.pointer_depth == 0;
    let to_is_enum = matches!(to.base, TypeBase::Enum(_)) && to.pointer_depth == 0;
    if from_is_enum || to_is_enum {
        let other = if from_is_enum { to } else { from };
        return matches!(other.base, TypeBase::Enum(_))
            || (matches!(other.base, TypeBase::SInt | TypeBase::UInt) && other.pointer_depth == 0);
    }

    // Function pointers are pointer-shaped: cast to/from any pointer-like type
    // or an integer (so `0 as fn(...) -> R` builds a null function pointer).
    let from_is_fnptr = matches!(from.base, TypeBase::FnPtr(_)) && from.pointer_depth == 0;
    let to_is_fnptr = matches!(to.base, TypeBase::FnPtr(_)) && to.pointer_depth == 0;
    if from_is_fnptr || to_is_fnptr {
        let other = if from_is_fnptr { to } else { from };
        return matches!(other.base, TypeBase::FnPtr(_))
            || other.pointer_depth > 0
            || (matches!(other.base, TypeBase::SInt | TypeBase::UInt) && other.pointer_depth == 0);
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::LangType;

    /// An enum coerces to the same enum only — never to a different enum or to
    /// its underlying integer.
    #[test]
    fn enum_coerces_only_to_same_enum() {
        let color = LangType::enum_type(0);
        let color2 = LangType::enum_type(0);
        let dir = LangType::enum_type(1);
        assert!(types_coercible(&color, &color2));
        assert!(!types_coercible(&color, &dir));
        assert!(!types_coercible(&color, &LangType::I32));
        assert!(!types_coercible(&LangType::I32, &color));
    }

    /// An enum casts to/from an integer and to/from another enum (shared `i32`
    /// repr), but not to/from a float or a pointer.
    #[test]
    fn enum_casts_to_int_and_enum_only() {
        let color = LangType::enum_type(0);
        let dir = LangType::enum_type(1);
        assert!(cast_valid(&color, &LangType::I32));
        assert!(cast_valid(&LangType::I32, &color));
        assert!(cast_valid(&color, &dir));
        assert!(!cast_valid(&color, &LangType::F64));
        assert!(!cast_valid(&LangType::F64, &color));
        assert!(!cast_valid(&color, &LangType::U8_PTR));
    }
}
