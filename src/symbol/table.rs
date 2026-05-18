use crate::lexer::{LangType, Position};
use std::collections::HashMap;

/// Symbol information
#[derive(Debug, Clone, PartialEq)]
pub struct Symbol {
    pub name: String,
    pub symbol_type: LangType,
    pub scope_level: usize,
    pub pos: Position,
}

/// Variable symbol
pub type VarSymbol = Symbol;

/// Function symbol
#[derive(Debug, Clone, PartialEq)]
pub struct FunctionSymbol {
    pub name: String,
    pub params: Vec<(LangType, String)>,
    pub return_type: LangType,
    pub is_extern: bool,
    pub has_body: bool,
    pub pos: Position,
}

/// Symbol table for managing variables and functions
#[derive(Debug)]
pub struct SymbolTable {
    /// Variable scopes - each scope is a `HashMap` of name -> Symbol
    var_scopes: Vec<HashMap<String, VarSymbol>>,
    /// Function table - global scope only
    functions: HashMap<String, FunctionSymbol>,
    /// Current scope level
    current_scope: usize,
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
            var_scopes: vec![HashMap::new()],
            functions: HashMap::new(),
            current_scope: 0,
        }
    }

    /// Enter a new scope
    pub fn enter_scope(&mut self) {
        self.current_scope += 1;
        self.var_scopes.push(HashMap::new());
    }

    /// Exit the current scope and clean up variables
    pub fn exit_scope(&mut self) {
        if self.current_scope > 0 {
            self.var_scopes.pop();
            self.current_scope -= 1;
        }
    }

    /// Add a variable to the current scope
    /// # Errors
    /// If the variable already exists in the current scope
    pub fn add_variable(
        &mut self,
        name: String,
        symbol_type: LangType,
        pos: Position,
    ) -> Result<(), String> {
        let symbol = VarSymbol {
            name: name.clone(),
            symbol_type,
            scope_level: self.current_scope,
            pos,
        };

        if let Some(current_scope) = self.var_scopes.last_mut() {
            if current_scope.contains_key(&name) {
                return Err(format!("Variable '{name}' already declared in this scope"));
            }
            current_scope.insert(name, symbol);
            Ok(())
        } else {
            Err("No current scope".to_string())
        }
    }

    /// Look up a variable in all scopes (from innermost to outermost)
    #[must_use]
    pub fn lookup_variable(&self, name: &str) -> Option<&VarSymbol> {
        for scope in self.var_scopes.iter().rev() {
            if let Some(symbol) = scope.get(name) {
                return Some(symbol);
            }
        }
        None
    }

    /// Add or update a function
    /// # Errors
    /// If there is a conflicting definition
    pub fn add_function(&mut self, func: FunctionSymbol) -> Result<(), String> {
        match self.functions.get(&func.name) {
            Some(existing) if existing.has_body && func.has_body => {
                return Err(format!("Function '{}' already has a body", func.name));
            }
            Some(existing) if !existing.has_body && func.has_body => {
                // Forward declaration followed by definition - check compatibility
                if existing.params != func.params || existing.return_type != func.return_type {
                    return Err(format!(
                        "Function definition doesn't match declaration for '{}'",
                        func.name
                    ));
                }
            }
            _ => {}
        }

        self.functions.insert(func.name.clone(), func);
        Ok(())
    }

    /// Look up a function
    #[must_use]
    pub fn lookup_function(&self, name: &str) -> Option<&FunctionSymbol> {
        self.functions.get(name)
    }

    /// Get current scope level
    #[must_use]
    pub fn current_scope_level(&self) -> usize {
        self.current_scope
    }

    /// Get all functions (for final program assembly)
    #[must_use]
    pub fn get_all_functions(&self) -> Vec<&FunctionSymbol> {
        self.functions.values().collect()
    }
}
