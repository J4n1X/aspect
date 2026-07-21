use crate::lexer::{LangType, Position};
use crate::scope::ScopeStack;
use thiserror::Error;

/// Positionless — the caller knows the offending site and attaches it when
/// converting to a `ParserError`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SymbolError {
    #[error("variable '{0}' is already declared in this scope")]
    DuplicateVariable(String),

    #[error("function '{0}' already has a body")]
    FunctionAlreadyDefined(String),

    #[error("definition of function '{0}' does not match its declaration")]
    SignatureMismatch(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Symbol {
    pub name: String,
    pub symbol_type: LangType,
    pub pos: Position,
}

pub type VarSymbol = Symbol;

/// Stored in [`crate::symbol::module::ModuleSymbols`], but defined here because
/// the parser builds these while it parses.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionSymbol {
    pub name: String,
    pub params: Vec<(LangType, String)>,
    pub return_type: LangType,
    pub is_extern: bool,
    pub has_body: bool,
    /// Whether another module may call this through `$import` — mirrors
    /// [`crate::parser::ast::FunctionProto::vis`], stored here so call
    /// resolution can enforce it. Independent of LLVM linkage.
    pub vis: crate::symbol::module::Visibility,
    pub pos: Position,
}

/// The transient, parse-time variable scopes, discarded once parsing completes
/// (global symbols live in [`crate::symbol::module::ModuleSymbols`]).
#[derive(Debug)]
pub struct SymbolTable {
    var_scopes: ScopeStack<VarSymbol>,
}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolTable {
    #[must_use]
    pub fn new() -> Self {
        Self {
            var_scopes: ScopeStack::new(),
        }
    }

    pub fn enter_scope(&mut self) {
        self.var_scopes.enter();
    }

    pub fn exit_scope(&mut self) {
        self.var_scopes.exit();
    }

    /// # Errors
    /// Returns [`SymbolError::DuplicateVariable`] if a variable of the same name
    /// already exists in the current scope.
    pub fn add_variable(
        &mut self,
        name: String,
        symbol_type: LangType,
        pos: Position,
    ) -> Result<(), SymbolError> {
        if self.var_scopes.contains_in_current(&name) {
            return Err(SymbolError::DuplicateVariable(name));
        }
        let symbol = VarSymbol {
            name: name.clone(),
            symbol_type,
            pos,
        };
        self.var_scopes.insert(name, symbol);
        Ok(())
    }

    /// Searches innermost scope outward.
    #[must_use]
    pub fn lookup_variable(&self, name: &str) -> Option<&VarSymbol> {
        self.var_scopes.lookup(name)
    }

    /// Also reports whether the binding is a *global* (outermost scope) — the
    /// import-visibility check applies to globals only, locals being
    /// same-function by construction.
    #[must_use]
    pub fn lookup_variable_scoped(&self, name: &str) -> Option<(&VarSymbol, bool)> {
        self.var_scopes.lookup_scoped(name)
    }
}
