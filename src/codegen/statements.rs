use inkwell::basic_block::BasicBlock;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, FunctionValue, IntValue, PointerValue};
use inkwell::IntPredicate;

use crate::codegen::const_eval::const_eval;
use crate::codegen::generator::CodeGenerator;
use crate::codegen::value_emitter::ValueEmitter;
use crate::codegen::CodegenError;
use crate::parser::LangType;
use crate::parser::{ExprKind, Expression, Statement, StatementKind};

impl<'ctx> CodeGenerator<'ctx> {
    pub(crate) fn generate_statement(&mut self, stmt: &Statement) -> Result<(), CodegenError> {
        match &stmt.kind {
            StatementKind::Expression(expr) => self.generate_expression_statement(expr),
            StatementKind::VarDecl {
                var_type,
                name,
                initializer,
            } => self.generate_var_decl(stmt.pos, var_type, name, initializer.as_ref()),
            StatementKind::VarAssign { name, value } => {
                self.generate_var_assign(stmt.pos, name, value)
            }
            StatementKind::DerefAssign { target, value } => {
                self.generate_deref_assign(target, value)
            }
            StatementKind::FieldAssign { target, value } => {
                self.generate_field_assign(target, value)
            }
            StatementKind::Return(expr) => self.generate_return(expr.as_ref(), stmt.pos),
            StatementKind::If {
                condition,
                then_block,
                else_block,
            } => self.generate_if_statement(condition, then_block, else_block.as_deref()),
            StatementKind::While { condition, body } => self.generate_while_loop(condition, body),
            StatementKind::Switch {
                scrutinee,
                arms,
                default,
                ..
            } => self.generate_switch(scrutinee, arms, default.as_deref(), stmt.pos),
            StatementKind::For {
                init,
                condition,
                increment,
                body,
            } => self.generate_for_loop(init.clone(), condition.as_ref(), increment.clone(), body),
            StatementKind::Block(statements) => self.generate_block(statements),
            StatementKind::Break => {
                let (break_bb, _) = *self.loop_stack.last().ok_or_else(|| {
                    CodegenError::InvalidOperation("'break' outside of loop".to_string(), stmt.pos)
                })?;
                self.branch_to_dead_end(break_bb, "break.dead")
            }
            StatementKind::Continue => {
                let (_, continue_bb) = *self.loop_stack.last().ok_or_else(|| {
                    CodegenError::InvalidOperation(
                        "'continue' outside of loop".to_string(),
                        stmt.pos,
                    )
                })?;
                self.branch_to_dead_end(continue_bb, "continue.dead")
            }
        }
    }

    pub(crate) fn generate_expression_statement(
        &mut self,
        expr: &Expression,
    ) -> Result<(), CodegenError> {
        match &expr.kind {
            ExprKind::FunctionCall { name, args } => {
                self.generate_function_call_statement(name, args, expr.pos)
            }
            ExprKind::IndirectCall { callee, args } => {
                // Statement form accepts a void return; the expression form
                // would have errored on MissingReturn instead.
                self.generate_indirect_call_statement(callee, args)
            }
            _ => {
                self.generate_expression(expr)?;
                Ok(())
            }
        }
    }

    /// Allocate `name` of `llvm_type` at the top of `function`'s entry block
    /// (before its first instruction), then restore the builder's insert
    /// position. Entry-block allocas are what let mem2reg promote locals.
    pub(crate) fn build_entry_alloca(
        &self,
        function: FunctionValue<'ctx>,
        llvm_type: BasicTypeEnum<'ctx>,
        name: &str,
        pos: crate::lexer::Position,
    ) -> Result<PointerValue<'ctx>, CodegenError> {
        let entry_block = function
            .get_first_basic_block()
            .ok_or(CodegenError::UnexpectedStatement(pos))?;
        let current_block = self.builder.get_insert_block().unwrap();
        if let Some(first_instr) = entry_block.get_first_instruction() {
            self.builder.position_before(&first_instr);
        } else {
            self.builder.position_at_end(entry_block);
        }
        let alloca = self.builder.build_alloca(llvm_type, name)?;
        self.builder.position_at_end(current_block);
        Ok(alloca)
    }

    /// Branch to `target`, then park the builder in a fresh dead block —
    /// LLVM requires a valid insert point after a terminator even for
    /// statements that can never execute (`break`/`continue`/a value-block
    /// `return`, all of which end the current block early).
    pub(crate) fn branch_to_dead_end(
        &mut self,
        target: BasicBlock<'ctx>,
        dead_label: &str,
    ) -> Result<(), CodegenError> {
        self.builder.build_unconditional_branch(target)?;
        let dead_bb = self
            .context
            .append_basic_block(self.current_function.unwrap(), dead_label);
        self.builder.position_at_end(dead_bb);
        Ok(())
    }

    pub(crate) fn generate_var_decl(
        &mut self,
        pos: crate::lexer::Position,
        var_type: &LangType,
        name: &str,
        initializer: Option<&Expression>,
    ) -> Result<(), CodegenError> {
        let llvm_type = if var_type.is_array() {
            // Cache-aware: resolves type-struct elements (`Pair[2]`) too.
            self.lang_type_to_llvm_array(var_type)
                .map_err(|e| e.with_pos(pos))?
                .into()
        } else {
            self.lang_type_to_llvm(var_type).map_err(|e| e.with_pos(pos))?
        };

        // Allocate in the entry block for mem2reg compatibility.
        let function = self
            .current_function
            .ok_or(CodegenError::UnexpectedStatement(pos))?;
        let alloca = self.build_entry_alloca(function, llvm_type, name, pos)?;

        if var_type.is_array() {
            self.add_variable(name.to_string(), alloca, llvm_type, *var_type, None);
            if let Some(Expression {
                kind: ExprKind::ListInitializer(elements),
                ..
            }) = initializer
            {
                return self.generate_list_initializer(alloca, var_type, elements, pos);
            }
            return Ok(());
        }

        // Fold the initializer to a constant when possible. A `const` var caches
        // the folded value so reads bypass the alloca/load; a non-const var only
        // uses it as the stored value (it may be reassigned).
        if let Some(init_expr) = initializer
            && let Some(folded) = self.try_fold_constant_expression(init_expr)
        {
            let target_llvm = self.lang_type_to_llvm(var_type).map_err(|e| e.with_pos(pos))?;
            let coerced = if folded.get_type() == target_llvm {
                folded
            } else {
                self.constant_emitter().emit_cast(
                    folded,
                    target_llvm,
                    &init_expr.expr_type,
                    var_type,
                    init_expr.pos,
                )?
            };
            self.builder.build_store(alloca, coerced)?;
            let cv = if var_type.is_const {
                Some(coerced)
            } else {
                None
            };
            self.add_variable(name.to_string(), alloca, llvm_type, *var_type, cv);
            return Ok(());
        }

        self.add_variable(name.to_string(), alloca, llvm_type, *var_type, None);

        if let Some(init_expr) = initializer {
            let init_value = self.generate_coerced_value(init_expr, Some(var_type))?;
            self.builder.build_store(alloca, init_value)?;
        } else {
            self.builder.build_store(alloca, llvm_type.const_zero())?;
        }

        Ok(())
    }

    pub(crate) fn generate_var_assign(
        &mut self,
        pos: crate::lexer::Position,
        name: &str,
        value: &Expression,
    ) -> Result<(), CodegenError> {
        let (var_ptr, var_lang_type) = {
            let v = self
                .scope
                .lookup_any(name)
                .ok_or_else(|| CodegenError::UndefinedVariable(name.to_string(), pos))?;
            (v.ptr(), v.lang_type())
        };

        let value_llvm = self.generate_coerced_value(value, Some(&var_lang_type))?;
        self.builder.build_store(var_ptr, value_llvm)?;
        Ok(())
    }

    pub(crate) fn generate_deref_assign(
        &mut self,
        target: &Expression,
        value: &Expression,
    ) -> Result<(), CodegenError> {
        match &target.kind {
            ExprKind::Dereference(ptr_expr) => {
                let ptr = self.generate_expression(ptr_expr)?;
                // Coerce to the pointee type so that e.g. storing a literal i32
                // into a `u8 *` slot emits an i8 store, not a 4-byte i32 store.
                let value_llvm = self.generate_coerced_value(value, Some(&target.expr_type))?;
                self.builder
                    .build_store(ptr.into_pointer_value(), value_llvm)?;
                Ok(())
            }
            _ => Err(CodegenError::InvalidOperation(
                "DerefAssign target must be a dereference expression".to_string(),
                target.pos,
            )),
        }
    }

    /// Assign to a struct field: `base.field = value`.
    pub(crate) fn generate_field_assign(
        &mut self,
        target: &Expression,
        value: &Expression,
    ) -> Result<(), CodegenError> {
        let (field_ptr, field_ty) = self.emit_address(target)?;
        let value_llvm = self.generate_coerced_value(value, Some(&field_ty))?;
        self.builder.build_store(field_ptr, value_llvm)?;
        Ok(())
    }

    pub(crate) fn generate_return(
        &mut self,
        expr: Option<&Expression>,
        pos: crate::lexer::Position,
    ) -> Result<(), CodegenError> {
        // A `return` inside a value-block yields the innermost block, not the
        // function: store into the block's result slot and branch to its exit
        // block. Checked before the sret path — a value-block inside a
        // struct-returning function must still yield the block.
        if let Some((slot, exit_bb, result_type)) = self.value_block_stack.last().copied() {
            let expr = expr.ok_or_else(|| {
                CodegenError::InvalidOperation(
                    "value block `return` must carry a value".to_string(),
                    pos,
                )
            })?;
            let value = self.generate_coerced_value(expr, Some(&result_type))?;
            self.builder.build_store(slot, value)?;
            return self.branch_to_dead_end(exit_bb, "vblock.dead");
        }

        // Struct-by-value return: store through the hidden sret out-pointer and
        // return void.
        if let Some(sret_ptr) = self.current_sret {
            let expr = expr.ok_or_else(|| {
                CodegenError::InvalidOperation(
                    "struct-returning function must return a value".to_string(),
                    pos,
                )
            })?;
            let ret_type = self.current_function_return_type;
            let value = self.generate_coerced_value(expr, ret_type.as_ref())?;
            self.builder.build_store(sret_ptr, value)?;
            self.builder.build_return(None)?;
            return Ok(());
        }

        if let Some(expr) = expr {
            let ret_type = self.current_function_return_type;
            let value = self.generate_coerced_value(expr, ret_type.as_ref())?;
            self.builder.build_return(Some(&value))?;
        } else {
            self.builder.build_return(None)?;
        }
        Ok(())
    }

    pub(crate) fn generate_block(&mut self, statements: &[Statement]) -> Result<(), CodegenError> {
        self.enter_scope();
        for stmt in statements {
            self.generate_statement(stmt)?;
        }
        self.exit_scope();
        Ok(())
    }

    /// `{ ...; return v }` used as a value. Every `return` inside — routed here
    /// by `generate_return` via `value_block_stack` — stores into the result
    /// slot and branches to the exit block. The checker guarantees every path
    /// returns, so the fall-through tail is unreachable.
    pub(crate) fn generate_value_block(
        &mut self,
        statements: &[Statement],
        result_type: LangType,
        pos: crate::lexer::Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let function = self
            .current_function
            .ok_or(CodegenError::UnexpectedStatement(pos))?;
        let llvm_type = self
            .lang_type_to_llvm(&result_type)
            .map_err(|e| e.with_pos(pos))?;

        // Result slot in the entry block (mem2reg-friendly), mirroring
        // `generate_var_decl`'s alloca placement.
        let slot = self.build_entry_alloca(function, llvm_type, "vblock.slot", pos)?;

        let exit_bb = self.context.append_basic_block(function, "vblock.exit");

        self.value_block_stack.push((slot, exit_bb, result_type));
        self.enter_scope();
        for stmt in statements {
            if let Err(e) = self.generate_statement(stmt) {
                self.exit_scope();
                self.value_block_stack.pop();
                return Err(e);
            }
        }
        self.exit_scope();
        self.value_block_stack.pop();

        // The checker's all-paths rule makes the tail unreachable; if the
        // current block still lacks a terminator, say so explicitly.
        if self
            .builder
            .get_insert_block()
            .unwrap()
            .get_terminator()
            .is_none()
        {
            self.builder.build_unreachable()?;
        }

        self.builder.position_at_end(exit_bb);
        Ok(self.builder.build_load(llvm_type, slot, "vblock.val")?)
    }

    pub(crate) fn generate_if_statement(
        &mut self,
        condition: &Expression,
        then_block: &[Statement],
        else_block: Option<&[Statement]>,
    ) -> Result<(), CodegenError> {
        let function = self
            .current_function
            .ok_or(CodegenError::UnexpectedStatement(condition.pos))?;

        // One scope spans condition + then-block (not else): `is` bindings
        // registered while lowering the condition are visible exactly there,
        // and body-local declarations can't clobber outer names (the checker
        // scopes these blocks; codegen must mirror it — tri-scope invariant).
        self.enter_scope();
        let cond_value = self.generate_expression(condition)?;
        let cond_int = self.value_to_bool(condition.pos, cond_value)?;

        let then_bb = self.context.append_basic_block(function, "then");
        let else_bb = self.context.append_basic_block(function, "else");
        let merge_bb = self.context.append_basic_block(function, "ifcont");

        self.builder
            .build_conditional_branch(cond_int, then_bb, else_bb)?;

        self.builder.position_at_end(then_bb);
        for stmt in then_block {
            self.generate_statement(stmt)?;
        }
        self.exit_scope();
        if !self.block_has_terminator() {
            self.builder.build_unconditional_branch(merge_bb)?;
        }

        self.builder.position_at_end(else_bb);
        if let Some(else_stmts) = else_block {
            self.enter_scope();
            for stmt in else_stmts {
                self.generate_statement(stmt)?;
            }
            self.exit_scope();
        }
        if !self.block_has_terminator() {
            self.builder.build_unconditional_branch(merge_bb)?;
        }

        self.builder.position_at_end(merge_bb);

        Ok(())
    }

    pub(crate) fn generate_while_loop(
        &mut self,
        condition: &Expression,
        body: &[Statement],
    ) -> Result<(), CodegenError> {
        let function = self
            .current_function
            .ok_or(CodegenError::UnexpectedStatement(condition.pos))?;

        let cond_bb = self.context.append_basic_block(function, "while.cond");
        let body_bb = self.context.append_basic_block(function, "while.body");
        let end_bb = self.context.append_basic_block(function, "while.end");

        self.loop_stack.push((end_bb, cond_bb));

        self.builder.build_unconditional_branch(cond_bb)?;

        // One scope spans condition + body, mirroring the checker: `is`
        // bindings from the condition are re-stored each iteration and
        // visible in the body; body locals can't clobber outer names.
        self.enter_scope();
        self.builder.position_at_end(cond_bb);
        let cond_value = self.generate_expression(condition)?;
        let cond_int = self.value_to_bool(condition.pos, cond_value)?;
        self.builder
            .build_conditional_branch(cond_int, body_bb, end_bb)?;

        self.builder.position_at_end(body_bb);
        for stmt in body {
            self.generate_statement(stmt)?;
        }
        self.exit_scope();
        if !self.block_has_terminator() {
            self.builder.build_unconditional_branch(cond_bb)?;
        }

        self.loop_stack.pop();
        self.builder.position_at_end(end_bb);

        Ok(())
    }

    pub(crate) fn generate_for_loop(
        &mut self,
        init: Option<Box<Statement>>,
        condition: Option<&Expression>,
        increment: Option<Box<Statement>>,
        body: &[Statement],
    ) -> Result<(), CodegenError> {
        let function = self
            .current_function
            .ok_or_else(|| CodegenError::UnexpectedStatement(body[0].pos))?;

        self.enter_scope();

        if let Some(init_stmt) = init {
            self.generate_statement(&init_stmt)?;
        }

        let cond_bb = self.context.append_basic_block(function, "for.cond");
        let body_bb = self.context.append_basic_block(function, "for.body");
        let inc_bb = self.context.append_basic_block(function, "for.inc");
        let end_bb = self.context.append_basic_block(function, "for.end");

        // break → end_bb, continue → inc_bb
        self.loop_stack.push((end_bb, inc_bb));

        self.builder.build_unconditional_branch(cond_bb)?;

        self.builder.position_at_end(cond_bb);
        let cond_value = if let Some(cond_expr) = condition {
            let cond_val = self.generate_expression(cond_expr)?;
            self.value_to_bool(cond_expr.pos, cond_val)?
        } else {
            self.context.bool_type().const_all_ones()
        };
        self.builder
            .build_conditional_branch(cond_value, body_bb, end_bb)?;

        self.builder.position_at_end(body_bb);
        for stmt in body {
            self.generate_statement(stmt)?;
        }
        if !self.block_has_terminator() {
            self.builder.build_unconditional_branch(inc_bb)?;
        }

        self.builder.position_at_end(inc_bb);
        if let Some(inc_stmt) = increment {
            self.generate_statement(&inc_stmt)?;
        }
        self.builder.build_unconditional_branch(cond_bb)?;

        self.loop_stack.pop();
        self.builder.position_at_end(end_bb);

        self.exit_scope();

        Ok(())
    }

    pub(crate) fn value_to_bool(
        &self,
        pos: crate::lexer::Position,
        value: BasicValueEnum<'ctx>,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        if value.is_int_value() {
            let int_val = value.into_int_value();
            // Already i1 (e.g. a direct icmp/fcmp result) — no extra compare.
            if int_val.get_type().get_bit_width() == 1 {
                return Ok(int_val);
            }
            let zero = int_val.get_type().const_zero();
            Ok(self
                .builder
                .build_int_compare(IntPredicate::NE, int_val, zero, "tobool")?)
        } else if value.is_float_value() {
            let float_val = value.into_float_value();
            let zero = float_val.get_type().const_zero();
            Ok(self.builder.build_float_compare(
                inkwell::FloatPredicate::ONE,
                float_val,
                zero,
                "tobool",
            )?)
        } else if value.is_pointer_value() {
            // `if p` / `while p` on a pointer: true iff non-null — the inverse
            // of `!p` (which tests null). Comparing the pointer to a null of its
            // own type yields the i1 the conditional wants.
            let ptr = value.into_pointer_value();
            let null = ptr.get_type().const_null();
            Ok(self
                .builder
                .build_int_compare(IntPredicate::NE, ptr, null, "tobool")?)
        } else {
            Err(CodegenError::TypeError(
                "Cannot convert value to boolean".to_string(),
                pos,
            ))
        }
    }

    pub(crate) fn block_has_terminator(&self) -> bool {
        self.builder
            .get_insert_block()
            .and_then(inkwell::basic_block::BasicBlock::get_terminator)
            .is_some()
    }

    pub(crate) fn enter_scope(&mut self) {
        self.scope.enter();
    }

    pub(crate) fn exit_scope(&mut self) {
        self.scope.exit();
    }

    pub(crate) fn add_variable(
        &mut self,
        name: String,
        ptr: PointerValue<'ctx>,
        llvm_type: BasicTypeEnum<'ctx>,
        lang_type: LangType,
        const_value: Option<BasicValueEnum<'ctx>>,
    ) {
        self.scope
            .insert_local(name, ptr, llvm_type, lang_type, const_value);
    }

    /// `Some(value)` only when every sub-expression is provably constant
    /// (literal, folded `const` local, or a global with a known initializer);
    /// `None` for any dynamic sub-expression. Emits no IR.
    pub(crate) fn try_fold_constant_expression(
        &mut self,
        expr: &Expression,
    ) -> Option<BasicValueEnum<'ctx>> {
        const_eval(expr, self).ok()
    }
}

#[cfg(test)]
mod tests {
    use crate::codegen::CodeGenerator;
    use crate::parser::Parser;
    use crate::target::TargetSpec;
    use crate::typechecker::TypeChecker;
    use inkwell::context::Context;

    fn ir_for(source: &str, context: &Context) -> String {
        ir_for_opt(source, context, 0)
    }

    fn ir_for_opt(source: &str, context: &Context, opt_level: u8) -> String {
        let tokens = crate::lexer::tokenize(source.to_string()).expect("lex");
        let mut parser = Parser::new(tokens);
        let mut program = parser.parse_program().expect("parse");
        let mut tc = TypeChecker::new();
        tc.check_program(&mut program).expect("typecheck");
        let mut codegen = CodeGenerator::new(context, "switch_test", &TargetSpec::host())
            .expect("codegen setup");
        codegen.set_opt_level(opt_level);
        codegen.generate(&program).expect("generate");
        codegen.print_ir_to_string()
    }

    /// The else edge of a fully-listed sum switch is a `llvm.trap` block — a
    /// forged tag halts instead of undefined behavior — and it must be
    /// `llvm.trap`, never libc `abort` (freestanding targets link no libc).
    /// A corpus program can't assert this: an executed trap would kill the
    /// in-process JIT harness.
    #[test]
    fn complete_sum_switch_has_trap_edge() {
        let src = "sum S {\n    A(i32 x)\n    B\n}\n\nfn main(u32 argc, u8 **argv) -> i32 {\n    S s = S.A(1)\n    switch s {\n        case S.A(v) { return v }\n        case S.B { return 0 }\n    }\n}\n";
        let ctx = Context::create();
        let ir = ir_for(src, &ctx);
        assert!(ir.contains("llvm.trap"), "missing trap edge:\n{ir}");
        assert!(!ir.contains("@abort"), "trap edge must not call libc abort:\n{ir}");
    }

    /// Optimized builds treat a forged tag as UB: the else edge is a bare
    /// `unreachable`, keeping LLVM's range assumption (no jump-table bounds
    /// check).
    #[test]
    fn optimized_complete_switch_uses_unreachable() {
        let src = "sum S {\n    A(i32 x)\n    B\n}\n\nfn main(u32 argc, u8 **argv) -> i32 {\n    S s = S.A(1)\n    switch s {\n        case S.A(v) { return v }\n        case S.B { return 0 }\n    }\n}\n";
        let ctx = Context::create();
        let ir = ir_for_opt(src, &ctx, 2);
        assert!(!ir.contains("llvm.trap"), "trap should be -O0-only:\n{ir}");
        assert!(ir.contains("unreachable"), "missing unreachable else edge:\n{ir}");
    }

    /// Every alloca lands in the entry block — a mid-block alloca is dynamic
    /// stack adjustment, and in a loop it grows the stack per iteration
    /// (sret/byval/construction temps regressed this way once).
    #[test]
    fn all_allocas_in_entry_block() {
        let src = "sum S {\n    A(f64 a, f64 b)\n    B\n}\nfn mk(f64 x) -> S {\n    return S.A(x, x)\n}\nfn main(u32 argc, u8 **argv) -> i32 {\n    f64 acc = 0.0\n    for i32 i = 0; i < 3; i += 1 {\n        S s = mk(1.0)\n        if s is S.A(a, _) {\n            acc = acc + a\n        }\n    }\n    return acc as i32\n}\n";
        let ctx = Context::create();
        let ir = ir_for(src, &ctx);
        let mut label = "entry";
        for line in ir.lines() {
            if let Some(l) = line.strip_suffix(':')
                && !l.contains(' ')
            {
                label = if l == "entry" { "entry" } else { "other" };
            }
            if line.contains(" = alloca ") {
                assert_eq!(label, "entry", "mid-block alloca: {line}\n{ir}");
            }
        }
    }

    /// With a `default`, the else edge is the default block — no trap.
    #[test]
    fn defaulted_switch_has_no_trap() {
        let src = "fn main(u32 argc, u8 **argv) -> i32 {\n    i32 x = 1\n    switch x {\n        case 1 { return 1 }\n        default { return 0 }\n    }\n}\n";
        let ctx = Context::create();
        let ir = ir_for(src, &ctx);
        assert!(!ir.contains("llvm.trap"), "unexpected trap edge:\n{ir}");
    }
}
