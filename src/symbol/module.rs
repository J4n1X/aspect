//! Unified, cross-phase module symbol table.
//!
//! [`ModuleSymbols`] is the single authoritative table of a program's global
//! symbols — functions, type-structs, and aliases. It is built by the parser
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
use crate::symbol::table::{FunctionSymbol, SymbolError};
use std::collections::HashMap;

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
}

/// A registered type-struct: its name, ordered fields, and methods.
#[derive(Debug, Clone, PartialEq)]
pub struct StructInfo {
    pub id: u32,
    pub name: String,
    /// Fields in declaration/layout order. Empty until [`ModuleSymbols::set_fields`].
    pub fields: Vec<FieldInfo>,
    /// Field name -> index into `fields` (mirrors the LLVM struct element order).
    pub field_index: HashMap<String, usize>,
    /// Methods keyed by their (unmangled) source name. Populated in Milestone 3.
    pub methods: HashMap<String, MethodSig>,
}

/// A distinct function-pointer signature (`fn(params) -> return_type`).
/// Two FnPtr ids are equal iff their `FnPtrSig`s compare equal.
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
    /// Type-structs indexed by id (index into the vec == the id).
    structs_by_id: Vec<StructInfo>,
    /// Struct name -> id.
    structs_by_name: HashMap<String, u32>,
    /// Alias name -> the type it resolves to.
    aliases: HashMap<String, LangType>,
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
    ///
    /// Called during the parser's name-collection prescan so that struct names
    /// resolve regardless of declaration order (including self/mutual reference).
    /// Returns the existing id if the name was already interned.
    pub fn intern_struct(&mut self, name: &str) -> u32 {
        if let Some(&id) = self.structs_by_name.get(name) {
            return id;
        }
        let id = u32::try_from(self.structs_by_id.len())
            .expect("number of type-structs exceeds u32::MAX");
        self.structs_by_id.push(StructInfo {
            id,
            name: name.to_string(),
            fields: Vec::new(),
            field_index: HashMap::new(),
            methods: HashMap::new(),
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

    // ── Aliases ───────────────────────────────────────────────────────────────

    /// Define a type alias (`alias New Target`).
    pub fn define_alias(&mut self, name: String, ty: LangType) {
        self.aliases.insert(name, ty);
    }

    #[must_use]
    pub fn resolve_alias(&self, name: &str) -> Option<LangType> {
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
