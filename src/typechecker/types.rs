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

    // Void *values* are only compatible with void (function-return contexts).
    let from_void_value = from.base == TypeBase::Void && decayed_from.pointer_depth == 0;
    let to_void_value = to.base == TypeBase::Void && decayed_to.pointer_depth == 0;
    if from_void_value || to_void_value {
        return from_void_value && to_void_value;
    }

    // `u0*` (exactly depth 1) is the universal object pointer, but it bridges
    // implicitly only at **depth 1**, in both directions: `T* -> u0*` (type
    // erasure) and `u0* -> T*` (the `malloc` idiom). `T**` and deeper no longer
    // bridge — they need an explicit `as` cast (Proposal C rule 2). This closes
    // the unbounded-depth `u0*` hole while keeping the stdlib's depth-1 idiom.
    let from_opaque = from.base == TypeBase::Void && decayed_from.pointer_depth == 1;
    let to_opaque = to.base == TypeBase::Void && decayed_to.pointer_depth == 1;
    if (to_opaque && decayed_from.pointer_depth == 1)
        || (from_opaque && decayed_to.pointer_depth == 1)
    {
        return true;
    }

    // Pointer depth must match after decay
    if decayed_from.pointer_depth != decayed_to.pointer_depth {
        return false;
    }

    // Pointer-to-pointer of matching depth is no longer a free pass. The pointee
    // type must match exactly (rule 1 — a different pointee, including a
    // different signedness like `i32* -> u32*`, needs an explicit `as` cast),
    // and const may be *added* implicitly but never *removed* (rule 4 —
    // dropping const needs a cast). Pointer *comparisons* keep the old
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
        // A `bool` accepts only the literals `0` and `1`.
        TypeBase::Bool => val == 0 || val == 1,
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
    // Type-struct *values* are aggregates and cannot be bit-reinterpreted by a
    // cast; only the identical struct type "casts" to itself. Pointer-to-struct
    // casts (e.g. `Point* as u64`) fall through to the pointer rules below.
    if (matches!(from.base, TypeBase::Struct(_)) && from.pointer_depth == 0)
        || (matches!(to.base, TypeBase::Struct(_)) && to.pointer_depth == 0)
    {
        return from == to;
    }
    // Function pointers are pointer-shaped values. Allow casts to/from any
    // other pointer-like type or an integer (so `0 as fn(...) -> R` builds a
    // null function pointer, and integer ↔ FnPtr round-trips work).
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
