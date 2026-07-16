# Type-Struct System — Implementation Plan

Status: **complete** — all milestones (M0 foundation + symbol-table unification, M1 aliases,
M2 POD structs, M2b struct by-value ABI, M3 methods + `this` + `const fn`, M4 encapsulation
enforcement) implemented and verified. Remaining future work is the SysV / Win64 by-value
aggregate ABI for crossing the C/extern boundary (tracked in TODO.md). Supersedes the
discussion sketch in `Struct-Concept.md`.

This plan implements *aliases* and *type-structs* ("type-structs" is the internal/diagnostic
name; the surface keyword is `type`). It is grounded in a line-level survey of the five
subsystems (lexer, parser, typechecker, codegen, scope/symbol).

---

## 0. Locked design decisions

These were settled in design discussion and are not open for re-litigation during coding:

1. **`type` keyword defines a struct; there is no `struct` keyword.** Diagnostics call them
   "type-structs".
2. **`alias New Target`** is a C-style typedef: a pure name → `LangType` mapping.
3. **Visibility is `public` opt-in; hidden is the default.** A field is private unless prefixed
   `public`. There is no `hidden` keyword (privacy is the default, so it needs no marker).
4. **Struct literals must always be named: `Name { f = v, ... }`.** There is no bare/anonymous
   struct literal. This removes the Rust `if cond { }` ambiguity: a bare `{` is always a block;
   `Ident {` in expression position is a struct literal only when `Ident` resolves to a type-struct.
5. **Struct-literal validity rule:** `Name { ... }` is legal in a scope **iff** it names *every*
   field (no partial init, no defaulting) **and** every named field is *accessible* there. If any
   field is inaccessible (private, viewed from outside the type's own methods), the initializer is
   a **compile error** — never a silent default-init. Consequence: a type-struct with even one
   private field is constructible only from inside its own methods (the factory pattern that
   `SizedString.from_cstring` relies on).
6. **Type representation: `TypeBase::Struct(u32)`** — an interned struct id, *not* a `String`.
   `LangType` is `Copy`/`Eq` and copied by value in 100+ sites; a `String` payload would break
   `Copy` everywhere. A `u32` id preserves both derives with zero churn.
7. **Lowering: uniform by-pointer.** Structs are passed and returned by pointer (returns via an
   `sret` hidden out-pointer; value params via `byval`; `this` via a plain pointer). We do **not**
   implement the System V / Win64 by-value aggregate ABI now — that is a separate future item
   (see TODO "Struct by-value ABI"). Aspect-internal calls control both sides, so the uniform rule
   is correct; only `extern` by-value struct params/returns are forbidden until the ABI work lands.
8. **C-compatible layout from day one.** Struct LLVM types are built non-packed under the target
   data layout, which matches the platform C struct layout (natural alignment + tail padding). So
   passing a struct *pointer* to/from C is ABI-correct; only by-value crossing is deferred.

---

## 1. Type representation & the struct registry

### `TypeBase::Struct(u32)`  (`src/lexer/tokens.rs`)
- Add `Struct(u32)` to `TypeBase` (enum at `tokens.rs:150`). Derives unchanged — still `Copy, Eq`.
- Add a `TypeBase::Struct(id)` arm to `Display for LangType` (the `base_str` match at `tokens.rs:274`).
  `Display` cannot reach the registry, so it prints a placeholder `struct#<id>`. Pretty names in
  diagnostics are produced by a registry-aware helper at the error sites (see §4), not by `Display`.
- `langtype_from_str` (`tokens.rs:182`) is **left unchanged**: struct names lex as `Identifier`
  and are resolved to ids by the parser, never by the lexer.
- `size_bits` for a struct `LangType` is set to `0` and is **not meaningful** — struct layout comes
  from the LLVM struct type, and GEP uses field *index*, not byte offset. All numeric helpers guard
  structs out (§3, §4). (A real `sizeof` can fill this later; not needed now.)

### Unified `ModuleSymbols` table  (new `src/symbol/module.rs`, wired in `src/symbol/mod.rs`)

**Decision (unify, don't add a fourth silo).** The scope *mechanism* is already unified
(`ScopeStack<T>` backs all three phases), but function *signatures* are still rebuilt three times
(parser `SymbolTable.functions`, typechecker `FunctionSig`, codegen `function_lang_params`). Struct
ids are an interning decision fixed at parse time that codegen's GEP indices must match, so the
struct registry *cannot* be re-derived per phase — it must be shared. Rather than bolt a standalone
struct table next to the triplicated function story, all cross-phase **resolved global symbols** live
in one owned, lifetime-free `ModuleSymbols` table that rides on `Program` (like `string_literals`):

```rust
pub enum Visibility { Public, Private }

pub struct FieldInfo { pub name: String, pub ty: LangType, pub vis: Visibility }

pub struct MethodSig {                       // populated in Milestone 3
    pub mangled_name: String,                // "Type$method"
    pub params: Vec<(LangType, String)>,     // NOT including the implicit `this`
    pub return_type: LangType,
    pub is_static: bool,                     // no `this`
    pub is_const: bool,                      // `const fn` -> *const Struct receiver
}

pub struct StructInfo {
    pub id: u32,
    pub name: String,
    pub fields: Vec<FieldInfo>,              // declaration/layout order
    pub field_index: HashMap<String, usize>,
    pub methods: HashMap<String, MethodSig>, // Milestone 3
}

pub struct FunctionSig {                     // the de-duplicated function signature
    pub params: Vec<(LangType, String)>,
    pub return_type: LangType,
    pub is_extern: bool,
}

pub struct ModuleSymbols {
    pub functions: HashMap<String, FunctionSig>,
    structs_by_id: Vec<StructInfo>,          // index == id
    structs_by_name: HashMap<String, u32>,
    aliases: HashMap<String, LangType>,      // alias name -> resolved type (Milestone 1)
}
```
Struct API: `intern_name(&str) -> u32` (reserve id with empty body), `struct_id(&str) ->
Option<u32>`, `struct_info(id) -> &StructInfo`, `struct_info_mut(id)`, `set_fields(id,
Vec<FieldInfo>)`, `field(id, &str) -> Option<(usize, &FieldInfo)>`, `define_alias(name, LangType)`,
`resolve_alias(&str) -> Option<LangType>`, `add_method` (M3). Function API: `add_function(name,
FunctionSig)`, `function(&str) -> Option<&FunctionSig>`. Derives `Clone`/`PartialEq` for `Program`.

**Phase wiring (the de-duplication):**
- *Parser* builds `ModuleSymbols` during parsing (it already builds `SymbolTable.functions`). Its
  transient per-function **variable** scope (`ScopeStack<VarSymbol>`) stays parse-only and is *not*
  put on `Program`. At `parse_program` end, `program.symbols = self.module`.
- *Typechecker* drops its private `functions: HashMap<String, FunctionSig>` field and reads
  `program.symbols.functions` (it already takes `&mut Program`). `register_declarations` no longer
  rebuilds the function map; it validates against the shared one.
- *Codegen* keeps its `functions: HashMap<String, FunctionValue<'ctx>>` LLVM-handle map (`'ctx`-bound,
  inherently codegen-local) **and** its `function_lang_params` param-type index. The latter is a
  borrow-local cache the call sites genuinely need — `walk_expression` is not threaded the `Program`,
  so it cannot reach `program.symbols` at a call site. It is populated from the function protos
  (identical data to `program.symbols.functions`, single authoritative builder upstream = the parser),
  so it is an index, not a divergent re-derivation. Codegen *reads* `program.symbols` for struct
  layout (from M2). No codegen function-path change in M0 (keeps IR byte-identical).

This removes the existing three-way function-sig duplication *and* gives structs their required
shared home, in one table. IR is unchanged: codegen still declares functions from `program.functions`
protos and coerces args from the same `LangType`s, just read from one place instead of three.

---

## 2. Parser

### `Program` carries the symbol table  (`src/parser/ast.rs`)
- Add `pub symbols: ModuleSymbols` to `Program` (`ast.rs:181`). Update the single `Program { .. }`
  literal at `expressions.rs:817`. `Program` derives `PartialEq` (tests) — `ModuleSymbols` must too.
- `Parser` (`expressions.rs:138`) replaces today's `symbol_table: SymbolTable` with `module:
  ModuleSymbols` (global symbols, moved into `Program` at the end of `parse_program`) plus a
  transient `var_scopes: ScopeStack<VarSymbol>` for during-parse variable resolution (discarded).

### Name-collection prescan
Before the main `do_parse_program` loop (`expressions.rs:782`), scan `self.tokens` once for
`Keyword::Type <Identifier>` and `Keyword::Alias <Identifier>` and `intern_name` each struct name
(reserving its id). This makes struct names resolvable regardless of declaration order and handles
self-reference (`SizedString` methods mention `SizedString`) and mutual reference. Bodies/aliases
are filled during the main parse. The prescan does not consume tokens (it reads `self.tokens`
directly), so it does not interact with the backtracking combinators.

### `parse_type` extension  (`expressions.rs:708`) — shared by aliases and structs
Currently `parse_type` accepts only `TokenKind::LangType(_)`. Extend it to also accept
`TokenKind::Identifier(name)`:
1. If `name` resolves via `self.module.resolve_alias` → return that `LangType` (then apply pointer/
   array modifiers, see below).
2. Else if `self.module.struct_id(name)` → `LangType::new(TypeBase::Struct(id), 0, 0, false)`.
3. Else → `ParserError::UndefinedType(name, pos)` (new variant).

Because the lexer only attaches `*`/`[]` modifiers to *built-in* type tokens (a struct name lexes
as a bare `Identifier`, per `scanner.rs:465` notes), `parse_type` must itself consume trailing
`Asterisk`/`OpenBracket` to build `pointer_depth`/`array_size` for named types. Factor a small
`apply_type_modifiers(base: LangType) -> LangType` used after resolving an identifier type.

`lang_type!()` (macro at `expand.rs:78`) expands to `self.parse_type()?`, so this single change
propagates to params, return types, var decls, casts, and alloc.

### Top-level items  (`do_parse_program`, `expressions.rs:782`)
Add branches before the `LangType` global-var branch:
- `kw_if!(Alias)` / `check_keyword(Type)`-style guards. Disallow when `is_extern`.
- `parse_type_alias()` → `alias NewName TargetType` → `self.module.define_alias(name, ty)`.
- `parse_struct_def()` → `type Name { ... }` → fill the reserved id's fields (and methods in M3).
- Extend `synchronize()` (`expressions.rs:179`) to resync on `Type`/`Alias` for top-level recovery.

### New AST nodes  (`ast.rs`)
- `ExprKind::FieldAccess { base: Box<Expression>, field: String }`.
- `ExprKind::StructLiteral { struct_id: u32, fields: Vec<(String, Expression)> }`.
- `StatementKind::FieldAssign { target: Expression, value: Expression }` (target is a `FieldAccess`),
  mirroring the existing `DerefAssign { target, value }`.
- Method-call surface (`obj.m(args)`, `Type.m(args)`) is desugared into `FunctionCall` with a
  mangled name at parse time (M3), so it needs no new `ExprKind`.

### Field access & struct literals (parser)
- `parse_postfix` (`expressions.rs:512`): add a `TokenKind::Dot` arm in the loop → `parse_field_access`
  building `FieldAccess`. This composes with the existing `(` / `[` postfix handlers so `a.b.c`,
  `a.b[i]`, `a.b()` chain naturally.
- Struct literal: in `parse_primary` / `variable_reference` (`expressions.rs:617`), when an
  `Identifier` resolves (via `self.module.struct_id`) to a struct id and is immediately followed by
  `OpenBrace`, parse `Name { field = expr, ... }` into `StructLiteral`. Reuse the brace/`skip_nl!`
  handling from `parse_init_list` (`expressions.rs:927`). (Outside a known type name, `{` stays a
  block.)

### Errors  (`src/parser/errors.rs`)
- New variants: `UndefinedType(String, Position)`, `DuplicateType(String, Position)`,
  `UnknownField(String, String, Position)`. Add each to `position()` (`errors.rs:65`).
- If `SymbolError` grows a type-duplicate variant, extend `from_symbol` (`errors.rs:51`,
  exhaustive match).

---

## 3. Codegen  (`src/codegen/*`)

### Caches on `CodeGenerator`  (`generator.rs:18`)
- `struct_types: HashMap<u32, inkwell::types::StructType<'ctx>>` (the named LLVM struct per id).
- The field-name → index map already lives in `program.symbols` (the registry), so no separate
  layout cache is needed; codegen reads `program.symbols` (passed to `generate`).

### Registration pass  (`generate`, `generator.rs:93`)
Add a pass **before** `declare_function`: for each struct id, `context.opaque_struct_type(name)`
then `.set_body(&field_llvm_types, false)` (non-packed → C layout). opaque-then-body makes
pointer-field self/mutual references safe. Store in `struct_types`.

### Type lowering  (`types.rs`)
`LangTypeExt::to_llvm` (`types.rs:72`) cannot see the cache (it's `&self` on `LangType`). Add a
`CodeGenerator::lang_type_to_llvm(&self, ty) -> BasicTypeEnum` that returns the cached `StructType`
for `TypeBase::Struct(id)` with `pointer_depth == 0`, and otherwise delegates to `to_llvm`. Pointer-
to-struct stays opaque `ptr` (the existing pointer/array decay at `types.rs:73` already handles it).
Route struct-capable positions (params, returns, var allocas, GEP pointee) through this method.

### The lvalue path — `emit_address`  (`expressions.rs`, the keystone)
There is no address-emitting path today; add:
```rust
fn emit_address(&mut self, expr: &Expression) -> Result<(PointerValue<'ctx>, LangType)>
```
handling `Variable` (→ `scope.lookup_any(name).ptr()`), `Dereference(p)` (→ value of inner pointer),
and `FieldAccess { base, field }` (→ recurse `emit_address(base)`; if `base` is a *pointer-to-struct*,
load/use the pointer; `build_struct_gep(struct_ty, base_ptr, index, field)` → field ptr). Then:
- Field-access **value** arm in `walk_expression`: `emit_address` + `build_load` (mirroring the
  `Variable` arm at `expressions.rs:148`; decay arrays/structs to their pointer like that arm does).
- `ExprKind::Reference` (`expressions.rs:350`) delegates to `emit_address` so `&s.field` works.
- `FieldAssign` statement uses `emit_address(target)` + coerced `build_store`.

### Struct literals & var decls
- `StructLiteral` arm (Runtime mode only; reject in `Constant`): `build_alloca(struct_ty)`, then per
  field `build_struct_gep` + `generate_coerced_value(expr, Some(field_ty))` + `build_store`; yield
  the alloca pointer (structs travel by pointer).
- `generate_var_decl` (`statements.rs:~110`) gets a struct branch alongside the existing `is_array`
  aggregate branch: alloca the struct and initialize from the literal/initializer.

### Functions — sret & by-pointer  (`functions.rs`)
- `declare_function` (`functions.rs:56`): struct **return** → LLVM return becomes `void`, prepend a
  hidden `ptr` param with the `sret(<StructTy>)` type-attribute
  (`create_type_attribute(get_named_enum_kind_id("sret"), struct_ty)`,
  `add_attribute(AttributeLoc::Param(0), ..)`). Struct **value param** → lower to `ptr` with
  `byval(<StructTy>)` (copy semantics). `this` → plain `ptr` (no `byval`; mutation is intended).
- `generate_function` prologue (`functions.rs:122`): param indices shift by 1 when sret is present.
- `generate_return` (`statements.rs:211`): with sret, `build_store` the value into the sret pointer
  and `build_return(None)`.
- Call sites (`generate_function_call`, `functions.rs:162`; `generate_call_args`, `functions.rs:35`):
  for an sret callee, alloca a result slot, pass its pointer as arg0, the call "value" is that slot.
  For struct args, pass the address (the `function_lang_params` map already records param types).
- Guard: `extern` functions with by-value struct params/returns → error (deferred ABI).

### Constant / ValueEmitter guards  (`value_emitter.rs`, `expressions.rs`)
Structs are aggregates and never flow through `ValueEmitter`. Reject struct literals/field access in
the `EmitMode::Constant` arm (mirror existing constant guards at `expressions.rs:223/297/378/407`),
and guard struct types out of `emit_cast` paths.

---

## 4. Typechecker  (`src/typechecker/*`)

- `TypeChecker` reads `program.symbols` (no new owned copy needed; it already takes `&mut Program`).
  Optionally finalize per-struct data here.
- `register_declarations` (`checker.rs:109`): validate each struct's field types resolve (no
  undefined/recursive-by-value cycles — a struct may only contain *pointers* to itself/another, not
  a by-value instance of an incomplete type).
- Numeric guards: add explicit struct arms rejecting arithmetic/widening/casts in
  `binary_op_types_valid` (`checker.rs:688`), `wider_type` (`checker.rs:711`), and in
  `types.rs` `types_coercible`/`cast_valid`/`literal_int_fits`/`literal_float_compatible`. Structs
  coerce only to the identical struct id; no numeric interplay.
- Field access: arms in `synth_expression` (`checker.rs:329`) and `check_expression` (`checker.rs:528`)
  — synth the base, require struct (or pointer-to-struct), look up the field in `program.symbols`,
  return/stamp the field's `LangType`. Enforce visibility: a private field is accessible only when
  `current_function` is one of the struct's own methods (M4).
- Struct literal: check field-name coverage (all fields), per-field `check_expression` against the
  declared field type, and the visibility rule from §0.5 (inaccessible field → error, not default).
- Field assignment: const enforcement via the existing `AssignmentToConst` path (`checker.rs:207`).
- New error variants in `errors.rs`: `UnknownStructType`, `NotAStruct`, `UnknownField`,
  `MissingFields`, `InaccessibleField`, each with a `Position`; register in `position()`
  (`errors.rs:99`). Add a `type_name(&self, ty) -> String` helper that pretty-prints struct types
  by consulting `program.symbols` (since `Display` can't), used when building these messages.

---

## 5. Milestones (each compiles + tests green before the next)

- **M0 — Foundation + symbol-table unification. ✅ DONE (IR byte-identical, all tests green).**
  `TypeBase::Struct(u32)` + `Display` arm; the
  unified `ModuleSymbols` table (`src/symbol/module.rs`, owns functions + struct registry + aliases)
  + `symbol/mod.rs` wiring; `symbols` field on `Program` + the one literal; move function registration
  out of the parser's `SymbolTable` (now variables-only) into `self.module`; **delete the
  typechecker's private `FunctionSig`/`functions` rebuild and read `program.symbols.functions`**
  (via take-on-entry / restore-on-exit so the typechecker can later finalize the registry without a
  divergent copy); numeric-helper guards for `TypeBase::Struct`. Codegen's function path is unchanged
  (its `function_lang_params` is a borrow-local index, see §1). No struct/alias syntax yet — the
  table holds only functions, so all existing IR/tests stay byte-identical.
- **M1 — Aliases. ✅ DONE (corpus IR byte-identical; `tests/programs/aliases.ap` +
  `failures/parser_undefined_type.ap`).** `Keyword::Alias`; name-collection prescan; `parse_type`
  identifier resolution + `apply_type_modifiers` (pointer `*` on named types); `parse_type_alias`;
  alias table; statement dispatcher recognizes named-type locals (`Parser::starts_named_var_decl`);
  global vars of named type; top-level `alias` branch + `synchronize` resync. Introduced the
  `parse_type` identifier resolution that structs also need.
- **M2 — POD structs (pointer-based). ✅ DONE (corpus IR byte-identical; `tests/programs/structs.ap`,
  `struct_copy.ap` + 3 failure tests + 3 checker unit tests).** `type Name { [public] T field }`;
  `ExprKind::FieldAccess`/`StructLiteral`, `StatementKind::FieldAssign`; `Keyword::Public`; the
  `Dot` postfix + struct-literal detection in `parse_primary`; `src/codegen/structs.rs` with the
  registration pass (`opaque_struct_type` + `set_body`), `lang_type_to_llvm`, `struct_field`, and
  the `emit_address` lvalue keystone (Variable / Dereference / FieldAccess, with auto-deref of
  single-level pointer-to-struct); struct branch in `generate_var_decl`; `&s.field`; struct values
  as first-class aggregates (insertvalue literals, load/store copy). Field access works on struct
  values **and** pointer-to-struct, so structs pass to functions by pointer (`fn f(Point* p)`).
- **M2b — Struct by-value ABI (sret/byval). ✅ DONE (corpus IR byte-identical;
  `tests/programs/struct_byvalue.ap`, `struct_rvalue_field.ap`).** Struct *value* returns lower
  to a hidden `sret(%S)` out-pointer (callee stores through it, returns void; caller allocas a slot,
  passes it, loads the result); struct *value* params lower to `byval(%S)` (caller spills the value
  to a temp and passes its address; callee uses the incoming pointer as the variable's storage).
  Added `function_return_types` + `current_sret` to `CodeGenerator`, a unified `build_abi_call`, and
  rvalue-struct field access (materialise to a temp slot). Per-target by-value *across the C/extern
  boundary* (System V/Win64 register classification) remains the separate future TODO.
- **M3 — Methods + `this`. ✅ DONE (corpus IR byte-identical; `tests/programs/methods.ap`,
  `method_chain.ap`, `failures/type_const_fn_writes.ap`).** Methods inside a `type` body
  desugar to free functions named `Type$method`. An instance method takes a bare `this` receiver
  (no type annotation); the parser supplies it as an implicit `*Struct` first param.
  `const fn` makes the receiver `*const Struct`, and field access propagates that const through
  `resolve_field` so `this.field = ...` lands on the existing `AssignmentToConst` path. Method
  calls `obj.method(args)` desugar to a `FunctionCall` with the mangled name and the receiver
  prepended; value receivers are auto-referenced via `Reference`. Static calls `Type.method(...)`
  use the mangled name with no receiver. The codegen Reference arm spills rvalue struct receivers
  (e.g. `make(...).m()`) to a temp slot so the address can be taken. Also fixed a lexer bug where
  `scan_type_after_const` consumed the trailing identifier on failure (so `const fn` lexed wrong).
  An additional Reference check-mode rule now allows a const-pointer-to-non-const (`const T* p =
  &t`), needed so `&mutable_struct` coerces to a const-fn's `*const Struct` receiver.
- **M4 — Encapsulation + `const fn` enforcement. ✅ DONE
  (`tests/programs/encapsulation.ap` + `failures/type_private_field_read.ap`,
  `type_private_field_literal.ap`).** `TypeCheckError::InaccessibleField` plus
  `is_inside_struct_methods(id)` on the checker (matches `current_function` against the mangled
  prefix `"<TypeName>$"`). `resolve_field` and the struct-literal check each enforce visibility.
  Together with the existing "must name every field" rule, this makes a type-struct with any
  private field unconstructible by an external literal, forcing the factory-method pattern
  (the locked design from §0.5). `const fn` enforcement reuses the const-propagation in
  `resolve_field`: a `*const Struct` receiver makes `this.field` const, so any mutation lands on
  the existing `AssignmentToConst` path.

**Future (separate TODO):** System V + Windows x64 by-value aggregate ABI, so structs can cross the
`extern`/C boundary by value (small-struct-in-registers, sret>16 on SysV; ≤8-byte-in-register vs
by-ref on Win64). Until then, `extern` by-value struct params/returns are rejected.

---

## 6. Invariants to preserve
- `LangType` stays `Copy`/`Eq` (the `u32` id guarantees this).
- Existing corpus IR stays byte-identical until a struct is actually used (semantics bar = test exit
  codes). M0/M1 must not perturb any existing program's IR.
- New `match` arms over `TypeBase`/`ExprKind`/`StatementKind` are exhaustive (no wildcards added that
  would hide a missing case) — the compiler must keep flagging unhandled variants.
