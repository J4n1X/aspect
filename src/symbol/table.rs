use crate::lexer::{LangType, Position};
use crate::scope::ScopeStack;
use thiserror::Error;

/// Errors produced when mutating the symbol table.
///
/// These carry no source position — the caller knows the offending site and
/// attaches it when converting to a `ParserError`.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SymbolError {
    #[error("variable '{0}' is already declared in this scope")]
    DuplicateVariable(String),

    #[error("function '{0}' already has a body")]
    FunctionAlreadyDefined(String),

    #[error("definition of function '{0}' does not match its declaration")]
    SignatureMismatch(String),
}

/// Symbol information
#[derive(Debug, Clone, PartialEq)]
pub struct Symbol {
    pub name: String,
    pub symbol_type: LangType,
    pub pos: Position,
}

/// Variable symbol
pub type VarSymbol = Symbol;

/// Function symbol.
///
/// Stored in [`crate::symbol::module::ModuleSymbols`] (the cross-phase table).
/// Kept here alongside [`SymbolError`] because the parser builds these while it
/// parses; the symbol *table* below only manages variable scopes.
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionSymbol {
    pub name: String,
    pub params: Vec<(LangType, String)>,
    pub return_type: LangType,
    pub is_extern: bool,
    pub has_body: bool,
    pub pos: Position,
}

/// Transient, parse-time table of variable scopes.
///
/// Functions, type-structs, and aliases live in
/// [`crate::symbol::module::ModuleSymbols`] (which rides on the `Program`).
/// This table holds only the lexical variable scopes the parser needs while
/// parsing function bodies, and is discarded once parsing completes.
#[derive(Debug)]
pub struct SymbolTable {
    /// Lexical scopes mapping variable names to their symbols.
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

    /// Enter a new scope
    pub fn enter_scope(&mut self) {
        self.var_scopes.enter();
    }

    /// Exit the current scope and clean up variables
    pub fn exit_scope(&mut self) {
        self.var_scopes.exit();
    }

    /// Add a variable to the current scope
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

    /// Look up a variable in all scopes (from innermost to outermost)
    #[must_use]
    pub fn lookup_variable(&self, name: &str) -> Option<&VarSymbol> {
        self.var_scopes.lookup(name)
    }
}
