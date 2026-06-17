use crate::parser::LangType;
use crate::scope::ScopeStack as LexicalScopes;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, PointerValue};
use std::collections::HashMap;

/// Info for a local variable in a scope.
pub struct LocalVar<'ctx> {
    pub ptr: PointerValue<'ctx>,
    pub llvm_type: BasicTypeEnum<'ctx>,
    pub lang_type: LangType,
    /// If the variable was declared `const` and its initializer folded to a
    /// compile-time constant, the folded value is stored here so reads bypass
    /// the alloca/load entirely.
    pub const_value: Option<BasicValueEnum<'ctx>>,
}

/// Info for a global variable.
pub struct GlobalVarInfo<'ctx> {
    pub ptr: PointerValue<'ctx>,
    pub llvm_type: BasicTypeEnum<'ctx>,
    pub lang_type: LangType,
}

/// A borrowed view of either a local or a global variable.
pub enum VarRef<'a, 'ctx> {
    Local(&'a LocalVar<'ctx>),
    Global(&'a GlobalVarInfo<'ctx>),
}

impl<'a, 'ctx> VarRef<'a, 'ctx> {
    pub fn ptr(&self) -> PointerValue<'ctx> {
        match self {
            VarRef::Local(v) => v.ptr,
            VarRef::Global(g) => g.ptr,
        }
    }

    pub fn llvm_type(&self) -> BasicTypeEnum<'ctx> {
        match self {
            VarRef::Local(v) => v.llvm_type,
            VarRef::Global(g) => g.llvm_type,
        }
    }

    pub fn lang_type(&self) -> LangType {
        match self {
            VarRef::Local(v) => v.lang_type,
            VarRef::Global(g) => g.lang_type,
        }
    }

    pub fn const_value(&self) -> Option<BasicValueEnum<'ctx>> {
        match self {
            VarRef::Local(v) => v.const_value,
            VarRef::Global(_) => None,
        }
    }
}

/// Scoped variable storage: a stack of lexical scopes for locals (backed by the
/// shared [`crate::scope::ScopeStack`]) plus a flat map for globals.
pub struct ScopeStack<'ctx> {
    locals: LexicalScopes<LocalVar<'ctx>>,
    globals: HashMap<String, GlobalVarInfo<'ctx>>,
}

impl<'ctx> Default for ScopeStack<'ctx> {
    fn default() -> Self {
        Self::new()
    }
}

impl<'ctx> ScopeStack<'ctx> {
    pub fn new() -> Self {
        Self {
            locals: LexicalScopes::new(),
            globals: HashMap::new(),
        }
    }

    pub fn enter(&mut self) {
        self.locals.enter();
    }

    pub fn exit(&mut self) {
        self.locals.exit();
    }

    pub fn insert_local(
        &mut self,
        name: String,
        ptr: PointerValue<'ctx>,
        llvm_type: BasicTypeEnum<'ctx>,
        lang_type: LangType,
        const_value: Option<BasicValueEnum<'ctx>>,
    ) {
        self.locals.insert(
            name,
            LocalVar {
                ptr,
                llvm_type,
                lang_type,
                const_value,
            },
        );
    }

    pub fn insert_global(&mut self, name: String, info: GlobalVarInfo<'ctx>) {
        self.globals.insert(name, info);
    }

    /// Look up a local variable, searching from innermost scope outward.
    pub fn lookup_local(&self, name: &str) -> Option<&LocalVar<'ctx>> {
        self.locals.lookup(name)
    }

    pub fn lookup_global(&self, name: &str) -> Option<&GlobalVarInfo<'ctx>> {
        self.globals.get(name)
    }

    /// Look up a variable, preferring locals over globals.
    pub fn lookup_any(&self, name: &str) -> Option<VarRef<'_, 'ctx>> {
        if let Some(v) = self.lookup_local(name) {
            return Some(VarRef::Local(v));
        }
        self.globals.get(name).map(VarRef::Global)
    }

    /// Iterate over all local scopes (innermost first) — used by const-folding.
    pub fn iter_scopes(&self) -> impl Iterator<Item = &HashMap<String, LocalVar<'ctx>>> {
        self.locals.iter_scopes()
    }

    /// Direct access to the globals map — used by const-folding.
    pub fn globals(&self) -> &HashMap<String, GlobalVarInfo<'ctx>> {
        &self.globals
    }
}
