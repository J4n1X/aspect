//! Unified, cross-phase module symbol table.
//!
//! [`ModuleSymbols`] is the single authoritative table of a program's global
//! symbols. Built by the parser and shared by handle off
//! [`crate::parser::Program`], so the checker and codegen read the *same* table.
//! Type *ids* are interned once at parse time and codegen's GEP field indices
//! must agree with them, so the registry cannot be rebuilt per phase.
//!
//! Every named type — type-struct, enum, sum, alias — is one [`TypeDef`] in one
//! id space, with only the kind-specific part in [`TypeKind`]. That is what
//! makes "is this name taken?", the visibility gate and the prescan single
//! pieces of code: a new kind adds a `TypeKind` variant, not a parallel
//! registry.
//!
//! The parser's per-function *variable* scope is separate and transient — it
//! lives in [`crate::symbol::table::SymbolTable`], not here.

use crate::lexer::{LangType, Position};
use crate::symbol::ids::{EnumId, SigId, StructId, SumId, TypeDefId};
use crate::symbol::table::{FunctionSymbol, SymbolError};
use std::collections::HashMap;
use std::ops::Index;

/// Build the mangled free-function name a type-struct method lowers to:
/// `Type$method`. The single authority for the mangling scheme — see also
/// [`method_owner_prefix`] for the reverse test.
#[must_use]
pub fn mangle_method(type_name: &str, method_name: &str) -> String {
    format!("{type_name}${method_name}")
}

/// The mangled-name prefix shared by every method of `type_name`
/// (`"Type$"`). A function whose name starts with this prefix is a method of
/// that type — the inverse of [`mangle_method`].
#[must_use]
pub fn method_owner_prefix(type_name: &str) -> String {
    format!("{type_name}$")
}

/// Field visibility. The default for a type-struct field is [`Visibility::Private`];
/// `public` opts a field into external access.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Visibility {
    Public,
    Private,
}

/// A single field of a type-struct, in declaration (and layout) order.
#[derive(Debug, Clone, PartialEq)]
pub struct FieldInfo {
    pub name: String,
    pub ty: LangType,
    pub vis: Visibility,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MethodSig {
    /// The mangled free-function name this method lowers to, e.g. `"Type$method"`.
    pub mangled_name: String,
    /// Declared parameters, *excluding* the implicit `this` receiver.
    pub params: Vec<(LangType, String)>,
    pub return_type: LangType,
    /// `true` when the method has no `this` receiver (a "static" function).
    pub is_static: bool,
    /// `true` for `const fn` (receiver lowered to `*const Struct`).
    pub is_const: bool,
    /// Method visibility. Like fields, methods default to [`Visibility::Private`];
    /// `public fn` opts a method into external access.
    pub vis: Visibility,
}

/// One variant of a `sum` type: its name and payload fields in declaration
/// (and binding) order. A payload-less variant has an empty `fields`.
#[derive(Debug, Clone, PartialEq)]
pub struct SumVariant {
    pub name: String,
    /// `(name, type)` pairs. Names are required at declaration for
    /// diagnostics/documentation; matching is positional.
    pub fields: Vec<(String, LangType)>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct StructBody {
    pub fields: Vec<FieldInfo>,
    /// Field name -> index into `fields` (mirrors the LLVM struct element order).
    pub field_index: HashMap<String, usize>,
    pub methods: HashMap<String, MethodSig>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct EnumBody {
    /// Variant names in declaration order; the index *is* the variant's value.
    pub variants: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SumBody {
    /// Variants in declaration order; the index *is* the discriminant value.
    /// There is no per-variant visibility: exhaustive matching needs all
    /// variants or none.
    pub variants: Vec<SumVariant>,
}

/// The kind-specific half of a named type. The shared header lives on
/// [`TypeDef`], so code that treats all named types alike never matches here.
#[derive(Debug, Clone, PartialEq)]
pub enum TypeKind {
    Struct(StructBody),
    Enum(EnumBody),
    Sum(SumBody),
    Alias(LangType),
}

impl TypeKind {
    /// The single spelling authority, so the visibility gate and every
    /// duplicate/private diagnostic agree on what to call a kind.
    #[must_use]
    pub fn noun(&self) -> &'static str {
        match self {
            TypeKind::Struct(_) => "type-struct",
            TypeKind::Enum(_) => "enum",
            TypeKind::Sum(_) => "sum",
            TypeKind::Alias(_) => "type alias",
        }
    }

    #[must_use]
    pub fn keyword(&self) -> &'static str {
        match self {
            TypeKind::Struct(_) => "type",
            TypeKind::Enum(_) => "enum",
            TypeKind::Sum(_) => "sum",
            TypeKind::Alias(_) => "alias",
        }
    }
}

/// One named type. `id` indexes the registry and is what
/// `TypeBase::{Struct,Enum,Sum}` carries; sharing that id space with one name
/// map is what makes interning a name as two kinds at once impossible.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeDef {
    pub id: TypeDefId,
    pub name: String,
    /// `Position::file_id` of the declaring file — the provenance the
    /// import-visibility check resolves to a defining module.
    pub file_id: u32,
    /// `public` makes the type nameable from other modules. Fixed at intern
    /// (prescan) time, since under import cycles a module's uses can precede
    /// the definition in the inlined token stream.
    pub vis: Visibility,
    /// The declaring keyword's position, recorded at intern time so diagnostics
    /// firing long after the body parsed — the containment-cycle check, codegen
    /// layout failures — can still name the declaration site.
    pub pos: Position,
    /// `false` between the prescan reserving the name and the body being parsed;
    /// a second body for an already-`defined` name is the duplicate-type error.
    /// Deliberately not inferred from an empty body, so whether an empty
    /// `type`/`enum`/`sum` is legal stays a language question.
    pub defined: bool,
    pub kind: TypeKind,
}

impl TypeDef {
    /// # Panics
    /// If this def is not a type-struct. Ids reach the `as_*` accessors either
    /// from a `TypeBase` variant or a kind-filtered lookup, both of which
    /// already establish the kind.
    #[must_use]
    fn struct_body(&self) -> &StructBody {
        match &self.kind {
            TypeKind::Struct(body) => body,
            other => unreachable!("type '{}' is a {}, not a type-struct", self.name, other.noun()),
        }
    }

    #[must_use]
    fn enum_body(&self) -> &EnumBody {
        match &self.kind {
            TypeKind::Enum(body) => body,
            other => unreachable!("type '{}' is a {}, not an enum", self.name, other.noun()),
        }
    }

    #[must_use]
    fn sum_body(&self) -> &SumBody {
        match &self.kind {
            TypeKind::Sum(body) => body,
            other => unreachable!("type '{}' is a {}, not a sum", self.name, other.noun()),
        }
    }

    #[must_use]
    pub fn alias_target(&self) -> Option<LangType> {
        match &self.kind {
            TypeKind::Alias(ty) => Some(*ty),
            _ => None,
        }
    }

    #[must_use]
    pub fn noun(&self) -> &'static str {
        self.kind.noun()
    }
}

/// A distinct function-pointer signature (`fn(params) -> return_type`).
/// Two FnPtr ids are equal iff their `FnPtrSig`s compare equal.
///
/// Not a [`TypeDef`]: a fn-pointer type is structural and unnamed, so it has no
/// name to collide, no module to belong to and no visibility to gate.
#[derive(Debug, Clone, PartialEq)]
pub struct FnPtrSig {
    pub params: Vec<LangType>,
    pub return_type: LangType,
}

/// The program-wide table of resolved global symbols.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModuleSymbols {
    /// Functions by name (the de-duplicated signature store).
    functions: HashMap<String, FunctionSymbol>,
    /// Every named type, indexed by id (index into the vec == the id).
    types: Vec<TypeDef>,
    /// The one type namespace, so a collision between any two kinds is a single
    /// lookup instead of a check per kind.
    types_by_name: HashMap<String, TypeDefId>,
    /// Function-pointer signatures, interned by structural identity.
    /// Index into the vec == the FnPtr id stored in `TypeBase::FnPtr(u32)`.
    fnptr_sigs: Vec<FnPtrSig>,
}

impl ModuleSymbols {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // ── Functions ─────────────────────────────────────────────────────────────

    /// Prove `id` names a type-struct, yielding the id that indexes its body
    /// infallibly. `None` is a real diagnostic ("`Foo` is a sum, not a
    /// type-struct"), not an internal error.
    #[must_use]
    pub fn as_struct(&self, id: TypeDefId) -> Option<StructId> {
        matches!(self.types.get(id.raw() as usize)?.kind, TypeKind::Struct(_)).then_some(StructId(id))
    }

    /// See [`ModuleSymbols::as_struct`].
    #[must_use]
    pub fn as_sum(&self, id: TypeDefId) -> Option<SumId> {
        matches!(self.types.get(id.raw() as usize)?.kind, TypeKind::Sum(_)).then_some(SumId(id))
    }

    /// See [`ModuleSymbols::as_struct`].
    #[must_use]
    pub fn as_enum(&self, id: TypeDefId) -> Option<EnumId> {
        matches!(self.types.get(id.raw() as usize)?.kind, TypeKind::Enum(_)).then_some(EnumId(id))
    }



    /// A declaration may be followed by a matching definition, but two bodies —
    /// or a definition disagreeing with an earlier declaration — are errors.
    ///
    /// # Errors
    /// [`SymbolError::FunctionAlreadyDefined`] if two definitions supply a body,
    /// or [`SymbolError::SignatureMismatch`] if a definition disagrees with an
    /// earlier declaration.
    pub fn add_function(&mut self, func: FunctionSymbol) -> Result<(), SymbolError> {
        match self.functions.get(&func.name) {
            Some(existing) if existing.has_body && func.has_body => {
                return Err(SymbolError::FunctionAlreadyDefined(func.name));
            }
            Some(existing)
                if !existing.has_body
                    && func.has_body
                    && (existing.params != func.params
                        || existing.return_type != func.return_type) =>
            {
                return Err(SymbolError::SignatureMismatch(func.name));
            }
            _ => {}
        }
        self.functions.insert(func.name.clone(), func);
        Ok(())
    }

    #[must_use]
    pub fn lookup_function(&self, name: &str) -> Option<&FunctionSymbol> {
        self.functions.get(name)
    }

    pub fn functions(&self) -> impl Iterator<Item = (&String, &FunctionSymbol)> {
        self.functions.iter()
    }

    // ── Named types ──────────────────────────────────────────────────────────

    /// Reserve an id during the parser's prescan, so names resolve regardless of
    /// declaration order (self/mutual reference included). `kind` normally
    /// arrives with an empty body, filled in later by
    /// `set_fields`/`set_*_variants`.
    ///
    /// An already-interned name keeps its first id, kind, `vis` and `pos`, so a
    /// redeclaration under a different kind is caught at the body parse — where
    /// the diagnostic belongs.
    pub fn intern_type(
        &mut self,
        name: &str,
        file_id: u32,
        vis: Visibility,
        pos: Position,
        kind: TypeKind,
    ) -> TypeDefId {
        if let Some(&id) = self.types_by_name.get(name) {
            return id;
        }
        let id = TypeDefId::from_raw(
            u32::try_from(self.types.len()).expect("number of named types exceeds u32::MAX"),
        );
        self.types.push(TypeDef {
            id,
            name: name.to_string(),
            file_id,
            vis,
            pos,
            defined: false,
            kind,
        });
        self.types_by_name.insert(name.to_string(), id);
        id
    }

    /// The id of any named type, whatever its kind — the single "is this name
    /// taken?" query.
    #[must_use]
    pub fn type_id(&self, name: &str) -> Option<TypeDefId> {
        self.types_by_name.get(name).copied()
    }

    #[must_use]
    pub fn lookup_type(&self, name: &str) -> Option<&TypeDef> {
        self.types_by_name
            .get(name)
            .map(|&id| &self.types[id.raw() as usize])
    }

    /// # Panics
    /// If `id` was never interned. Ids reach callers only from a `LangType` or a
    /// lookup, both of which come from this table.
    #[must_use]
    pub fn type_def(&self, id: impl Into<TypeDefId>) -> &TypeDef {
        &self.types[id.into().raw() as usize]
    }

    pub fn types(&self) -> impl Iterator<Item = &TypeDef> {
        self.types.iter()
    }

    fn id_of_kind(&self, name: &str, want: fn(&TypeKind) -> bool) -> Option<TypeDefId> {
        let def = self.lookup_type(name)?;
        want(&def.kind).then_some(def.id)
    }

    /// The `LangType` that spells this declaration: the nominal type for a
    /// struct/enum/sum, or the target an alias stands for. Total by
    /// construction — minting the proof id here is what keeps callers from
    /// needing an unwrap to name a type they just looked up.
    #[must_use]
    pub fn named_type(&self, id: impl Into<TypeDefId>) -> LangType {
        let id = id.into();
        match &self.types[id.raw() as usize].kind {
            TypeKind::Struct(_) => LangType::struct_type(StructId(id)),
            TypeKind::Enum(_) => LangType::enum_type(EnumId(id)),
            TypeKind::Sum(_) => LangType::sum_type(SumId(id)),
            TypeKind::Alias(target) => *target,
        }
    }

    /// Kind-filtered name lookup — one of the two places a [`StructId`] is
    /// minted (the other is [`Self::as_struct`]).
    #[must_use]
    pub fn struct_id(&self, name: &str) -> Option<StructId> {
        self.id_of_kind(name, |k| matches!(k, TypeKind::Struct(_)))
            .map(StructId)
    }

    #[must_use]
    pub fn enum_id(&self, name: &str) -> Option<EnumId> {
        self.id_of_kind(name, |k| matches!(k, TypeKind::Enum(_)))
            .map(EnumId)
    }

    #[must_use]
    pub fn sum_id(&self, name: &str) -> Option<SumId> {
        self.id_of_kind(name, |k| matches!(k, TypeKind::Sum(_)))
            .map(SumId)
    }

    /// In id order — codegen's registration passes depend on it. The filter
    /// establishes the kind, so each item carries its proof id.
    pub fn structs(&self) -> impl Iterator<Item = (StructId, &TypeDef)> {
        self.types
            .iter()
            .filter(|d| matches!(d.kind, TypeKind::Struct(_)))
            .map(|d| (StructId(d.id), d))
    }

    pub fn enums(&self) -> impl Iterator<Item = (EnumId, &TypeDef)> {
        self.types
            .iter()
            .filter(|d| matches!(d.kind, TypeKind::Enum(_)))
            .map(|d| (EnumId(d.id), d))
    }

    pub fn sums(&self) -> impl Iterator<Item = (SumId, &TypeDef)> {
        self.types
            .iter()
            .filter(|d| matches!(d.kind, TypeKind::Sum(_)))
            .map(|d| (SumId(d.id), d))
    }

    /// Also marks the declaration defined.
    pub fn set_fields(&mut self, id: TypeDefId, fields: Vec<FieldInfo>) {
        let field_index = fields
            .iter()
            .enumerate()
            .map(|(i, f)| (f.name.clone(), i))
            .collect();
        let def = &mut self.types[id.raw() as usize];
        def.defined = true;
        match &mut def.kind {
            TypeKind::Struct(body) => {
                body.fields = fields;
                body.field_index = field_index;
            }
            other => unreachable!("set_fields on a {}", other.noun()),
        }
    }

    pub fn set_enum_variants(&mut self, id: TypeDefId, variants: Vec<String>) {
        let def = &mut self.types[id.raw() as usize];
        def.defined = true;
        match &mut def.kind {
            TypeKind::Enum(body) => body.variants = variants,
            other => unreachable!("set_enum_variants on a {}", other.noun()),
        }
    }

    pub fn set_sum_variants(&mut self, id: TypeDefId, variants: Vec<SumVariant>) {
        let def = &mut self.types[id.raw() as usize];
        def.defined = true;
        match &mut def.kind {
            TypeKind::Sum(body) => body.variants = variants,
            other => unreachable!("set_sum_variants on a {}", other.noun()),
        }
    }

    pub fn add_method(&mut self, id: TypeDefId, name: String, sig: MethodSig) {
        match &mut self.types[id.raw() as usize].kind {
            TypeKind::Struct(body) => {
                body.methods.insert(name, sig);
            }
            other => unreachable!("add_method on a {}", other.noun()),
        }
    }

    #[must_use]
    pub fn field(&self, id: StructId, name: &str) -> Option<(usize, &FieldInfo)> {
        let body = &self[id];
        let idx = *body.field_index.get(name)?;
        Some((idx, &body.fields[idx]))
    }

    #[must_use]
    pub fn enum_variant_index(&self, id: EnumId, variant: &str) -> Option<usize> {
        self[id].variants.iter().position(|v| v == variant)
    }

    #[must_use]
    pub fn sum_variant_index(&self, id: SumId, variant: &str) -> Option<usize> {
        self[id].variants.iter().position(|v| v.name == variant)
    }

    /// `alias New Target`. Interned like any other named type — same id space,
    /// same visibility gate — and `defined` immediately, its target having
    /// resolved eagerly.
    pub fn define_alias(
        &mut self,
        name: &str,
        ty: LangType,
        file_id: u32,
        pos: Position,
    ) -> TypeDefId {
        let id = self.intern_type(name, file_id, Visibility::Private, pos, TypeKind::Alias(ty));
        self.types[id.raw() as usize].defined = true;
        id
    }

    #[must_use]
    pub fn resolve_alias(&self, name: &str) -> Option<LangType> {
        self.lookup_type(name)?.alias_target()
    }

    // ── Function-pointer signatures ──────────────────────────────────────────

    /// Intern a function-pointer signature, returning a stable id. Identical
    /// signatures return the same id (structural deduplication), so two FnPtr
    /// types are compared by id alone — `LangType` stays `Copy`/`Eq`.
    pub fn intern_fnptr(&mut self, params: Vec<LangType>, return_type: LangType) -> SigId {
        let sig = FnPtrSig {
            params,
            return_type,
        };
        if let Some(idx) = self.fnptr_sigs.iter().position(|s| *s == sig) {
            return SigId::from_raw(u32::try_from(idx).expect("fnptr signature index overflows u32"));
        }
        let id = SigId::from_raw(
            u32::try_from(self.fnptr_sigs.len())
                .expect("number of fn-ptr signatures exceeds u32::MAX"),
        );
        self.fnptr_sigs.push(sig);
        id
    }

    #[must_use]
    pub fn fnptr_sig(&self, id: SigId) -> &FnPtrSig {
        &self.fnptr_sigs[id.raw() as usize]
    }

    /// All registered FnPtr signatures, indexed by id. Unlike
    /// [`Self::fnptr_sig`], probing an out-of-range id here does not panic.
    #[must_use]
    pub fn all_fnptr_sigs(&self) -> &[FnPtrSig] {
        &self.fnptr_sigs
    }
}

fn as_module_index(id: impl Into<TypeDefId>) -> usize {
    id.into().raw() as usize
}


impl Index<StructId> for ModuleSymbols {
    type Output = StructBody;

    fn index(&self, index: StructId) -> &Self::Output {
        self.types[as_module_index(index)].struct_body()
    }
}

impl Index<SumId> for ModuleSymbols {
    type Output = SumBody;

    fn index(&self, index: SumId) -> &Self::Output {
        self.types[as_module_index(index)].sum_body()
    }
}

impl Index<EnumId> for ModuleSymbols {
    type Output = EnumBody;

    fn index(&self, index: EnumId) -> &Self::Output {
        self.types[as_module_index(index)].enum_body()
    }
}