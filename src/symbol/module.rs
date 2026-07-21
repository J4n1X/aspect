//! Unified, cross-phase module symbol table.
//!
//! [`ModuleSymbols`] is the single authoritative table of a program's global
//! symbols — functions, type-structs, enums, and aliases. It is built by the parser
//! and rides on [`crate::parser::Program`] (like `string_literals`), so the
//! type checker and code generator consume the *same* table rather than each
//! re-deriving its own (which is how function signatures used to be tripled).
//!
//! Struct *ids* are an interning decision fixed once at parse time; codegen's
//! GEP field indices must agree with them, so the registry genuinely cannot be
//! rebuilt per phase — hence the shared home here.
//!
//! The parser's per-function *variable* scope is a separate, transient concern
//! and lives in [`crate::symbol::table::SymbolTable`]; it is not part of this
//! table and is discarded after parsing.

use crate::lexer::LangType;
use crate::parser::ast::Attribute;
use crate::symbol::table::{FunctionSymbol, SymbolError};
use std::collections::HashMap;

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
    /// Leading attributes in source order (outside-in, leftmost applied last).
    pub attrs: Vec<Attribute>,
}

/// The signature of a type-struct method (populated in Milestone 3).
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

/// A registered type-struct: its name, ordered fields, and methods.
#[derive(Debug, Clone, PartialEq)]
pub struct StructInfo {
    pub id: u32,
    pub name: String,
    /// `Position::file_id` of the file whose `type` keyword declared this
    /// struct — the provenance the import-visibility check resolves to a
    /// defining module.
    pub file_id: u32,
    /// Module visibility of the type itself: `public type` opts the struct
    /// into being nameable from other modules; the default is private to its
    /// defining module. Like `file_id`, this is a declaration-site fact fixed
    /// at intern (prescan) time — it must be known before the `type` body
    /// parses, because under import cycles a module's uses can legally
    /// precede the definition in the inlined token stream.
    pub vis: Visibility,
    /// Fields in declaration/layout order. Empty until [`ModuleSymbols::set_fields`].
    pub fields: Vec<FieldInfo>,
    /// Field name -> index into `fields` (mirrors the LLVM struct element order).
    pub field_index: HashMap<String, usize>,
    /// Methods keyed by their (unmangled) source name. Populated in Milestone 3.
    pub methods: HashMap<String, MethodSig>,
    /// Leading attributes of the `type` declaration itself, in source order
    /// (outside-in, leftmost applied last). Empty until
    /// [`ModuleSymbols::set_struct_attrs`].
    pub attrs: Vec<Attribute>,
}

/// A registered C-style enum: its name, ordered variants, and provenance.
///
/// Shaped in parallel to [`StructInfo`] (id, `file_id`, `vis`, `attrs`) so the
/// import-visibility check and any future metaprogram query index treat the two
/// item kinds uniformly. An enum has no layout or methods — a variant's value
/// is just its index into `variants`, and the type lowers to `i32`.
#[derive(Debug, Clone, PartialEq)]
pub struct EnumInfo {
    pub id: u32,
    pub name: String,
    /// `Position::file_id` of the file whose `enum` keyword declared this enum
    /// — the provenance the import-visibility check resolves to a defining
    /// module.
    pub file_id: u32,
    /// Module visibility: `public enum` opts the enum into being nameable from
    /// other modules; the default is private to its defining module. Like
    /// [`StructInfo::vis`], it is fixed at intern (prescan) time — import
    /// cycles can place a use before the definition in the token stream.
    pub vis: Visibility,
    /// Variant names in declaration order; the index *is* the variant's value.
    /// Empty until [`ModuleSymbols::set_enum_variants`].
    pub variants: Vec<String>,
    /// Leading attributes of the `enum` declaration, in source order
    /// (outside-in, leftmost applied last). Empty until
    /// [`ModuleSymbols::set_enum_attrs`].
    pub attrs: Vec<Attribute>,
}

/// A distinct function-pointer signature (`fn(params) -> return_type`).
/// Two FnPtr ids are equal iff their `FnPtrSig`s compare equal.
#[derive(Debug, Clone, PartialEq)]
pub struct FnPtrSig {
    pub params: Vec<LangType>,
    pub return_type: LangType,
}

/// A defined type alias: the (eagerly resolved) target type plus the
/// `Position::file_id` of the file whose `alias` directive declared it —
/// the provenance the import-visibility check resolves to a defining module.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AliasInfo {
    pub ty: LangType,
    pub file_id: u32,
}

/// The program-wide table of resolved global symbols.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ModuleSymbols {
    /// Functions by name (the de-duplicated signature store).
    functions: HashMap<String, FunctionSymbol>,
    /// Type-structs indexed by id (index into the vec == the id).
    structs_by_id: Vec<StructInfo>,
    /// Struct name -> id.
    structs_by_name: HashMap<String, u32>,
    /// Enums indexed by id (index into the vec == the id).
    enums_by_id: Vec<EnumInfo>,
    /// Enum name -> id.
    enums_by_name: HashMap<String, u32>,
    /// Alias name -> the type it resolves to plus its declaring file.
    aliases: HashMap<String, AliasInfo>,
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

    /// Add or update a function.
    ///
    /// Mirrors the previous `SymbolTable::add_function`: a declaration may be
    /// followed by a matching definition, but two bodies, or a definition that
    /// disagrees with an earlier declaration, are errors.
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

    /// Iterate over all registered functions.
    pub fn functions(&self) -> impl Iterator<Item = (&String, &FunctionSymbol)> {
        self.functions.iter()
    }

    // ── Type-structs ────────────────────────────────────────────────────────────

    /// Reserve an id for a struct name, creating an empty (body-less) entry.
    /// `file_id` records the declaring file (from the `type` keyword token)
    /// and `vis` the declared module visibility (`public type` vs `type`),
    /// both consumed by the visibility checks.
    ///
    /// Called during the parser's name-collection prescan so that struct names
    /// resolve regardless of declaration order (including self/mutual reference).
    /// Returns the existing id if the name was already interned (the first
    /// declaration's `file_id`/`vis` win; a second `type` body for the same
    /// name is a duplicate-type error downstream).
    pub fn intern_struct(&mut self, name: &str, file_id: u32, vis: Visibility) -> u32 {
        if let Some(&id) = self.structs_by_name.get(name) {
            return id;
        }
        let id = u32::try_from(self.structs_by_id.len())
            .expect("number of type-structs exceeds u32::MAX");
        self.structs_by_id.push(StructInfo {
            id,
            name: name.to_string(),
            file_id,
            vis,
            fields: Vec::new(),
            field_index: HashMap::new(),
            methods: HashMap::new(),
            attrs: Vec::new(),
        });
        self.structs_by_name.insert(name.to_string(), id);
        id
    }

    #[must_use]
    pub fn struct_id(&self, name: &str) -> Option<u32> {
        self.structs_by_name.get(name).copied()
    }

    #[must_use]
    pub fn struct_info(&self, id: u32) -> &StructInfo {
        &self.structs_by_id[id as usize]
    }

    #[must_use]
    pub fn struct_info_mut(&mut self, id: u32) -> &mut StructInfo {
        &mut self.structs_by_id[id as usize]
    }

    /// All registered structs, in id order.
    pub fn structs(&self) -> impl Iterator<Item = &StructInfo> {
        self.structs_by_id.iter()
    }

    /// Replace a struct's fields and rebuild its `field_index`.
    pub fn set_fields(&mut self, id: u32, fields: Vec<FieldInfo>) {
        let field_index = fields
            .iter()
            .enumerate()
            .map(|(i, f)| (f.name.clone(), i))
            .collect();
        let info = &mut self.structs_by_id[id as usize];
        info.fields = fields;
        info.field_index = field_index;
    }

    /// Attach the leading attributes of a `type` declaration to its struct
    /// (`@attr type Name { ... }`). Interning happens in the prescan, long
    /// before the attributes are parsed — hence a setter rather than an
    /// `intern_struct` parameter.
    pub fn set_struct_attrs(&mut self, id: u32, attrs: Vec<Attribute>) {
        self.structs_by_id[id as usize].attrs = attrs;
    }

    /// Look up a field by name, returning its layout index and info.
    #[must_use]
    pub fn field(&self, id: u32, name: &str) -> Option<(usize, &FieldInfo)> {
        let info = &self.structs_by_id[id as usize];
        let idx = *info.field_index.get(name)?;
        Some((idx, &info.fields[idx]))
    }

    /// Register a method signature on a struct (Milestone 3).
    pub fn add_method(&mut self, id: u32, name: String, sig: MethodSig) {
        self.structs_by_id[id as usize].methods.insert(name, sig);
    }

    // ── Enums ─────────────────────────────────────────────────────────────────

    /// Reserve an id for an enum name, creating an empty (variant-less) entry.
    /// `file_id` records the declaring file (from the `enum` keyword token) and
    /// `vis` the declared module visibility (`public enum` vs `enum`), both
    /// consumed by the visibility checks.
    ///
    /// Called during the parser's name-collection prescan so enum names resolve
    /// regardless of declaration order (forward references, import cycles).
    /// Returns the existing id if the name was already interned (the first
    /// declaration's `file_id`/`vis` win; a second `enum` body for the same
    /// name is a duplicate-type error downstream).
    pub fn intern_enum(&mut self, name: &str, file_id: u32, vis: Visibility) -> u32 {
        if let Some(&id) = self.enums_by_name.get(name) {
            return id;
        }
        let id = u32::try_from(self.enums_by_id.len()).expect("number of enums exceeds u32::MAX");
        self.enums_by_id.push(EnumInfo {
            id,
            name: name.to_string(),
            file_id,
            vis,
            variants: Vec::new(),
            attrs: Vec::new(),
        });
        self.enums_by_name.insert(name.to_string(), id);
        id
    }

    #[must_use]
    pub fn enum_id(&self, name: &str) -> Option<u32> {
        self.enums_by_name.get(name).copied()
    }

    #[must_use]
    pub fn enum_info(&self, id: u32) -> &EnumInfo {
        &self.enums_by_id[id as usize]
    }

    /// All registered enums, in id order.
    pub fn enums(&self) -> impl Iterator<Item = &EnumInfo> {
        self.enums_by_id.iter()
    }

    /// Replace an enum's variant list (finalising its `enum` body).
    pub fn set_enum_variants(&mut self, id: u32, variants: Vec<String>) {
        self.enums_by_id[id as usize].variants = variants;
    }

    /// Attach the leading attributes of an `enum` declaration to its info.
    /// Interning happens in the prescan, before the attributes are parsed —
    /// hence a setter rather than an `intern_enum` parameter.
    pub fn set_enum_attrs(&mut self, id: u32, attrs: Vec<Attribute>) {
        self.enums_by_id[id as usize].attrs = attrs;
    }

    /// The value (index) of a variant by name, or `None` if the enum has no
    /// such variant.
    #[must_use]
    pub fn enum_variant_index(&self, id: u32, variant: &str) -> Option<usize> {
        self.enums_by_id[id as usize]
            .variants
            .iter()
            .position(|v| v == variant)
    }

    // ── Aliases ───────────────────────────────────────────────────────────────

    /// Define a type alias (`alias New Target`). `file_id` records the
    /// declaring file (from the `alias` keyword token) for the
    /// import-visibility check.
    pub fn define_alias(&mut self, name: String, ty: LangType, file_id: u32) {
        self.aliases.insert(name, AliasInfo { ty, file_id });
    }

    #[must_use]
    pub fn resolve_alias(&self, name: &str) -> Option<LangType> {
        self.aliases.get(name).map(|info| info.ty)
    }

    /// Full alias entry — the resolved type plus its declaring file.
    #[must_use]
    pub fn alias_info(&self, name: &str) -> Option<AliasInfo> {
        self.aliases.get(name).copied()
    }

    // ── Function-pointer signatures ──────────────────────────────────────────

    /// Intern a function-pointer signature, returning a stable id. Identical
    /// signatures return the same id (structural deduplication), so two FnPtr
    /// types are compared by id alone — `LangType` stays `Copy`/`Eq`.
    pub fn intern_fnptr(&mut self, params: Vec<LangType>, return_type: LangType) -> u32 {
        let sig = FnPtrSig {
            params,
            return_type,
        };
        if let Some(idx) = self.fnptr_sigs.iter().position(|s| *s == sig) {
            return u32::try_from(idx).expect("fnptr signature index overflows u32");
        }
        let id = u32::try_from(self.fnptr_sigs.len())
            .expect("number of fn-ptr signatures exceeds u32::MAX");
        self.fnptr_sigs.push(sig);
        id
    }

    /// Resolve a FnPtr id back to its signature.
    #[must_use]
    pub fn fnptr_sig(&self, id: u32) -> &FnPtrSig {
        &self.fnptr_sigs[id as usize]
    }

    /// All registered FnPtr signatures, indexed by id. Used by codegen to seed
    /// its local cache (the walker is not threaded the `Program`).
    #[must_use]
    pub fn all_fnptr_sigs(&self) -> &[FnPtrSig] {
        &self.fnptr_sigs
    }
}
