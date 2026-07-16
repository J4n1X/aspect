# Symbol Table

The symbol table (`src/symbol/table.rs`) manages scoped variable lookups and a flat function registry. It is built during parsing and used by the parser for type resolution.

## Structure

```rust
pub struct SymbolTable {
    var_scopes: Vec<HashMap<String, VarSymbol>>,  // Stack of variable scopes
    functions: HashMap<String, FunctionSymbol>,     // Flat function table (global)
    current_scope: usize,                           // Current nesting depth
}
```

- Variables use a **stack of hashmaps** — each scope level gets its own `HashMap`.
- Functions are stored in a **single flat `HashMap`** — they are always global.

## Variable Symbols

```rust
pub struct Symbol {
    pub name: String,
    pub symbol_type: LangType,  // Full type info
    pub scope_level: usize,     // Scope depth at declaration
    pub pos: Position,          // Source location
}
```

`VarSymbol` is a type alias for `Symbol`.

## Function Symbols

```rust
pub struct FunctionSymbol {
    pub name: String,
    pub params: Vec<(LangType, String)>,
    pub return_type: LangType,
    pub is_extern: bool,
    pub has_body: bool,  // true if a body was provided (vs forward decl)
    pub pos: Position,
}
```

## Scope Management

| Method | Behavior |
|--------|----------|
| `enter_scope()` | Increment `current_scope`, push new `HashMap` |
| `exit_scope()` | Pop topmost `HashMap`, decrement `current_scope` (refuses to go below 0) |

Initial state has one scope (index 0) — the global scope. Block statements and for-loops create new scopes.

## Variable Operations

### `add_variable(name, symbol) -> Result<(), String>`

Inserts into the **current (topmost) scope only**. Returns an error if a variable with the same name already exists **in that same scope**. Shadowing of outer scopes is allowed since they're in different hashmaps.

### `lookup_variable(name) -> Option<&VarSymbol>`

Searches from **innermost scope to outermost** (iterates `var_scopes` in reverse). Returns `None` if not found in any scope.

## Function Operations

### `add_function(name, symbol) -> Result<(), String>`

Handles three cases:
1. **Duplicate body**: existing function has a body and new one also has a body → error
2. **Forward declaration → definition**: existing has no body, new one does → validates `params` and `return_type` match exactly; error if they don't
3. **First declaration or bodyless re-declaration**: inserts/overwrites

### `lookup_function(name) -> Option<&FunctionSymbol>`

Simple lookup in the flat `functions` HashMap.

## Scope Example

```aspect
fn example() {
    i32 x = 10          # scope 1: {x: i32}
    {
        i32 y = 20      # scope 2: {y: i32} — x still visible from scope 1
        i32 x = 30      # scope 2: {y: i32, x: i32} — shadows outer x
    }                   # exit scope 2
    # x is still 10 here (scope 1)
}
```

## Usage in the Pipeline

The parser creates and populates the symbol table during parsing:
- `parse_var_decl_or_assignment()` calls `add_variable()`
- `parse_function()` calls `add_function()`
- `parse_block_statement()` calls `enter_scope()` / `exit_scope()`
- `parse_for_statement()` calls `enter_scope()` / `exit_scope()`
- Expression parsing calls `lookup_variable()` for identifier types and `lookup_function()` for call return types

The typechecker has its own **independent** scope system (a separate `Vec<HashMap<String, LangType>>`), not sharing the parser's `SymbolTable`.
