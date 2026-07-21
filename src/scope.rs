//! A generic lexical scope stack shared by every compiler phase.
//!
//! Every phase needs a stack of name → value maps with innermost-outward
//! lookup, differing only in the *value* stored, so the mechanics live here once
//! as [`ScopeStack<T>`] and each phase parameterises it with its own payload.
//!
//! The outermost (global) scope is created on construction and never popped, so
//! [`ScopeStack::insert`] and [`ScopeStack::contains_in_current`] always have a
//! scope to act on.

use std::collections::HashMap;

/// A stack of lexical scopes mapping names to values of type `T`.
#[derive(Debug, Clone)]
pub struct ScopeStack<T> {
    scopes: Vec<HashMap<String, T>>,
}

impl<T> ScopeStack<T> {
    /// Create a stack with a single (global) scope.
    #[must_use]
    pub fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
        }
    }

    /// Push a new innermost scope.
    pub fn enter(&mut self) {
        self.scopes.push(HashMap::new());
    }

    /// Pop the innermost scope. The global scope is never popped.
    pub fn exit(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }

    /// Bind `name` in the innermost scope, returning any previous binding there.
    pub fn insert(&mut self, name: String, value: T) -> Option<T> {
        self.scopes
            .last_mut()
            .expect("scope stack always retains its global scope")
            .insert(name, value)
    }

    /// Whether `name` is already bound in the *innermost* scope (shadowing an
    /// outer binding does not count).
    #[must_use]
    pub fn contains_in_current(&self, name: &str) -> bool {
        self.scopes
            .last()
            .is_some_and(|scope| scope.contains_key(name))
    }

    /// Look up `name`, searching from the innermost scope outward.
    #[must_use]
    pub fn lookup(&self, name: &str) -> Option<&T> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name))
    }

    /// Like [`ScopeStack::lookup`], additionally reporting whether the
    /// binding found lives in the outermost (global) scope. A local that
    /// shadows a global reports `false`.
    #[must_use]
    pub fn lookup_scoped(&self, name: &str) -> Option<(&T, bool)> {
        self.scopes
            .iter()
            .enumerate()
            .rev()
            .find_map(|(depth, scope)| scope.get(name).map(|value| (value, depth == 0)))
    }

    /// Iterate over the scopes, innermost first.
    pub fn iter_scopes(&self) -> impl Iterator<Item = &HashMap<String, T>> {
        self.scopes.iter().rev()
    }
}

impl<T> Default for ScopeStack<T> {
    fn default() -> Self {
        Self::new()
    }
}
