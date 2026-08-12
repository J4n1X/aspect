//! Typed indices into the symbol registries.
//!
//! Three id spaces used to be spelled `u32` and were therefore
//! interchangeable: named-type declarations, function-pointer signatures, and
//! source files. Passing one where another belonged compiled cleanly and
//! resolved against the wrong table, surfacing as an `unreachable!` somewhere
//! unrelated. Each space now has its own type.
//!
//! On top of that, [`StructId`]/[`SumId`]/[`EnumId`] are *proof* ids: a value
//! of one exists only where the registry has already checked the declaration's
//! kind, so indexing a body with one cannot fail. Minting is confined to
//! `symbol/` — the inner field is `pub(super)`, not public — which is why
//! there is no `from_raw` on them. `TypeDefId` and `SigId` are plain indices
//! and do carry `from_raw`.

use std::fmt;

/// Index into the module's type registry — a *declaration*, not a type. `i32`
/// and `u8*` are structural and never enter the registry, so they have no
/// `TypeDefId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TypeDefId(pub(super) u32);

impl TypeDefId {
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl fmt::Display for TypeDefId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Generates the kind-proof id newtypes: same shape, differing only in which
/// `TypeKind` the registry checked before handing one out.
macro_rules! proof_ids {
    ($($name:ident => $noun:literal),+ $(,)?) => {$(
        #[doc = concat!("A [`TypeDefId`] the registry has proven names a ", $noun, ".")]
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(pub(super) TypeDefId);

        impl $name {
            /// Widen back to the declaration id — for the shared `TypeDef`
            /// metadata (name, position, visibility) that is not kind-specific.
            #[must_use]
            pub const fn def(self) -> TypeDefId {
                self.0
            }
        }

        impl From<$name> for TypeDefId {
            fn from(id: $name) -> Self {
                id.0
            }
        }

        /// Unchecked mint for unit tests that exercise pure `LangType`
        /// behaviour and have no registry to prove anything against.
        #[cfg(test)]
        impl $name {
            #[allow(dead_code)]
            pub(crate) const fn for_test(raw: u32) -> Self {
                Self(TypeDefId(raw))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
                write!(f, "{}", self.0.raw())
            }
        }
    )+};
}

proof_ids! {
    StructId => "type-struct",
    SumId => "sum",
    EnumId => "enum",
}

/// Index into the module's function-pointer signature registry. A *separate*
/// space from [`TypeDefId`], and deliberately not convertible to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SigId(pub(super) u32);

impl SigId {
    #[must_use]
    pub const fn from_raw(raw: u32) -> Self {
        Self(raw)
    }

    #[must_use]
    pub const fn raw(self) -> u32 {
        self.0
    }
}

impl fmt::Display for SigId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
