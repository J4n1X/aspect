use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, IntValue, PointerValue};
use inkwell::IntPredicate;

use crate::codegen::expressions::{walk_expression, EmitMode};
use crate::codegen::generator::CodeGenerator;
use crate::codegen::value_emitter::{ConstantEmitter, ValueEmitter};
use crate::codegen::{CodegenError, LangTypeExt};
use crate::parser::LangType;
use crate::parser::{ExprKind, Expression, Statement, StatementKind};

impl<'ctx> CodeGenerator<'ctx> {
    /// Generate code for a statement
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
            StatementKind::Return(expr) => self.generate_return(expr.as_ref()),
            StatementKind::If {
                condition,
                then_block,
                else_block,
            } => self.generate_if_statement(condition, then_block, else_block.as_deref()),
            StatementKind::While { condition, body } => self.generate_while_loop(condition, body),
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
        if let ExprKind::FunctionCall { name, args } = &expr.kind {
            self.generate_function_call_statement(name, args, expr.pos)
        } else {
            self.generate_expression(expr)?;
            Ok(())
        }
    }

    pub(crate) fn generate_var_decl(
        &mut self,
        pos: crate::lexer::Position,
        var_type: &LangType,
        name: &str,
        initializer: Option<&Expression>,
    ) -> Result<(), CodegenError> {
        let llvm_type = if var_type.is_array() {
            var_type.to_llvm_array(self.context)?.into()
        } else {
            var_type.to_llvm(self.context)?
        };

        // Allocate in the entry block for mem2reg compatibility
        let function = self
            .current_function
            .ok_or(CodegenError::UnexpectedStatement(pos))?;
        let entry_block = function
            .get_first_basic_block()
            .ok_or(CodegenError::UnexpectedStatement(pos))?;
        let current_block = self.builder.get_insert_block().unwrap();

        // Position at the start of the entry block (before any instructions/terminators)
        if let Some(first_instr) = entry_block.get_first_instruction() {
            self.builder.position_before(&first_instr);
        } else {
            self.builder.position_at_end(entry_block);
        }
        let alloca = self.builder.build_alloca(llvm_type, name)?;
        self.builder.position_at_end(current_block);

        if var_type.is_array() {
            self.add_variable(name.to_string(), alloca, llvm_type, *var_type, None);
            if let Some(Expression {
                kind: ExprKind::ListInitializer(elements),
                ..
            }) = initializer
            {
                return self.generate_list_initializer(alloca, var_type, elements);
            }
            return Ok(());
        }

        // Attempt to fold the initializer to a compile-time constant for all variables.
        // For `const` vars: cache the folded value in the scope entry so reads bypass
        //                   the alloca/load entirely.
        // For non-const vars: use the constant as the stored value but don't cache it,
        //                     since the variable may be reassigned later.
        if let Some(init_expr) = initializer {
            if let Some(folded) = self.try_fold_constant_expression(init_expr) {
                let target_llvm = var_type.to_llvm(self.context)?;
                let coerced = if folded.get_type() == target_llvm {
                    folded
                } else {
                    ConstantEmitter {
                        context: self.context,
                    }
                    .emit_cast(
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

    pub(crate) fn generate_return(
        &mut self,
        expr: Option<&Expression>,
    ) -> Result<(), CodegenError> {
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

    /// Generate an if statement
    pub(crate) fn generate_if_statement(
        &mut self,
        condition: &Expression,
        then_block: &[Statement],
        else_block: Option<&[Statement]>,
    ) -> Result<(), CodegenError> {
        let function = self
            .current_function
            .ok_or(CodegenError::UnexpectedStatement(condition.pos))?;

        // Generate condition
        let cond_value = self.generate_expression(condition)?;
        let cond_int = self.value_to_bool(cond_value)?;

        let then_bb = self.context.append_basic_block(function, "then");
        let else_bb = self.context.append_basic_block(function, "else");
        let merge_bb = self.context.append_basic_block(function, "ifcont");

        // Branch on condition
        self.builder
            .build_conditional_branch(cond_int, then_bb, else_bb)?;

        // Generate then block
        self.builder.position_at_end(then_bb);
        for stmt in then_block {
            self.generate_statement(stmt)?;
        }
        if !self.block_has_terminator() {
            self.builder.build_unconditional_branch(merge_bb)?;
        }

        // Generate else block
        self.builder.position_at_end(else_bb);
        if let Some(else_stmts) = else_block {
            for stmt in else_stmts {
                self.generate_statement(stmt)?;
            }
        }
        if !self.block_has_terminator() {
            self.builder.build_unconditional_branch(merge_bb)?;
        }

        // Continue at merge block
        self.builder.position_at_end(merge_bb);

        Ok(())
    }

    /// Generate a while loop
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

        // Push loop context for break/continue
        self.loop_stack.push((end_bb, cond_bb));

        // Jump to condition
        self.builder.build_unconditional_branch(cond_bb)?;

        // Generate condition
        self.builder.position_at_end(cond_bb);
        let cond_value = self.generate_expression(condition)?;
        let cond_int = self.value_to_bool(cond_value)?;
        self.builder
            .build_conditional_branch(cond_int, body_bb, end_bb)?;

        // Generate body
        self.builder.position_at_end(body_bb);
        for stmt in body {
            self.generate_statement(stmt)?;
        }
        if !self.block_has_terminator() {
            self.builder.build_unconditional_branch(cond_bb)?;
        }

        // Pop loop context and continue after loop
        self.loop_stack.pop();
        self.builder.position_at_end(end_bb);

        Ok(())
    }

    /// Generate a for loop
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

        // Enter scope for loop variable
        self.enter_scope();

        // Generate init
        if let Some(init_stmt) = init {
            self.generate_statement(&init_stmt)?;
        }

        let cond_bb = self.context.append_basic_block(function, "for.cond");
        let body_bb = self.context.append_basic_block(function, "for.body");
        let inc_bb = self.context.append_basic_block(function, "for.inc");
        let end_bb = self.context.append_basic_block(function, "for.end");

        // Push loop context for break/continue
        // break jumps to end_bb, continue jumps to inc_bb
        self.loop_stack.push((end_bb, inc_bb));

        // Jump to condition
        self.builder.build_unconditional_branch(cond_bb)?;

        // Generate condition
        self.builder.position_at_end(cond_bb);
        let cond_value = if let Some(cond_expr) = condition {
            let cond_val = self.generate_expression(cond_expr)?;
            self.value_to_bool(cond_val)?
        } else {
            self.context.bool_type().const_all_ones()
        };
        self.builder
            .build_conditional_branch(cond_value, body_bb, end_bb)?;

        // Generate body
        self.builder.position_at_end(body_bb);
        for stmt in body {
            self.generate_statement(stmt)?;
        }
        if !self.block_has_terminator() {
            self.builder.build_unconditional_branch(inc_bb)?;
        }

        // Generate increment
        self.builder.position_at_end(inc_bb);
        if let Some(inc_stmt) = increment {
            self.generate_statement(&inc_stmt)?;
        }
        self.builder.build_unconditional_branch(cond_bb)?;

        // Pop loop context and continue after loop
        self.loop_stack.pop();
        self.builder.position_at_end(end_bb);

        // Exit loop scope
        self.exit_scope();

        Ok(())
    }

    /// Convert a value to a boolean (i1) for conditionals
    pub(crate) fn value_to_bool(
        &self,
        value: BasicValueEnum<'ctx>,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        if value.is_int_value() {
            let int_val = value.into_int_value();
            // Already i1 (e.g. direct result of icmp/fcmp) — no extra compare needed
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
        } else {
            Err(CodegenError::TypeError(
                "Cannot convert value to boolean".to_string(),
                crate::lexer::Position::new(0, 0),
            ))
        }
    }

    /// Check if the current block has a terminator
    pub(crate) fn block_has_terminator(&self) -> bool {
        self.builder
            .get_insert_block()
            .and_then(inkwell::basic_block::BasicBlock::get_terminator)
            .is_some()
    }

    /// Get a zero value for a type
    pub(crate) fn get_zero_value(
        &self,
        ty: &crate::lexer::LangType,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let llvm_type = ty.to_llvm(self.context)?;
        Ok(llvm_type.const_zero())
    }

    // Scope management
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

    /// Try to fold `expr` to a compile-time constant without emitting any IR.
    ///
    /// Returns `Some(value)` only when every sub-expression is provably constant
    /// (literal, previously-folded `const` local, or a global with a known initializer).
    /// Returns `None` for any dynamic sub-expression (function call, non-const local, etc.).
    pub(crate) fn try_fold_constant_expression(
        &mut self,
        expr: &Expression,
    ) -> Option<BasicValueEnum<'ctx>> {
        walk_expression(expr, self, EmitMode::Constant).ok()
    }
}
