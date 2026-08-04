# Aspect Compiler — Consolidated Refactor Plan

Synthesized from an independent clean-room review of the codebase.

**Status: executed in full, 2026-08-04** — all 11 batches landed, suite green throughout. Grouped by module/file-cluster; ordered so `cargo test` stays green after every commit. Each item lists concrete mechanical steps, required corrections (things that won't compile as originally proposed), effort, and dependencies.

**Effort legend:** S = under an hour, mechanical, one function. M = a few hours, multiple functions/one new file. L = half a day+, cross-file signature changes or a real design decision embedded in the mechanics.

---

## 1. Parser — `src/parser/expressions.rs` cluster (visibility → types → dot-access)

These four items all touch `src/parser/expressions.rs` and `src/parser/mod.rs`, and have a real dependency chain (later items call helpers the earlier items promote to `pub(crate)`). **Must be done in this order, as a single work stream — one commit each, `cargo test` between.**

### 1.1 `visibility.rs` extraction — M
Consolidate `check_struct_visibility` / `check_sum_visibility` / `check_enum_visibility` (expressions.rs:200-241) plus `module_of_file`, `check_import_visibility`, `check_name_visibility`.

- In `src/parser/errors.rs`: tighten `private_type`/`private_sum`/`private_enum`'s `name` param from `impl Into<String>` to `&str` (all call sites already pass `&str`; this is a real, contained API change — required because a generic fn item cannot coerce to a `fn(&str,&str,&str,Position)->ParserError` pointer).
- Add `check_item_visibility(&self, kind: &'static str, name: &str, file_id: u32, vis: Visibility, use_pos: Position, private_err: fn(&str,&str,&str,Position)->ParserError) -> Result<(), ParserError>`; reduce the three wrappers to one-liners each.
- Move the whole cluster (`module_of_file`, `check_import_visibility`, `check_item_visibility`, `check_struct/sum/enum/name_visibility`) + their `#[cfg(test)] mod tests` block (currently at the end of expressions.rs) into new `src/parser/visibility.rs`. Add `pub mod visibility;` to `parser/mod.rs`.
- **Mark every moved method `pub(crate)`** — expressions.rs keeps calling several of these (struct/enum/sum-literal parsing) so this is not optional.
- Name the new consolidation helper `check_item_visibility`, not anything resembling `check_name_visibility` (near-homonym risk).

### 1.2 `type_expr.rs` extraction — M
Move `parse_type`, `apply_type_modifiers`, `starts_named_var_decl`, `starts_fnptr_var_decl`, `starts_grouped_var_decl`, `type_suffix_then_ident` (expressions.rs:1082-1291) into new `src/parser/type_expr.rs`; add `pub mod type_expr;`.

- Extract `parse_type`'s `Identifier` arm into `fn resolve_named_type(&mut self, name: &str, pos: Position) -> Result<LangType, ParserError>` and its `Keyword(Fn)` arm into `fn parse_fnptr_type(&mut self) -> Result<LangType, ParserError>`.
- `resolve_named_type` calls the four visibility checks from **1.1** — since those are already `pub(crate)` in `visibility.rs` by this point, no extra visibility work needed here (this is why 1.1 must land first).
- `starts_named_var_decl`/`starts_fnptr_var_decl`/`starts_grouped_var_decl` are already `pub(crate)` — no change needed (called from declarations.rs/statements.rs/program.rs/asm.rs today).
- Update `doc/compiler/02-parser.md` (file table + parse_type mention), `doc/compiler/08-parser-macro-rewrite.md` (directory listing), and `.claude/skills/aspect-architecture/SKILL.md`'s file map.

### 1.3 `dot_access.rs` extraction — L
Move `parse_dot_postfix` (expressions.rs:1344-1500) **and** `build_method_call` (1505-1622) into new `src/parser/dot_access.rs`; add `pub mod dot_access;`.

- Add `fn unshadowed_named_ref<'a>(&self, base: &'a Expression) -> Option<&'a str>`; apply at **all four** sites: the enum guard, the sum guard, the static-method-value guard in `parse_dot_postfix`, and the static-call guard inside `build_method_call` (line ~1514 — a fourth occurrence the original 3-site survey missed).
- Trial helpers `try_enum_variant_value` / `try_sum_construct` / `try_static_method_ref` return `Option<Result<Expression, ParserError>>`. **`?` does not propagate through this shape** — each internal `self.check_x_visibility(id, pos)?` must become `if let Err(e) = self.check_x_visibility(id, pos) { return Some(Err(e)); }`.
- Split `build_method_call` into:
  - `build_static_method_call(&mut self, id: u32, method_name: &str, args: Vec<Expression>, pos: Position)` — takes `id: u32` (already resolved), not `var_name: &str`.
  - `build_instance_method_call(&mut self, base: Expression, method_name: &str, args: Vec<Expression>, pos: Position)`.
  - `autoref_receiver(base: Expression, pos: Position) -> Result<Expression, ParserError>` — needs `pos` (the call site's position, not `base.pos` — they differ) for its deeper-pointer-depth error. No `&self` needed.
  - Optional polish: `resolve_method_target(&mut self, id: u32, method_name: &str, want_static: bool, pos: Position) -> Result<(String, LangType), ParserError>` to dedup the 5 near-identical lines both static/instance paths share (visibility check, type-name derivation, mirrored-polarity `MethodCallForm` error, mangling, return-type lookup).
- Preserve exact original trial order: method-call check → enum → sum → static-method-value → field-access fallback.
- Mark the dispatcher entry point and `build_method_call`'s replacements `pub(crate)` (still called from `parse_postfix` in expressions.rs).
- Add `pub mod dot_access;` to `parser/mod.rs`.

### 1.4 `parse_unary` split — S (low priority, optional, can be folded into 1.3's session or skipped)
Extract each of the 5 arms (691-804) into `parse_reference`/`parse_dereference`/`parse_negation`/`parse_logical_not`/`parse_bitwise_not`, keeping `parse_negation`'s literal-fold + `0 - expr` fallback together. No cross-cutting duplication — purely a readability win. Can stay in expressions.rs or move alongside `parse_cast`/`parse_postfix`.

---

## 2. Parser — `src/parser/program.rs` / `declarations.rs`

Two items, same two files, one work stream, sequential.

### 2.1 `reject_extern` + `parse_top_level_item` — M
- Extract `fn reject_extern(is_extern: bool, pos: Position) -> Result<(), ParserError>` for the 5 identical "extern can only be used with functions" blocks in `do_parse_program` (program.rs:121-126, 130-134, 139-143, 147-151, 164-168). **Add a compile-failure fixture** (`tests/programs/failures/parser_extern_alias.ap` or similar) — this diagnostic currently has zero test coverage.
- Widen scope beyond the original ask: move lines ~66-177 (not just 109-177) — the `defines_a_fn`/`defines_a_type`/`defines_a_global` classification, the public/export validity checks, and the dispatch chain — into `declarations.rs` as `pub(crate) fn parse_top_level_item(&mut self, vis: Visibility, export: bool, is_extern: bool, kind: Option<(Keyword, Position)>, vis_pos: Position) -> Result<TopLevelItem, ParserError>` with `enum TopLevelItem { Fns(Vec<Function>), Global(GlobalVar), None }`. This is what actually eliminates the double-tested predicates rather than relocating them.
- Bump `parse_type_alias` to `pub(crate)` (currently private in program.rs; the moved Alias branch needs it from declarations.rs).
- `do_parse_program`'s loop shrinks to modifier parsing + one call + a 3-arm match on `TopLevelItem`.

### 2.2 `cycles.rs` extraction — S
Move `Node`, `edges`, `visit` out of `check_byvalue_containment_cycles` (declarations.rs:291-380) into `src/parser/cycles.rs`; add `pub(crate) mod cycles;`.

- Fold root construction into the entry point: `pub(crate) fn find_byvalue_cycles(module: &ModuleSymbols) -> Vec<Node>` (builds roots from `module.structs()`/`module.sums()` internally — don't leave root-building behind in declarations.rs, it's equally module-agnostic).
- `check_byvalue_containment_cycles` shrinks to ~15 lines: call + translate each `Node` to `(name, pos)` via `struct_decl_pos`/`sum_decl_pos` + push `ParserError::RecursiveByValue`.
- This was empirically verified end-to-end (implemented, `cargo check`/`cargo test` clean, reverted) — genuinely zero-risk. Add unit tests in `cycles.rs` while there (self-cycle, mutual struct/sum cycle, pointer-breaks-cycle) since testability was the whole point.

---

## 3. Parser — `src/parser/statements.rs` switch machinery — L

Independent file from §1/§2 — can run in parallel with those, not with itself.

Move `parse_switch_statement`, `parse_switch_arm`, `parse_switch_pattern`, `switch_coverage_complete`, `case_outside_switch` (statements.rs:419-711, 41% of the file) into new `src/parser/switch.rs`; add `pub mod switch;`.

- Mark `parse_switch_statement` and `case_outside_switch` `pub(crate)` (STATEMENT_TABLE in statements.rs references them by fn-pointer path — this breaks silently otherwise since the new file is a sibling module, not a descendant).
- Copy the **full** required `use` set, not just `SwitchArm`/`SwitchPattern`: also `Keyword, LangType, Position, TokenKind, TypeBase` (lexer), `ExprKind, Expression, ParserError, Statement, StatementKind` (parser), `aspect_macros::parse_rule`.
- Split `parse_switch_pattern` into `parse_sum_variant_pattern(&mut self, sum_id: u32, sum_name: &str) -> Result<SwitchPattern, ParserError>` and `parse_enum_variant_pattern(&mut self, enum_id: u32) -> Result<Option<SwitchPattern>, ParserError>` (`None` = fall through to field access). **Copy each branch's guard exactly** — the sum branch checks `!s_ty.is_array()`, the enum branch does not; don't "normalize" this asymmetry away.
- Replace the 3 `HashSet`-cardinality blocks in `switch_coverage_complete` (Sum/Enum/Bool) with one `fn covers_all<T: Eq + Hash>(seen: impl Iterator<Item = T>, total: usize) -> bool`.
- Run `cargo test` — good existing coverage (`switch.ap`, `switch_dead_default.ap`, `sum_match.ap`, ~9 failure fixtures).

---

## 4. Codegen — sum/switch/struct-literal cluster (`expressions.rs`, `structs.rs`, `sums.rs`, `statements.rs`)

All five sub-items touch overlapping files in `src/codegen/`. **One work stream, five commits, in this exact order** — later steps depend on earlier ones structurally (the sum-payload dedup helper built in 4.4 is what 4.5's `bind_switch_arm_payload` calls).

### 4.1 Leaf-helper extraction in `expressions.rs` — S
Extract `emit_variable_load`, `emit_comparison`, `emit_reference`, `emit_unary_not` from `walk_expression`'s Variable/Comparison/Reference/UnaryNot arms (206-256, 285-318, 320-358, 394-418), matching the file's existing `emit_pointer_arithmetic`/`emit_short_circuit`/`emit_sum_probe` style (plain `fn(cg: &mut CodeGenerator<'ctx>, ...)`, not `&dyn ValueEmitter`).
- **Signature correction**: `emit_comparison`'s `op` parameter is `&ComparisonOp`, not `&BinaryOp` as first drafted — `ExprKind::Comparison` uses a distinct enum.
- Have `emit_comparison` do its own `walk_expression` calls internally (Comparison evaluates both operands unconditionally — no need to hoist them like `emit_pointer_arithmetic` does) so the match arm is a true one-liner.
- Carry over both why-comments (the `!range` bool-metadata rationale on Variable, the rvalue-materialization rationale on Reference's fallback).
- Verified via full implement/test/revert cycle — safe.

### 4.2 `StructLiteral` → `structs.rs` — S
Move the `ExprKind::StructLiteral` arm (expressions.rs, 33 lines) into `structs.rs`'s existing `impl<'ctx> CodeGenerator<'ctx>` block as `pub(crate) fn emit_struct_literal(&mut self, struct_id: u32, fields: &[(String, Expression)], pos: Position) -> Result<BasicValueEnum<'ctx>, CodegenError>` — **an inherent method, not a free function** (the proposed dispatch `cg.emit_struct_literal(...)` requires method syntax).
- Adjust the two `HashMap` accesses for the owned-`u32` shift: `self.struct_types.get(&struct_id)`, `self.struct_field(struct_id, fname)` (drop the `*`).
- Add `BasicValueEnum` to structs.rs's inkwell import. Keep both TODO comments verbatim. Leave const_eval.rs's separate StructLiteral arm untouched.

### 4.3 Break/Continue/value-block dead-end helper — S
Extract `fn branch_to_dead_end(&mut self, target: BasicBlock<'ctx>, dead_label: &str) -> Result<(), CodegenError>` (build_unconditional_branch + append_basic_block + position_at_end) used identically at Break (50-60), Continue (61-74), and generate_return's value-block path (258-275) in statements.rs.
- Add `use inkwell::basic_block::BasicBlock;`. Mark `pub(crate)` for house-style consistency. Add a one-line why-comment (LLVM needs a valid insert point after a terminator even for unreachable code).
- Verified safe under two-phase borrows (the `&BasicBlock<'ctx>` read from `self.loop_stack.last()` ends before the `&mut self` helper call activates); `BasicBlock` is `Copy`.
- Independent of 4.4/4.5's switch work (different lines) but same file — do it here to keep the diff small before the switch-heavy commits land.

### 4.4 `SumConstruct`/`Is`/`IsBinding` → `sums.rs`, widened 3-way payload dedup — M/L
Move `emit_sum_probe` (148-177), and new `emit_sum_construct`/`emit_is_binding` (bodies of the SumConstruct/IsBinding arms, 477-536 and 547-621) from expressions.rs into `sums.rs`, next to `sum_payload_type`/`sum_field_llvm_types`.
- Convert `emit_sum_probe` to a `pub(crate) fn(&mut self, ...)` method (currently a private free fn — the thin `Is` arm staying in expressions.rs needs it across the module boundary).
- **Widen the dedup beyond the original 2-way finding**: factor `fn copy_sum_payload_field(&mut self, dst: PointerValue<'ctx>, src: PointerValue<'ctx>, field_ty: LangType, pos: Position) -> Result<(), CodegenError>` covering **both** the array leg (align lookup + `build_memcpy`) and the scalar leg (`build_load`+`build_store`). Use it from `emit_sum_construct`, `emit_is_binding`, **and** `generate_switch`'s binder-copy loop in `statements.rs` (statements.rs ~427-464) — this is a genuine 3-way duplication, not 2-way; statements.rs already reaches into `sums.rs` internals today so this is no new precedent.
- Update `sums.rs`'s module `//!` doc comment to state the widened scope (storage layout *and* construction/pattern-match codegen) — and note the resulting asymmetry with `StructLiteral` (deliberately staying in `structs.rs`/inline) so a future reader doesn't "fix" it reflexively.
- Needed imports: `BasicValueEnum`, `IntValue`, `PointerValue` (inkwell), `crate::parser::Expression`. Watch the `&u32` → owned-`u32` HashMap-index fixups (`cg.sum_variant_fields[&sum_id]` etc.).

### 4.5 `generate_switch` → `switch.rs` (codegen) — M
New `src/codegen/switch.rs` (add `pub mod switch;` to codegen/mod.rs), mirroring `sums.rs`/`structs.rs` granularity:
- `switch_discriminant(&mut self, scrutinee, function, pos) -> Result<(IntValue, Option<(PointerValue,u32)>), CodegenError>` (326-359 verbatim).
- `switch_case_value(&self, pattern: &SwitchPattern, disc_ty: IntType) -> Result<IntValue, CodegenError>` (377-401 verbatim — **drop the `pos` param**, it's unused; the one fallible arm uses the pattern's own `e.pos`).
- `bind_switch_arm_payload(&mut self, function, sum_slot, variant, binders, pos)` (417-464's loop body, now calling **4.4's** `copy_sum_payload_field` instead of hand-rolling array/scalar copy).
- `emit_switch_else(&mut self, else_bb, default, merge_bb, pos)` (480-506 verbatim).
- Replace both hand-rolled terminator checks (470-477, 487-493) with the existing `self.block_has_terminator()` helper already used 4 other places in this file.
- Realistic result is ~55-70 lines of orchestrator, not ~30 — if a tighter orchestrator is wanted, add two more extractions (`build_switch_cases`, `generate_switch_arm`) on top.

---

## 5. Codegen — `src/codegen/const_eval.rs` — M
Independent file, parallelizable with §4/§6/§7.

Extract `const_eval_sum_construct` (209-257) and `const_eval_struct_literal` (265-301) from the `const_eval` match (23-338) — these are the genuinely tangled arms (padding-byte computation, layout lookup, sum-type rejection). `const_eval_binary`/`const_eval_reference` (93-114, 121-139) are optional/for-consistency only, not required.
- **Keep `sum_id`/`variant`/`struct_id` as `&u32` in the new signatures** (not owned `u32`) so the body pastes verbatim — avoids needing `&`-fixups at every `HashMap::get`/index call inside.
- Move the doc comment above the current SumConstruct arm (padded-struct layout rationale) onto `const_eval_sum_construct`.

---

## 6. Codegen — `src/codegen/value_emitter.rs` `emit_cast` — M
Independent file, parallelizable with §4/§5/§7.

Split `RuntimeEmitter::emit_cast` (257-364) and `ConstantEmitter::emit_cast` (524-653) into small private free functions.
- **RuntimeEmitter (5 helpers, actual branch count)**: `runtime_cast_to_pointer`, `runtime_cast_int_to_float`, `runtime_cast_float_to_int`, `runtime_cast_ptr_to_int`, `runtime_cast_int_resize`. Dispatcher's fallback stays `Ok(value)` verbatim — **do not add a runtime float-resize helper**; RuntimeEmitter has no `target_is_float && value.is_float_value()` branch today, which is a real pre-existing type-punning bug (confirmed: `f32→f64` cast stores the f32 bit pattern into an f64 slot untouched). Preserve the gap in this refactor; file it separately with its own corpus test.
- **ConstantEmitter (7 helpers, not 6 — original proposal omitted float↔float)**: `const_cast_int_resize`, `const_cast_int_to_float`, `const_cast_float_to_int`, `const_cast_float_resize` (**new, required — omitting it silently drops f32↔f64 const-folding**), `const_cast_ptr_to_ptr`, `const_cast_int_to_ptr`, `const_cast_ptr_to_int`. Dispatcher keeps its own `Err(CodegenError::InvalidOperation(...))` fallback.
- Don't force signature symmetry between the two families — Runtime's never need `pos` (no error paths), Constant's do.

---

## 7. Codegen — `src/codegen/globals.rs` `generate_global_variable` — S
Independent file, parallelizable with §4/§5/§6.

- Extract `fn global_linkage(export: bool) -> Linkage` replacing both if/else linkage decisions (57-61, 89-93) — note: it's two if/else blocks, not "a match and an if/else" as first described.
- Extract `fn register_global(&mut self, name: String, ptr: PointerValue<'ctx>, llvm_type: BasicTypeEnum<'ctx>, lang_type: LangType)` wrapping `scope.insert_global(...)` — **take owned `String`, not `&str`** (every call site already owns one via `global.name.clone()`; `&str` would force a needless extra allocation inside).
- Optional bonus: `generate_string_literal` (149-156) builds the identical `GlobalVarInfo`→`insert_global` shape and could route through `register_global` too (3 call sites unified instead of 2).

---

## 8. Typechecker — `src/typechecker/checker/expressions.rs` — L
Independent file from §9, parallelizable with it.

### 8.1 `synth_expression` split
Extract into new `aggregates.rs` and `calls.rs` (add `mod aggregates; mod calls;` to `checker/mod.rs` or wherever the parent declares submodules):
- `calls.rs`: `check_call`, `synth_indirect_call`.
- `aggregates.rs`: `synth_struct_literal`, `synth_sum_construct`.
- **Bump to `pub(crate)`**: `check_call`, `synth_struct_literal`, `synth_sum_construct`, `synth_indirect_call`, `is_inside_struct_methods` (the last stays in expressions.rs but now needs cross-file callers in the new files).
- Dedup `check_call`'s and `synth_indirect_call`'s identical arity-check-then-zip loop into `fn check_call_args(&mut self, params: impl ExactSizeIterator<Item = LangType>, args: &mut [Expression], callee_name_for_error: &str, pos: Position)` — **must be an iterator, not `&[LangType]`**, since `check_call`'s source is `Vec<(LangType, String)>` and `IndirectCall`'s is `Vec<LangType>`; `check_call` passes `sig.params.iter().map(|(t,_)| *t)`.

### 8.2 `check_expression` split (stays in expressions.rs) — S
Extract `check_binary_numeric(&mut self, left, op, right, target, pos)` — call site is genuinely **two lines** (`self.check_binary_numeric(...); expr.expr_type = *target;`), not one, since the helper has no access to `expr.expr_type` to stamp it — and `check_reference(&mut self, inner, target, expr_type: LangType, pos)`, a true one-liner. Place both near `binary_op_types_valid`/`comparison_operands_valid`. Move both arms' why-comments (opaque-`u0*` rationale, const-pointer rationale) into doc comments on the new functions. Verified via full implement/test/revert — clean.

---

## 9. Typechecker — `src/typechecker/checker/statements.rs` — L
Independent file from §8, parallelizable with it.

### 9.1 `check_switch` → new `switch.rs` — L
Move `check_switch` (287-457, already carrying `#[allow(clippy::too_many_lines)]`) into `checker::switch`, plus `finish_variant_stances`, and extract `Class` (function-local enum) to module scope, `classify_scrutinee`, `check_const_pattern`, `check_sum_variant_pattern`.
- **Bump `check_switch` to `pub(crate)`** (private today, called only from `check_statement` in the same file — `checker::switch` is a sibling module).
- `check_switch_exhaustiveness`'s proposed signature has 8 total params (incl. `self`) — this **triggers `clippy::too_many_arguments`** (empirically confirmed: threshold is 8). Bundle `seen_consts: &HashSet<i64>` and `seen_variants: &HashSet<u32>` into one tuple/struct param to drop to 7, or accept a fresh scoped `#[allow]`.
- **Decide and document diagnostic-order**: the natural two-full-passes shape (all-arms-patterns then all-arms-bodies) changes emitted diagnostic order from today's per-arm interleaving. This doesn't break any current test (error-string assertions are order-independent), but it's a real behavior change to `aspc compile` output — either keep a single per-arm loop to preserve exact order, or call out the reordering explicitly in the commit message.

### 9.2 `Return` arm → `check_return` — S
Extract the `Return` arm (81-117) verbatim into `fn check_return(&mut self, opt_expr: &mut Option<Expression>, stmt_pos: Position)`. Preserve the existing "value-block vs function return" comment as a `///` doc comment on the new method (matches the precedent `check_switch` already sets).

---

## 10. Preprocessor — `src/preprocessor/mod.rs` — M
Independent file.

Dedup the `ScopedDefines`-construction snippet duplicated between `process_tokens`'s ordinary-token arm (349-379) and `process_directive_line`'s `$if`/`$ifdef` handling (417-429).

- **Must be an associated function taking explicit borrows, not a `&self` method** — a `fn current_scope(&self) -> ScopedDefines<'_>` ties the return value's lifetime to the whole `&self`, causing E0502 at both call sites (confirmed by compiling both shapes):
  ```rust
  fn scope_for<'a>(defines: &'a DefineTable, file_modules: &'a [Option<String>], ctx: &'a FileContext) -> ScopedDefines<'a>
  ```
- Call sites fetch `ctx` via `self.file_stack.last()` locally, then `Self::scope_for(&self.defines, &self.file_modules, ctx)` — this keeps the borrow as a direct field-path, disjoint from `self.tokens`/`self.conditionals`.
- Optionally also extract `fn emit_ordinary_token(&mut self, token: &Token, brace_depth: &mut usize)` for the rest of that match arm (brace-depth bookkeeping + `ctx.saw_content = true`).
- Verified via implement/test/revert (103 preprocessor unit tests pass unchanged).

---

## 11. Lexer — `src/lexer/scanner.rs` `scan_token` — S
Independent file.

Add `fn assign_or(&mut self, plain: TokenKind, compound: TokenKind) -> TokenKind` (`if self.match_char('=') { compound } else { plain }`).
- Apply to `+`, `*`, `/`, `%`, `^`, and the tails of `&`, `|` (original scope) — **and extend to `<`, `>`, `-`** (the amendment): their post-first-lookahead tails are the identical shape one level deeper (e.g. `<`'s else-branch is `assign_or(Less, LessEqual)`). This makes 10 of 12 two/three-char arms consistent instead of arbitrarily stopping at 7.
- Leave `=` and `!` untouched — `Equal`/`NotEqual` aren't assignment forms; routing them through `assign_or` would be a misnomer despite matching code shape.

---

## Honorable mentions (low priority, not detailed — fold in opportunistically, no dedicated batch)

- `parser/expressions.rs::parse_unary` split — folded into §1.4 above.
- `parser/statements.rs` + `parser/program.rs`: unify the 4x-duplicated `parse_block_statement()` + `unreachable!()` unwrap into `Self::unwrap_block(stmt: Statement) -> Vec<Statement>`.
- `codegen/value_emitter.rs::ConstantEmitter`: extract `require_zext`/`require_sext`/`require_float_const` wrapping the 8 near-identical `.ok_or_else(...)` constant-extraction blocks.
- `typechecker/types.rs`: extract `opaque_pointer_bridges`, `pointer_pointees_coercible` (from `types_coercible`) and `cast_valid_enum`, `cast_valid_fnptr` (from `cast_valid`) — file is already readable top-to-bottom, mainly a testability nice-to-have.
- `preprocessor/expr_eval.rs::expand_operands`: extract `parse_defined(rest, i, defines) -> Result<(Token, usize), PreprocessError>` from the `defined(NAME)` match arm.
- `symbol/module.rs`: generic `intern_id<T>(by_name, by_id, name, make: impl FnOnce(u32)->T) -> u32` helper to dedup `intern_struct`/`intern_enum`/`intern_sum`.
- `main.rs::compile_file`: extract `emit_ir`/`emit_obj` from the `EmitTarget` match arms.

---

## Batch grouping for parallel delegation

Group by **files touched** — no two concurrent work streams own the same file. Each batch below is internally sequential (single owner, multiple commits, `cargo test` green after each); batches in different rows can proceed fully in parallel.

| Batch | Files owned | Contents (in commit order) | Total effort |
|---|---|---|---|
| **P1** | `src/parser/expressions.rs`, `visibility.rs`(new), `type_expr.rs`(new), `dot_access.rs`(new), `parser/mod.rs` | §1.1 → §1.2 → §1.3 → (§1.4 optional) | L |
| **P2** | `src/parser/program.rs`, `declarations.rs`, `cycles.rs`(new), `parser/mod.rs` | §2.1 → §2.2 | M |
| **P3** | `src/parser/statements.rs`, `switch.rs`(new), `parser/mod.rs` | §3 | L |
| **C1** | `src/codegen/expressions.rs`, `structs.rs`, `sums.rs`, `statements.rs`, `switch.rs`(new), `codegen/mod.rs` | §4.1 → §4.2 → §4.3 → §4.4 → §4.5 | L |
| **C2** | `src/codegen/const_eval.rs` | §5 | M |
| **C3** | `src/codegen/value_emitter.rs` | §6 | M |
| **C4** | `src/codegen/globals.rs` | §7 | S |
| **T1** | `src/typechecker/checker/expressions.rs`, `aggregates.rs`(new), `calls.rs`(new) | §8.1 → §8.2 | L |
| **T2** | `src/typechecker/checker/statements.rs`, `switch.rs`(new) | §9.1 → §9.2 | L |
| **X1** | `src/preprocessor/mod.rs` | §10 | M |
| **X2** | `src/lexer/scanner.rs` | §11 | S |

**All 11 batches (P1-P3, C1-C4, T1-T2, X1-X2) are mutually file-disjoint and can proceed in parallel.** The only shared touch-points are the one-line `pub mod X;` additions to `parser/mod.rs` (P1, P2, P3) and `codegen/mod.rs` (C1 only, no conflict) — trivial, easily-resolved merge conflicts if P1/P2/P3 land concurrently; recommend either serializing just those three `mod.rs` edits at integration time, or having one of the three streams own the `mod.rs` line for all three (added in their respective commits, rebased before merge).

**Recommended grouping if capacity is limited:** run P1, C1, and T1 first (the three highest-effort/highest-interdependency items, each needing sustained context); run P2+P3, C2+C3+C4, T2, X1+X2 as a second wave once the first wave's `mod.rs` touch-points are settled — though strictly nothing blocks all 11 proceeding at once.