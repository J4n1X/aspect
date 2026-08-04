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
                let (break_bb, _) = self.loop_stack.last().ok_or_else(|| {
                    CodegenError::InvalidOperation("'break' outside of loop".to_string(), stmt.pos)
                })?;
                self.builder.build_unconditional_branch(*break_bb)?;
                let dead_bb = self
                    .context
                    .append_basic_block(self.current_function.unwrap(), "break.dead");
                self.builder.position_at_end(dead_bb);
                Ok(())
            }
            StatementKind::Continue => {
                let (_, continue_bb) = self.loop_stack.last().ok_or_else(|| {
                    CodegenError::InvalidOperation(
                        "'continue' outside of loop".to_string(),
                        stmt.pos,
                    )
                })?;
                self.builder.build_unconditional_branch(*continue_bb)?;
                let dead_bb = self
                    .context
                    .append_basic_block(self.current_function.unwrap(), "continue.dead");
                self.builder.position_at_end(dead_bb);
                Ok(())
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
            self.builder.build_unconditional_branch(exit_bb)?;
            // Park subsequent (unreachable) statements in a dead block, the
            // same trick `break`/`continue` use.
            let dead_bb = self
                .context
                .append_basic_block(self.current_function.unwrap(), "vblock.dead");
            self.builder.position_at_end(dead_bb);
            return Ok(());
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

    /// Lower a `switch`: evaluate the scrutinee exactly once, LLVM `switch`
    /// over the discriminant (a sum's `i32` tag, or the int/bool/enum value),
    /// per-arm blocks with payload bindings copied out of the scrutinee slot.
    /// The else edge is the `default` block when present; otherwise a
    /// `llvm.trap` block — the checker guarantees `default` exists unless the
    /// arms are coverage-complete, so the trap is only reachable through a
    /// forged tag (`u0*` bridge, stale pointer) or an out-of-range `as`-cast
    /// enum, and one cold trap beats undefined behavior. `llvm.trap` (`ud2`),
    /// never libc `abort()`: freestanding targets link no libc.
    pub(crate) fn generate_switch(
        &mut self,
        scrutinee: &Expression,
        arms: &[crate::parser::SwitchArm],
        default: Option<&[Statement]>,
        pos: crate::lexer::Position,
    ) -> Result<(), CodegenError> {
        use crate::parser::{LiteralValue, SwitchPattern};

        let function = self
            .current_function
            .ok_or(CodegenError::UnexpectedStatement(pos))?;
        let s_ty = scrutinee.expr_type;

        // Discriminant, plus (for sums) the slot arm bindings read from.
        let mut sum_slot = None;
        let disc = if s_ty.pointer_depth == 0
            && !s_ty.is_array()
            && let crate::lexer::TypeBase::Sum(sum_id) = s_ty.base
        {
            let storage = *self.sum_types.get(&sum_id).ok_or_else(|| {
                CodegenError::TypeError(format!("unregistered sum id {sum_id}"), pos)
            })?;
            let value = self.generate_expression(scrutinee)?;
            let slot = self.build_entry_alloca(function, storage.into(), "switch.scrut", pos)?;
            self.builder.build_store(slot, value)?;
            let tag_ptr = self
                .builder
                .build_struct_gep(storage, slot, 0, "switch.tag")?;
            sum_slot = Some((slot, sum_id));
            self.builder
                .build_load(self.context.i32_type(), tag_ptr, "tag")?
                .into_int_value()
        } else {
            let v = self.generate_expression(scrutinee)?.into_int_value();
            // Bool variables load as `i8` (comparisons already yield `i1`) —
            // normalize so the switch constants share one width.
            if s_ty.base == crate::lexer::TypeBase::Bool && v.get_type().get_bit_width() > 1 {
                self.builder.build_int_compare(
                    inkwell::IntPredicate::NE,
                    v,
                    v.get_type().const_zero(),
                    "switch.bool",
                )?
            } else {
                v
            }
        };

        let else_bb = self.context.append_basic_block(
            function,
            if default.is_some() {
                "switch.default"
            } else {
                "switch.trap"
            },
        );
        let merge_bb = self.context.append_basic_block(function, "switch.end");

        let disc_ty = disc.get_type();
        let mut cases = Vec::new();
        let mut arm_blocks = Vec::with_capacity(arms.len());
        for arm in arms {
            let bb = self.context.append_basic_block(function, "switch.case");
            for pattern in &arm.patterns {
                let const_val = match pattern {
                    SwitchPattern::SumVariant { variant, .. } => {
                        disc_ty.const_int(u64::from(*variant), false)
                    }
                    SwitchPattern::Const(e) => match &e.kind {
                        ExprKind::Literal(LiteralValue::Integer(v)) => {
                            // `as u64` keeps the two's-complement bits; LLVM
                            // truncates to the discriminant width, so negative
                            // labels land correctly at any width.
                            disc_ty.const_int(*v as u64, false)
                        }
                        ExprKind::Literal(LiteralValue::Bool(b)) => {
                            disc_ty.const_int(u64::from(*b), false)
                        }
                        ExprKind::EnumValue { value, .. } => {
                            disc_ty.const_int(*value as u64, false)
                        }
                        _ => {
                            return Err(CodegenError::InvalidOperation(
                                "non-constant case pattern survived checking".to_string(),
                                e.pos,
                            ))
                        }
                    },
                };
                cases.push((const_val, bb));
            }
            arm_blocks.push(bb);
        }
        self.builder.build_switch(disc, else_bb, &cases)?;

        for (arm, bb) in arms.iter().zip(arm_blocks) {
            self.builder.position_at_end(bb);
            self.enter_scope();
            // Bindings are copies: GEP the payload through the variant's bare
            // payload struct at storage field 1 (the uniform offset) and copy
            // each bound field into its own local.
            if let Some(SwitchPattern::SumVariant { variant, binders }) = arm.patterns.first()
                && binders.iter().any(Option::is_some)
            {
                let (slot, sum_id) =
                    sum_slot.expect("binding pattern without a sum scrutinee");
                let storage = self.sum_types[&sum_id];
                let payload_ptr = self
                    .builder
                    .build_struct_gep(storage, slot, 1, "switch.payload")?;
                let payload_ty = self
                    .sum_payload_type(sum_id, *variant as usize)
                    .map_err(|e| e.with_pos(arm.pos))?
                    .expect("binding pattern on a payload-less variant");
                for (i, binder) in binders.iter().enumerate() {
                    let Some((name, field_ty)) = binder else { continue };
                    let field_ptr = self.builder.build_struct_gep(
                        payload_ty,
                        payload_ptr,
                        u32::try_from(i).expect("payload field index fits u32"),
                        name,
                    )?;
                    if field_ty.is_array() {
                        let arr_ty = self
                            .lang_type_to_llvm_array(field_ty)
                            .map_err(|e| e.with_pos(arm.pos))?;
                        let alloca =
                            self.build_entry_alloca(function, arr_ty.into(), name, arm.pos)?;
                        let bytes = self.sizeof_lang_type(field_ty, arm.pos)?;
                        let align = self
                            .target_machine
                            .get_target_data()
                            .get_abi_alignment(&arr_ty);
                        self.builder.build_memcpy(
                            alloca,
                            align,
                            field_ptr,
                            align,
                            self.context.i64_type().const_int(bytes, false),
                        )?;
                        self.add_variable(name.clone(), alloca, arr_ty.into(), *field_ty, None);
                    } else {
                        let llvm_ty = self
                            .lang_type_to_llvm(field_ty)
                            .map_err(|e| e.with_pos(arm.pos))?;
                        let alloca =
                            self.build_entry_alloca(function, llvm_ty, name, arm.pos)?;
                        let v = self.builder.build_load(llvm_ty, field_ptr, name)?;
                        self.builder.build_store(alloca, v)?;
                        self.add_variable(name.clone(), alloca, llvm_ty, *field_ty, None);
                    }
                }
            }
            for stmt in &arm.body {
                self.generate_statement(stmt)?;
            }
            self.exit_scope();
            if self
                .builder
                .get_insert_block()
                .and_then(|b| b.get_terminator())
                .is_none()
            {
                self.builder.build_unconditional_branch(merge_bb)?;
            }
        }

        self.builder.position_at_end(else_bb);
        if let Some(default_body) = default {
            self.enter_scope();
            for stmt in default_body {
                self.generate_statement(stmt)?;
            }
            self.exit_scope();
            if self
                .builder
                .get_insert_block()
                .and_then(|b| b.get_terminator())
                .is_none()
            {
                self.builder.build_unconditional_branch(merge_bb)?;
            }
        } else {
            let trap = inkwell::intrinsics::Intrinsic::find("llvm.trap")
                .and_then(|i| i.get_declaration(&self.module, &[]))
                .ok_or_else(|| {
                    CodegenError::InvalidOperation(
                        "llvm.trap intrinsic unavailable".to_string(),
                        pos,
                    )
                })?;
            self.builder.build_call(trap, &[], "")?;
            self.builder.build_unreachable()?;
        }

        self.builder.position_at_end(merge_bb);
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
        let tokens = crate::lexer::tokenize(source.to_string()).expect("lex");
        let mut parser = Parser::new(tokens);
        let mut program = parser.parse_program().expect("parse");
        let mut tc = TypeChecker::new();
        tc.check_program(&mut program).expect("typecheck");
        let mut codegen = CodeGenerator::new(context, "switch_test", &TargetSpec::host())
            .expect("codegen setup");
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
        let src = "sum S {\n    A(i32 x)\n    B\n}\n\nfn main(u32 argc, u8 **argv) -> i32 {\n    S s = S.A(1)\n    switch s {\n        case A(v) { return v }\n        case B { return 0 }\n    }\n}\n";
        let ctx = Context::create();
        let ir = ir_for(src, &ctx);
        assert!(ir.contains("llvm.trap"), "missing trap edge:\n{ir}");
        assert!(!ir.contains("@abort"), "trap edge must not call libc abort:\n{ir}");
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
