# Symbol Table

The symbol table (`src/symbol/table.rs`) manages scoped **variable** lookups during parsing. It has no function registry: functions and named types live in `ModuleSymbols` (`src/symbol/module.rs`), shared by handle off the `Program` across phases. `SymbolTable` is transient — it holds only the lexical variable scopes the parser needs while parsing function bodies, and is discarded once parsing completes.

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
    pub vis: Visibility, // whether another module may call it through $import
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

Simple lookup in the flat `functions` HashMap. `ModuleSymbols` also holds every named type (see below) and the interned fn-pointer signatures.

## Named-Type Registry (on `ModuleSymbols`)

Type-structs, enums, sums and aliases share **one** id space and **one** name
map. A `TypeDef` carries the header every kind needs; only the kind-specific part
sits in `TypeKind`:

```rust
pub struct TypeDef {
    pub id: u32,          // what TypeBase::{Struct,Enum,Sum}(id) carries
    pub name: String,
    pub file_id: u32,     // provenance for the import-visibility check
    pub vis: Visibility,  // fixed at intern time
    pub pos: Position,    // the declaring keyword
    pub defined: bool,    // false between name reservation and body parse
    pub kind: TypeKind,
}

pub enum TypeKind {
    Struct(StructBody),  // fields + field_index + methods
    Enum(EnumBody),      // variants: Vec<String>; index == the value
    Sum(SumBody),        // variants: Vec<SumVariant>; index == the discriminant
    Alias(LangType),     // eagerly-resolved target
}
```

One table is what makes these single pieces of code rather than one copy per
kind:

| Concern | Entry point |
|---|---|
| "Is this name taken?" (any kind) | `type_id(name)` / `lookup_type(name)` |
| Name reservation for every kind | one `prescan_type_names` pass, keyword → `TypeKind` |
| Import + `public` gate | `Parser::check_type_visibility(id, pos)`, noun from `TypeKind::noun()` |
| Duplicate detection | `Parser::claim_type_decl(name, pos, want_kind)` |
| Named-type resolution | one `match def.kind` in `resolve_named_type` |

Kind-filtered helpers (`struct_id`/`enum_id`/`sum_id`, `structs()`/`enums()`/
`sums()`) are thin views over that one table, and `type_def(id).as_struct()` /
`.as_enum()` / `.as_sum()` reach a body — panicking on a kind mismatch, which the
`TypeBase` variant or a kind-filtered lookup has already ruled out.

Two properties worth knowing:

- **`defined` is a real field, not an inferred one.** Duplicate detection used to
  ask whether a body was still empty, which cannot tell "not parsed yet" from
  "parsed, and empty" — two empty `type Foo {}` declarations compiled silently.
  It also entangled the language's rejection of empty enums/sums with the
  sentinel's fidelity; those rules are now free-standing choices.
- **A cross-kind redeclaration is caught at the body, not the prescan.** The
  prescan reserves the first spelling it sees; `claim_type_decl` then rejects a
  body whose def is of another kind, so the diagnostic lands on the second
  declaration.

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

### Ownership across phases

`Program::symbols` is an `Rc<ModuleSymbols>`. The parser builds the table and
wraps it once when it finishes; the typechecker and code generator each take an
`Rc::clone` of that handle and only ever read through it. There is therefore
exactly **one** copy of every signature, field layout and variant payload in the
compiler — no phase re-derives its own index, so none can drift out of agreement
with the registry the interned ids point into.

Cloning the *handle* (never the table) is also what resolves the borrow problem
both later phases hit: they need a declared field or payload list open while
recursing through `&mut self`. See doc/compiler/06-codegen.md § *The registry
handle* for the pattern.
