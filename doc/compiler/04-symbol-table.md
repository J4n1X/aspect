# Symbol Table

The symbol table (`src/symbol/table.rs`) manages scoped **variable** lookups during parsing. It has no function registry: functions, type-structs, and aliases live in `ModuleSymbols` (`src/symbol/module.rs`), which rides on the `Program` across phases. `SymbolTable` is transient — it holds only the lexical variable scopes the parser needs while parsing function bodies, and is discarded once parsing completes.

## Structure

```rust
pub struct SymbolTable {
    /// Lexical scopes mapping variable names to their symbols.
    var_scopes: ScopeStack<VarSymbol>,
}
```

Scoping is delegated to the generic `ScopeStack<T>` in `src/scope.rs` — a stack of hashmaps, one per scope level, shared by the lexer, parser, typechecker, and codegen.

## Variable Symbols

```rust
pub struct Symbol {
    pub name: String,
    pub symbol_type: LangType,  // Full type info
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
| `enter_scope()` | Push a new innermost `HashMap` onto `var_scopes` |
| `exit_scope()` | Pop the innermost `HashMap`; the global scope is never popped (`ScopeStack::exit` guards on `len() > 1`) |

Initial state has one scope — the global scope. Block statements and for-loops create new scopes.

## Variable Operations

### `add_variable(name: String, symbol_type: LangType, pos: Position) -> Result<(), SymbolError>`

Inserts into the **current (topmost) scope only**. Returns an error if a variable with the same name already exists **in that same scope**. Shadowing of outer scopes is allowed since they're in different hashmaps.

### `lookup_variable(name) -> Option<&VarSymbol>`

Searches from **innermost scope to outermost**. Returns `None` if not found in any scope.

### `lookup_variable_scoped(name) -> Option<(&VarSymbol, bool)>`

As above, additionally reporting whether the binding is a *global* (outermost scope). The import-visibility check applies to globals only — locals and parameters are same-function by construction.

## `SymbolError`

`DuplicateVariable` / `FunctionAlreadyDefined` / `SignatureMismatch`. These carry no
position; the caller attaches one via `ParserError::from_symbol`.

Note `FunctionAlreadyDefined` and `SignatureMismatch` both render as
`ParserError::FunctionRedefinition`, so a signature mismatch reports "Redefinition of
function 'f'" — the distinction below is invisible in the diagnostic.

## Function Operations (on `ModuleSymbols`, not `SymbolTable`)

These live in `src/symbol/module.rs`.

### `ModuleSymbols::add_function(func: FunctionSymbol) -> Result<(), SymbolError>`

Handles three cases:
1. **Duplicate body**: existing function has a body and new one also has a body → error
2. **Forward declaration → definition**: existing has no body, new one does → validates `params` and `return_type` match exactly; error if they don't
3. **First declaration or bodyless re-declaration**: inserts/overwrites

### `ModuleSymbols::lookup_function(name) -> Option<&FunctionSymbol>`

Simple lookup in the flat `functions` HashMap. `ModuleSymbols` also holds type-structs and aliases.

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
- `parse_function()` calls `ModuleSymbols::add_function()` (via `self.module`)
- `parse_block_statement()` calls `enter_scope()` / `exit_scope()`
- `parse_for_statement()` calls `enter_scope()` / `exit_scope()`
- Expression parsing calls `lookup_variable()` for identifier types and `lookup_function()` for call return types

The typechecker has its own **independent** scope system (a separate `Vec<HashMap<String, LangType>>`), not sharing the parser's `SymbolTable`.
