use anyhow::{Context as AnyhowContext, Result as AnyhowResult};
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{CodeModel, InitializationConfig, RelocMode, Target, TargetMachine};
use inkwell::module::Module;
use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{
    BasicValueEnum, FunctionValue, IntValue, PointerValue,
};
use inkwell::basic_block::BasicBlock;
use inkwell::{AddressSpace, OptimizationLevel};
use inkwell::IntPredicate;
use std::collections::HashMap;

use crate::codegen::{is_void_type, lang_type_to_llvm, lang_type_to_llvm_array, CodegenError};
use crate::lexer::TypeBase;
use crate::parser::{
    BinaryOp, ComparisonOp, ExprKind, Expression, Function, GlobalVar, LiteralValue, Program,
    Statement, StatementKind,
};
use crate::parser::LangType;

/// Info for a local variable in a scope
struct LocalVar<'ctx> {
    ptr: PointerValue<'ctx>,
    llvm_type: BasicTypeEnum<'ctx>,
    lang_type: LangType,
}

/// Info for a global variable
struct GlobalVarInfo<'ctx> {
    ptr: PointerValue<'ctx>,
    llvm_type: BasicTypeEnum<'ctx>,
    lang_type: LangType,
}

pub struct CodeGenerator<'ctx> {
    context: &'ctx Context,
    module: Module<'ctx>,
    builder: Builder<'ctx>,
    target: Target,

    functions: HashMap<String, FunctionValue<'ctx>>,

    // Unified local variable scope stack (RT-11: merged variables + variable_types + array tracking)
    variables: Vec<HashMap<String, LocalVar<'ctx>>>,

    // Global variables (unified tracking)
    global_variables: HashMap<String, GlobalVarInfo<'ctx>>,

    current_function: Option<FunctionValue<'ctx>>,

    // Loop stack for break/continue support (RT-4)
    // Each entry is (break_bb, continue_bb)
    loop_stack: Vec<(BasicBlock<'ctx>, BasicBlock<'ctx>)>,
}

impl<'ctx> CodeGenerator<'ctx> {
    /// Creates a new `CodeGenerator` with the given LLVM context and module name.
    ///
    /// # Panics
    ///
    /// Panics if the default target triple cannot be resolved to a valid target.
    #[must_use]
    pub fn new(context: &'ctx Context, module_name: &str) -> Self {
        let module = context.create_module(module_name);
        let builder = context.create_builder();
        
        // Initialize target
        Target::initialize_native(&InitializationConfig::default()).expect("Failed to initialize native target");

        // TODO: Make target configurable
        let target = Target::from_triple(&TargetMachine::get_default_triple())
            .expect("Failed to get target from triple");

        Self {
            context,
            module,
            builder,
            target,
            functions: HashMap::new(),
            variables: vec![HashMap::new()],
            global_variables: HashMap::new(),
            current_function: None,
            loop_stack: Vec::new(),
        }
    }

    /// Generate LLVM IR from a program
    /// # Errors
    /// Returns `CodegenError` if any of the nested functions fail
    /// # Panics
    /// Panics if target machine creation fails, which should not happen with valid targets
    pub fn generate(&mut self, program: &Program) -> AnyhowResult<()> {
        // Generate global string literals first (they might be referenced by globals)
        for (i, s) in program.string_literals.iter().enumerate() {
            self.generate_string_literal(i, s);
        }

        // First pass: Declare all functions (for forward references)
        for func in &program.functions {
            self.declare_function(func)
                .with_context(|| format!("failed to declare function '{}'", func.proto.name))?;
        }

        // Generate global variables
        for global in &program.global_vars {
            self.generate_global_variable(global)
                .with_context(|| format!("failed to generate global variable '{}'", global.name))?;
        }

        // Second pass: Generate function bodies
        for func in &program.functions {
            if !func.proto.is_extern {
                self.generate_function(func)
                    .with_context(|| format!("failed to generate function '{}'", func.proto.name))?;
            }
        }
        Ok(())
    }

    /// Declare a function (without body)
    fn declare_function(&mut self, func: &Function) -> Result<FunctionValue<'ctx>, CodegenError> {
        // Convert parameter types
        let param_types: Result<Vec<_>, _> = func
            .proto
            .params
            .iter()
            .map(|(ty, _)| lang_type_to_llvm(self.context, ty))
            .collect();
        let param_types = param_types?;

        // Convert return type
        let return_type = if is_void_type(&func.proto.return_type) {
            None
        } else {
            Some(lang_type_to_llvm(self.context, &func.proto.return_type)?)
        };

        // Create function type
        let fn_type = if let Some(ret_ty) = return_type {
            let param_types: Vec<_> = param_types.iter().map(|ty| (*ty).into()).collect();
            ret_ty.fn_type(&param_types, false)
        } else {
            let param_types: Vec<_> = param_types.iter().map(|ty| (*ty).into()).collect();
            self.context.void_type().fn_type(&param_types, false)
        };

        // Add function to module
        let function = self.module.add_function(&func.proto.name, fn_type, None);

        // Set parameter names
        for (i, (_, param_name)) in func.proto.params.iter().enumerate() {
            function
                .get_nth_param(u32::try_from(i).expect("Parameter index out of bounds"))
                .unwrap()
                .set_name(param_name);
        }

        self.functions.insert(func.proto.name.clone(), function);
        Ok(function)
    }

    /// Generate code for a statement
    fn generate_function(&mut self, func: &Function) -> Result<(), CodegenError> {
        let function = *self.functions.get(&func.proto.name).ok_or_else(|| {
            CodegenError::UndefinedFunction(func.proto.name.clone(), func.proto.pos)
        })?;

        self.current_function = Some(function);

        // Create entry block
        let entry_block = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry_block);

        // Enter function scope
        self.enter_scope();

        // Allocate space for parameters and store them (in the entry block)
        for (i, (param_type, param_name)) in func.proto.params.iter().enumerate() {
            let param_value = function.get_nth_param(u32::try_from(i).expect("Parameter index out of bounds")).unwrap();
            let param_llvm_type = lang_type_to_llvm(self.context, param_type)?;

            let alloca = self.builder.build_alloca(param_llvm_type, param_name)?;
            self.builder.build_store(alloca, param_value)?;

            self.add_variable(param_name.clone(), alloca, param_llvm_type, *param_type);
        }

        // Generate function body (variables are allocated at their declaration site)
        for stmt in &func.body {
            self.generate_statement(stmt)?;
        }

        // If function doesn't have an explicit return, add one
        if !self.block_has_terminator() {
            if is_void_type(&func.proto.return_type) {
                self.builder.build_return(None)?;
            } else {
                let zero = self.get_zero_value(&func.proto.return_type)?;
                self.builder.build_return(Some(&zero))?;
            }
        }

        self.exit_scope();
        self.current_function = None;

        Ok(())
    }

    /// Generate a global variable
    fn generate_global_variable(&mut self, global: &GlobalVar) -> Result<(), CodegenError> {
        let (global_type, _is_array) = if global.var_type.is_array() {
            (lang_type_to_llvm_array(self.context, &global.var_type)?.into(), true)
        } else {
            (lang_type_to_llvm(self.context, &global.var_type)?, false)
        };

        let global_var =
            self.module
                .add_global(global_type, Some(AddressSpace::default()), &global.name);

        if let Some(init_expr) = &global.initializer {
            let init_value = self.generate_constant_expression(init_expr)?;
            global_var.set_initializer(&init_value);
        } else {
            global_var.set_initializer(&global_type.const_zero());
        }

        self.global_variables.insert(
            global.name.clone(),
            GlobalVarInfo {
                ptr: global_var.as_pointer_value(),
                llvm_type: global_type,
                lang_type: global.var_type,
            },
        );
        Ok(())
    }

    /// Generate a string literal
    fn generate_string_literal(&mut self, index: usize, value: &str) {
        let string_name = format!(".str.{index}");
        let string_value = self.context.const_string(value.as_bytes(), true);
        let global_string = self.module.add_global(
            string_value.get_type(),
            Some(AddressSpace::default()),
            &string_name,
        );
        global_string.set_initializer(&string_value);
        global_string.set_constant(true);

        let ptr_ty = self.context.ptr_type(AddressSpace::default());
        self.global_variables.insert(
            string_name,
            GlobalVarInfo {
                ptr: global_string.as_pointer_value(),
                llvm_type: ptr_ty.into(),
                lang_type: LangType::new(TypeBase::UInt, 8, 1, false),
            },
        );
    }

    /// Generate code for a statement
    fn generate_statement(&mut self, stmt: &Statement) -> Result<(), CodegenError> {
        match &stmt.kind {
            StatementKind::Expression(expr) => self.generate_expression_statement(expr),
            StatementKind::VarDecl { var_type, name, initializer } => {
                self.generate_var_decl(stmt.pos, var_type, name, initializer.as_ref())
            }
            StatementKind::VarAssign { name, value } => {
                self.generate_var_assign(stmt.pos, name, value)
            }
            StatementKind::DerefAssign { target, value } => {
                self.generate_deref_assign(target, value)
            }
            StatementKind::Return(expr) => self.generate_return(expr.as_ref()),
            StatementKind::If { condition, then_block, else_block } => {
                self.generate_if_statement(condition, then_block, else_block.as_deref())
            }
            StatementKind::While { condition, body } => {
                self.generate_while_loop(condition, body)
            }
            StatementKind::For { init, condition, increment, body } => {
                self.generate_for_loop(init.clone(), condition.as_ref(), increment.clone(), body)
            }
            StatementKind::Block(statements) => self.generate_block(statements),
            StatementKind::Break => {
                let (break_bb, _) = self.loop_stack.last()
                    .ok_or_else(|| CodegenError::InvalidOperation(
                        "'break' outside of loop".to_string(), stmt.pos,
                    ))?;
                self.builder.build_unconditional_branch(*break_bb)?;
                let dead_bb = self.context.append_basic_block(
                    self.current_function.unwrap(), "break.dead",
                );
                self.builder.position_at_end(dead_bb);
                Ok(())
            }
            StatementKind::Continue => {
                let (_, continue_bb) = self.loop_stack.last()
                    .ok_or_else(|| CodegenError::InvalidOperation(
                        "'continue' outside of loop".to_string(), stmt.pos,
                    ))?;
                self.builder.build_unconditional_branch(*continue_bb)?;
                let dead_bb = self.context.append_basic_block(
                    self.current_function.unwrap(), "continue.dead",
                );
                self.builder.position_at_end(dead_bb);
                Ok(())
            }
        }
    }

    fn generate_expression_statement(&mut self, expr: &Expression) -> Result<(), CodegenError> {
        if let ExprKind::FunctionCall { name, args } = &expr.kind {
            self.generate_function_call_statement(name, args, expr.pos)
        } else {
            self.generate_expression(expr)?;
            Ok(())
        }
    }

    fn generate_var_decl(
        &mut self,
        pos: crate::lexer::Position,
        var_type: &LangType,
        name: &str,
        initializer: Option<&Expression>,
    ) -> Result<(), CodegenError> {
        let llvm_type = if var_type.is_array() {
            lang_type_to_llvm_array(self.context, var_type)?.into()
        } else {
            lang_type_to_llvm(self.context, var_type)?
        };

        // Allocate in the entry block for mem2reg compatibility (RT-1 fix)
        let function = self.current_function
            .ok_or_else(|| CodegenError::UnexpectedStatement(pos))?;
        let entry_block = function.get_first_basic_block()
            .ok_or_else(|| CodegenError::UnexpectedStatement(pos))?;
        let current_block = self.builder.get_insert_block().unwrap();

        // Position at the start of the entry block (before any instructions/terminators)
        if let Some(first_instr) = entry_block.get_first_instruction() {
            self.builder.position_before(&first_instr);
        } else {
            self.builder.position_at_end(entry_block);
        }
        let alloca = self.builder.build_alloca(llvm_type, name)?;
        self.builder.position_at_end(current_block);

        // Store in scope with LangType for array tracking (RT-2 fix)
        self.add_variable(name.to_string(), alloca, llvm_type, *var_type);

        if var_type.is_array() {
            return Ok(());
        }

        if let Some(init_expr) = initializer {
            // For integer/float literals assigned to numeric types, generate with
            // the target type directly so the literal type matches its context.
            // Skip for pointer/array targets — those need the default path.
            let init_value = match &init_expr.kind {
                ExprKind::Literal(lit @ LiteralValue::Integer(_))
                    if var_type.pointer_depth == 0 && !var_type.is_array() =>
                {
                    self.generate_literal_typed(lit, var_type, init_expr.pos)?
                }
                ExprKind::Literal(lit @ LiteralValue::Float(_))
                    if var_type.pointer_depth == 0 && !var_type.is_array() =>
                {
                    self.generate_literal_typed(lit, var_type, init_expr.pos)?
                }
                _ => {
                    let mut val = self.generate_expression(init_expr)?;
                    if val.get_type() != llvm_type {
                        val = self.cast_value(val, llvm_type, &init_expr.expr_type, var_type)?;
                    }
                    val
                }
            };
            self.builder.build_store(alloca, init_value)?;
        } else {
            self.builder.build_store(alloca, llvm_type.const_zero())?;
        }

        Ok(())
    }

    fn generate_var_assign(
        &mut self,
        pos: crate::lexer::Position,
        name: &str,
        value: &Expression,
    ) -> Result<(), CodegenError> {
        let var_info = self
            .lookup_var_info(name)
            .ok_or_else(|| CodegenError::UndefinedVariable(name.to_string(), pos))?;

        let var_ptr = var_info.ptr;
        let var_type = var_info.llvm_type;
        let var_lang_type = var_info.lang_type;

        let value_llvm = match &value.kind {
            ExprKind::Literal(lit @ LiteralValue::Integer(_))
                if var_lang_type.pointer_depth == 0 && !var_lang_type.is_array() =>
            {
                self.generate_literal_typed(lit, &var_lang_type, value.pos)?
            }
            ExprKind::Literal(lit @ LiteralValue::Float(_))
                if var_lang_type.pointer_depth == 0 && !var_lang_type.is_array() =>
            {
                self.generate_literal_typed(lit, &var_lang_type, value.pos)?
            }
            _ => {
                let mut val = self.generate_expression(value)?;
                if val.get_type() != var_type {
                    val = self.cast_value(val, var_type, &value.expr_type, &var_lang_type)?;
                }
                val
            }
        };

        self.builder.build_store(var_ptr, value_llvm)?;
        Ok(())
    }

    fn generate_deref_assign(
        &mut self,
        target: &Expression,
        value: &Expression,
    ) -> Result<(), CodegenError> {
        match &target.kind {
            ExprKind::Dereference(ptr_expr) => {
                let ptr = self.generate_expression(ptr_expr)?;
                let value_llvm = self.generate_expression(value)?;
                self.builder.build_store(ptr.into_pointer_value(), value_llvm)?;
                Ok(())
            }
            _ => Err(CodegenError::InvalidOperation(
                "DerefAssign target must be a dereference expression".to_string(),
                target.pos,
            )),
        }
    }

    fn generate_return(&mut self, expr: Option<&Expression>) -> Result<(), CodegenError> {
        if let Some(expr) = expr {
            let value = self.generate_expression(expr)?;
            self.builder.build_return(Some(&value))?;
        } else {
            self.builder.build_return(None)?;
        }
        Ok(())
    }

    fn generate_block(&mut self, statements: &[Statement]) -> Result<(), CodegenError> {
        self.enter_scope();
        for stmt in statements {
            self.generate_statement(stmt)?;
        }
        self.exit_scope();
        Ok(())
    }

    /// Generate an if statement
    fn generate_if_statement(
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
            .build_conditional_branch(cond_int, then_bb, else_bb)
            ?;

        // Generate then block
        self.builder.position_at_end(then_bb);
        for stmt in then_block {
            self.generate_statement(stmt)?;
        }
        if !self.block_has_terminator() {
            self.builder
                .build_unconditional_branch(merge_bb)
                ?;
        }

        // Generate else block
        self.builder.position_at_end(else_bb);
        if let Some(else_stmts) = else_block {
            for stmt in else_stmts {
                self.generate_statement(stmt)?;
            }
        }
        if !self.block_has_terminator() {
            self.builder
                .build_unconditional_branch(merge_bb)
                ?;
        }

        // Continue at merge block
        self.builder.position_at_end(merge_bb);

        Ok(())
    }

    /// Generate a while loop
    fn generate_while_loop(
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

        // Push loop context for break/continue (RT-4)
        self.loop_stack.push((end_bb, cond_bb));

        // Jump to condition
        self.builder.build_unconditional_branch(cond_bb)?;

        // Generate condition
        self.builder.position_at_end(cond_bb);
        let cond_value = self.generate_expression(condition)?;
        let cond_int = self.value_to_bool(cond_value)?;
        self.builder.build_conditional_branch(cond_int, body_bb, end_bb)?;

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
    fn generate_for_loop(
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

        // Push loop context for break/continue (RT-4)
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
        self.builder.build_conditional_branch(cond_value, body_bb, end_bb)?;

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

    /// Generate code for an expression
    fn generate_expression(
        &mut self,
        expr: &Expression,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        match &expr.kind {
            ExprKind::Literal(lit) => self.generate_literal(lit, &expr.expr_type),

            ExprKind::Variable(name) => {
                let var_info = self
                    .lookup_var_info(name)
                    .ok_or_else(|| CodegenError::UndefinedVariable(name.clone(), expr.pos))?;

                // Array-to-pointer decay (RT-2: use lang_type instead of HashSet)
                if var_info.lang_type.is_array() {
                    return Ok(var_info.ptr.into());
                }

                Ok(self.builder.build_load(var_info.llvm_type, var_info.ptr, name)?)
            }

            ExprKind::Binary { left, op, right } => self.generate_binary_op(left, op, right),

            ExprKind::Comparison { left, op, right } => self.generate_comparison(left, op, right),

            ExprKind::Reference(expr) => {
                match &expr.kind {
                    ExprKind::Variable(name) => {
                        let var_info = self.lookup_var_info(name)
                            .ok_or_else(|| {
                                CodegenError::UndefinedVariable(name.clone(), expr.pos)
                            })?;
                        Ok(var_info.ptr.into())
                    }
                    ExprKind::Dereference(inner) => {
                        // &*ptr = ptr
                        self.generate_expression(inner)
                    }
                    _ => Err(CodegenError::InvalidOperation(
                        "Cannot take address of non-lvalue".to_string(),
                        expr.pos,
                    )),
                }
            }

            ExprKind::Dereference(inner_expr) => {
                let ptr = self.generate_expression(inner_expr)?;
                // The type to load is the type of the dereference expression itself (the pointee type),
                // not the type of the pointer expression
                let derefed_type = if inner_expr.expr_type.pointer_depth == 0 {
                    return Err(CodegenError::TypeError(
                        "Cannot dereference a non-pointer type".to_string(),
                        expr.pos,
                    ));
                } else {
                    LangType {
                        base: inner_expr.expr_type.base,
                        size_bits: inner_expr.expr_type.size_bits,
                        pointer_depth: inner_expr.expr_type.pointer_depth - 1,
                        is_const: inner_expr.expr_type.is_const,
                        array_size: None,
                    }
                };
                let pointee_type = lang_type_to_llvm(self.context, &derefed_type)?;
                Ok(self.builder
                    .build_load(pointee_type, ptr.into_pointer_value(), "deref")?)
            }

            ExprKind::FunctionCall { name, args } => {
                self.generate_function_call(name, args, expr.pos)
            }

            ExprKind::Cast { expr, target_type } => self.generate_cast(expr, target_type),
            ExprKind::Alloc { alloc_type, count } => {
                self.generate_alloc(alloc_type, count)
            }

            ExprKind::UnaryNot(inner) => {
                let val = self.generate_expression(inner)?.into_int_value();
                let zero = val.get_type().const_zero();
                let cmp = self.builder.build_int_compare(
                    inkwell::IntPredicate::EQ,
                    val,
                    zero,
                    "nottmp",
                )?;
                Ok(self.builder.build_int_z_extend(cmp, self.context.i32_type(), "nottmp_ext")?.into())
            }

            ExprKind::BitwiseNot(inner) => {
                let val = self.generate_expression(inner)?.into_int_value();
                Ok(self.builder.build_not(val, "nottmp")?.into())
            }
        }
    }

    /// Generate a literal value, using the given type for the literal.
    /// Includes overflow detection for integer literals.
    #[allow(clippy::cast_sign_loss)]
    fn generate_literal_typed(
        &self,
        lit: &LiteralValue,
        ty: &crate::lexer::LangType,
        pos: crate::lexer::Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        match lit {
            LiteralValue::Integer(val) => {
                let llvm_type = lang_type_to_llvm(self.context, ty)?;
                match llvm_type {
                    BasicTypeEnum::IntType(int_ty) => {
                        let bits = int_ty.get_bit_width();
                        if bits < 64 {
                            let fits = if matches!(ty.base, TypeBase::SInt) {
                                let min = -(1i64 << (bits - 1));
                                let max = (1i64 << (bits - 1)) - 1;
                                *val >= min && *val <= max
                            } else {
                                *val >= 0 && (*val as u64) < (1u64 << bits)
                            };
                            if !fits {
                                return Err(CodegenError::TypeError(
                                    format!("integer literal {} overflows {}", val, ty),
                                    pos,
                                ));
                            }
                        }
                        Ok(int_ty.const_int(*val as u64, true).into())
                    }
                    _ => Err(CodegenError::TypeError(
                        "Integer literal must have integer type".to_string(),
                        pos,
                    )),
                }
            }
            LiteralValue::Float(val) => {
                let llvm_type = lang_type_to_llvm(self.context, ty)?;
                match llvm_type {
                    BasicTypeEnum::FloatType(float_ty) => Ok(float_ty.const_float(*val).into()),
                    _ => Err(CodegenError::TypeError(
                        "Float literal must have float type".to_string(),
                        pos,
                    )),
                }
            }
            LiteralValue::String(index) => {
                let string_name = format!(".str.{index}");
                let global_info = self.global_variables.get(&string_name)
                    .expect("Internal error: String literal global not found");
                let i8_ptr_type = self.context.ptr_type(AddressSpace::default());
                let casted = self.builder.build_pointer_cast(global_info.ptr, i8_ptr_type, "str")?;
                Ok(casted.into())
            }
        }
    }

    /// Generate a literal value (default path, no overflow checking)
    #[allow(clippy::cast_sign_loss)]
    fn generate_literal(
        &self,
        lit: &LiteralValue,
        ty: &crate::lexer::LangType,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        match lit {
            LiteralValue::Integer(val) => {
                let llvm_type = lang_type_to_llvm(self.context, ty)?;
                match llvm_type {
                    BasicTypeEnum::IntType(int_ty) => {
                        Ok(int_ty.const_int(*val as u64, true).into())
                    }
                    _ => Err(CodegenError::TypeError(
                        "Integer literal must have integer type".to_string(),
                        crate::lexer::Position::new(0, 0),
                    )),
                }
            }

            LiteralValue::Float(val) => {
                let llvm_type = lang_type_to_llvm(self.context, ty)?;
                match llvm_type {
                    BasicTypeEnum::FloatType(float_ty) => Ok(float_ty.const_float(*val).into()),
                    _ => Err(CodegenError::TypeError(
                        "Float literal must have float type".to_string(),
                        crate::lexer::Position::new(0, 0),
                    )),
                }
            }

            LiteralValue::String(index) => {
                let string_name = format!(".str.{index}");
                let global_info = self.global_variables.get(&string_name)
                    .expect("Internal error: String literal global not found");

                let i8_ptr_type = self.context.ptr_type(AddressSpace::default());
                let casted = self
                    .builder
                    .build_pointer_cast(global_info.ptr, i8_ptr_type, "str")?;

                Ok(casted.into())
            }
        }
    }

    fn generate_int_binary_op(
        &mut self,
        left: &Expression,
        op: &BinaryOp,
        right: &Expression,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let is_signed = matches!(left.expr_type.base, TypeBase::SInt);
        let mut left_int = self.generate_expression(left)?.into_int_value();
        let mut right_int = self.generate_expression(right)?.into_int_value();

        // Ensure both integers have the same bit width
        if left_int.get_type().get_bit_width() != right_int.get_type().get_bit_width() {
            let left_bits = left_int.get_type().get_bit_width();
            let right_bits = right_int.get_type().get_bit_width();
            let right_is_signed = matches!(right.expr_type.base, TypeBase::SInt);
            if left_bits > right_bits {
                eprintln!("warning: implicit cast: {} ({}-bit) widened to {}-bit to match left operand in binary expression at {}:{}",
                    right.expr_type, right_bits, left_bits, right.pos.line, right.pos.column);
                right_int = if right_is_signed {
                    self.builder.build_int_s_extend(right_int, left_int.get_type(), "sext")?
                } else {
                    self.builder.build_int_z_extend(right_int, left_int.get_type(), "zext")?
                };
            } else {
                eprintln!("warning: implicit cast: {} ({}-bit) widened to {}-bit to match right operand in binary expression at {}:{}",
                    left.expr_type, left_bits, right_bits, left.pos.line, left.pos.column);
                left_int = if is_signed {
                    self.builder.build_int_s_extend(left_int, right_int.get_type(), "sext")?
                } else {
                    self.builder.build_int_z_extend(left_int, right_int.get_type(), "zext")?
                };
            }
        }

        let res = match op {
            BinaryOp::Add => self
                .builder
                .build_int_add(left_int, right_int, "add")
                .map(Into::into)?,
            BinaryOp::Sub => self
                .builder
                .build_int_sub(left_int, right_int, "sub")
                .map(Into::into)?,
            BinaryOp::Mul => self
                .builder
                .build_int_mul(left_int, right_int, "mul")
                .map(Into::into)?,
            BinaryOp::Div => {
                if is_signed {
                    self.builder
                        .build_int_signed_div(left_int, right_int, "sdiv")
                        .map(Into::into)?
                } else {
                    self.builder
                        .build_int_unsigned_div(left_int, right_int, "udiv")
                        .map(Into::into)?
                }
            }
            BinaryOp::Mod => {
                if is_signed {
                    self.builder
                        .build_int_signed_rem(left_int, right_int, "srem")
                        .map(Into::into)?
                } else {
                    self.builder
                        .build_int_unsigned_rem(left_int, right_int, "urem")
                        .map(Into::into)?
                }
            }
            BinaryOp::And => self
                .builder
                .build_and(left_int, right_int, "and")
                .map(Into::into)?,
            BinaryOp::Or => self
                .builder
                .build_or(left_int, right_int, "or")
                .map(Into::into)?,
            BinaryOp::Xor => self
                .builder
                .build_xor(left_int, right_int, "xor")
                .map(Into::into)?,
            BinaryOp::LeftShift => self
                .builder
                .build_left_shift(left_int, right_int, "shl")
                .map(Into::into)?,
            BinaryOp::RightShift => {
                if is_signed {
                    self.builder
                        .build_right_shift(left_int, right_int, true, "ashr")
                        .map(Into::into)?
                } else {
                    self.builder
                        .build_right_shift(left_int, right_int, false, "lshr")
                        .map(Into::into)?
                }
            }
            BinaryOp::LogicalAnd => {
                let is_zero = self.builder.build_int_compare(
                    inkwell::IntPredicate::EQ,
                    left_int,
                    left_int.get_type().const_zero(),
                    "land_l",
                )?;
                let right_is_nonzero = self.builder.build_int_compare(
                    inkwell::IntPredicate::NE,
                    right_int,
                    right_int.get_type().const_zero(),
                    "land_r",
                )?;
                let i1_false = self.context.bool_type().const_int(0, false);
                let result = self.builder.build_select(
                    is_zero,
                    i1_false,
                    right_is_nonzero,
                    "landtmp",
                )?;
                self.builder.build_int_z_extend(
                    result.into_int_value(),
                    self.context.i32_type(),
                    "landtmp_ext",
                )?.into()
            }
            BinaryOp::LogicalOr => {
                let is_nonzero = self.builder.build_int_compare(
                    inkwell::IntPredicate::NE,
                    left_int,
                    left_int.get_type().const_zero(),
                    "lor_l",
                )?;
                let right_is_nonzero = self.builder.build_int_compare(
                    inkwell::IntPredicate::NE,
                    right_int,
                    right_int.get_type().const_zero(),
                    "lor_r",
                )?;
                let i1_true = self.context.bool_type().const_int(1, false);
                let result = self.builder.build_select(
                    is_nonzero,
                    i1_true,
                    right_is_nonzero,
                    "lortmp",
                )?;
                self.builder.build_int_z_extend(
                    result.into_int_value(),
                    self.context.i32_type(),
                    "lortmp_ext",
                )?.into()
            }
        };
        Ok(res)
    }

    fn generate_float_binary_op(
        &mut self,
        left: &Expression,
        op: &BinaryOp,
        right: &Expression,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let left_float = self.generate_expression(left)?.into_float_value();
        let right_float =self.generate_expression(right)?.into_float_value();
        match op {
            BinaryOp::Add => Ok(self
                .builder
                .build_float_add(left_float, right_float, "fadd")
                .map(Into::into)?),
            BinaryOp::Sub => Ok(self
                .builder
                .build_float_sub(left_float, right_float, "fsub")
                .map(Into::into)?),
            BinaryOp::Mul => Ok(self
                .builder
                .build_float_mul(left_float, right_float, "fmul")
                .map(Into::into)?),
            BinaryOp::Div => Ok(self
                .builder
                .build_float_div(left_float, right_float, "fdiv")
                .map(Into::into)?),
            _ => Err(CodegenError::InvalidOperation(
                format!("Operator {op:?} not supported for floats"),
                left.pos,
            ))
        }
    }

    fn generate_pointer_binary_op(&mut self, left: &Expression, op: &BinaryOp, right: &Expression) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        if right.expr_type.pointer_depth > 0 {
            return Err(CodegenError::InvalidOperation(
                "Pointer arithmetic only allowed with integers".to_string(),
                left.pos,
            ));
        }
        let left_ptr = self.generate_expression(left)?.into_pointer_value();
        let right_int = self.generate_expression(right)?.into_int_value();
        let pointee_type = lang_type_to_llvm(self.context, &LangType {
            base: left.expr_type.base,
            size_bits: left.expr_type.size_bits,
            pointer_depth: left.expr_type.pointer_depth - 1,
            is_const: left.expr_type.is_const,
            array_size: None,
        })?;
        
        match op {
            BinaryOp::Add => unsafe {
                Ok(self.builder.build_gep(pointee_type, left_ptr, &[right_int], "ptr_add")
                    .map(Into::into)?)
            },
            BinaryOp::Sub => {
                let neg_right = self.builder
                    .build_int_neg(right_int, "neg")
                    ?;
                unsafe {
                    Ok(self.builder.build_gep(pointee_type, left_ptr, &[neg_right], "ptr_sub")
                        .map(Into::into)?)
                }
            },
            _ => Err(CodegenError::InvalidOperation(
                format!("Operator {op:?} not supported for pointers"),
                left.pos,
            )),
        }
    }

    /// Generate a binary operation
    fn generate_binary_op(
        &mut self,
        left: &Expression,
        op: &BinaryOp,
        right: &Expression,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        // Determine if we're working with floats or ints
        let is_float = matches!(left.expr_type.base, TypeBase::SFloat);

        // Pointers are special. They should only be allowed to be manipulated by integers, and you can only do addition and subtraction.
        let is_pointer = left.expr_type.pointer_depth > 0;

        if is_float {
            self.generate_float_binary_op(left, op, right)
        } else if is_pointer {
            self.generate_pointer_binary_op(left, op, right)
        } else {
            self.generate_int_binary_op(left, op, right)
        }
    }

    /// Generate a comparison
    fn generate_comparison(
        &mut self,
        left: &Expression,
        op: &ComparisonOp,
        right: &Expression,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let left_val = self.generate_expression(left)?;
        let right_val = self.generate_expression(right)?;

        let is_float = matches!(left.expr_type.base, TypeBase::SFloat);

        if is_float {
            let mut left_float = left_val.into_float_value();
            let mut right_float = right_val.into_float_value();

            // Ensure both floats have the same type (f32 vs f64)
            if left_float.get_type() != right_float.get_type() {
                let left_bits = if left_float.get_type() == self.context.f32_type() { 32 } else { 64 };
                let right_bits = if right_float.get_type() == self.context.f32_type() { 32 } else { 64 };
                if left_bits > right_bits {
                    eprintln!("warning: implicit cast: {} ({}-bit) widened to {}-bit to match left operand in comparison at {}:{}",
                        right.expr_type, right_bits, left_bits, right.pos.line, right.pos.column);
                    right_float = self.builder.build_float_ext(right_float, left_float.get_type(), "fpext")?;
                } else {
                    eprintln!("warning: implicit cast: {} ({}-bit) widened to {}-bit to match right operand in comparison at {}:{}",
                        left.expr_type, left_bits, right_bits, left.pos.line, left.pos.column);
                    left_float = self.builder.build_float_ext(left_float, right_float.get_type(), "fpext")?;
                }
            }

            let predicate = match op {
                ComparisonOp::Equal => inkwell::FloatPredicate::OEQ,
                ComparisonOp::NotEqual => inkwell::FloatPredicate::ONE,
                ComparisonOp::Less => inkwell::FloatPredicate::OLT,
                ComparisonOp::Greater => inkwell::FloatPredicate::OGT,
                ComparisonOp::LessEqual => inkwell::FloatPredicate::OLE,
                ComparisonOp::GreaterEqual => inkwell::FloatPredicate::OGE,
            };

            let cmp = self
                .builder
                .build_float_compare(predicate, left_float, right_float, "fcmp")
                ?;

            // Extend to i32
            Ok(self.builder
                .build_int_z_extend(cmp, self.context.i32_type(), "cmp_ext")
                .map(Into::into)?)
        } else {
            let mut left_int = left_val.into_int_value();
            let mut right_int = right_val.into_int_value();
            let is_signed = matches!(left.expr_type.base, TypeBase::SInt);

            // Ensure both integers have the same bit width
            if left_int.get_type().get_bit_width() != right_int.get_type().get_bit_width() {
                let left_bits = left_int.get_type().get_bit_width();
                let right_bits = right_int.get_type().get_bit_width();
                let right_is_signed = matches!(right.expr_type.base, TypeBase::SInt);
                if left_bits > right_bits {
                    eprintln!("warning: implicit cast: {} ({}-bit) widened to {}-bit to match left operand in comparison at {}:{}",
                        right.expr_type, right_bits, left_bits, right.pos.line, right.pos.column);
                    right_int = if right_is_signed {
                        self.builder.build_int_s_extend(right_int, left_int.get_type(), "sext")?
                    } else {
                        self.builder.build_int_z_extend(right_int, left_int.get_type(), "zext")?
                    };
                } else {
                    eprintln!("warning: implicit cast: {} ({}-bit) widened to {}-bit to match right operand in comparison at {}:{}",
                        left.expr_type, left_bits, right_bits, left.pos.line, left.pos.column);
                    left_int = if is_signed {
                        self.builder.build_int_s_extend(left_int, right_int.get_type(), "sext")?
                    } else {
                        self.builder.build_int_z_extend(left_int, right_int.get_type(), "zext")?
                    };
                }
            }

            let predicate = match op {
                ComparisonOp::Equal => IntPredicate::EQ,
                ComparisonOp::NotEqual => IntPredicate::NE,
                ComparisonOp::Less => {
                    if is_signed {
                        IntPredicate::SLT
                    } else {
                        IntPredicate::ULT
                    }
                }
                ComparisonOp::Greater => {
                    if is_signed {
                        IntPredicate::SGT
                    } else {
                        IntPredicate::UGT
                    }
                }
                ComparisonOp::LessEqual => {
                    if is_signed {
                        IntPredicate::SLE
                    } else {
                        IntPredicate::ULE
                    }
                }
                ComparisonOp::GreaterEqual => {
                    if is_signed {
                        IntPredicate::SGE
                    } else {
                        IntPredicate::UGE
                    }
                }
            };

            let cmp = self
                .builder
                .build_int_compare(predicate, left_int, right_int, "icmp")
                ?;

            // Extend to i32
            Ok(self.builder
                .build_int_z_extend(cmp, self.context.i32_type(), "cmp_ext")
                .map(Into::into)?)
        }
    }

    /// Generate a function call (expression context - must return a value)
    fn generate_function_call(
        &mut self,
        name: &str,
        args: &[Expression],
        pos: crate::lexer::Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let function = *self
            .functions
            .get(name)
            .ok_or_else(|| CodegenError::UndefinedFunction(name.to_string(), pos))?;

        let mut arg_values = Vec::new();
        for arg in args {
            let val = self.generate_expression(arg)?;
            arg_values.push(val.into());
        }

        let call_result = self
            .builder
            .build_call(function, &arg_values, "call")
            ?;

        // Extract BasicValueEnum from the call result
        call_result
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CodegenError::MissingReturn(name.to_string(), pos))
    }

    /// Generate a function call as a statement (void return is OK)
    fn generate_function_call_statement(
        &mut self,
        name: &str,
        args: &[Expression],
        pos: crate::lexer::Position,
    ) -> Result<(), CodegenError> {
        let function = *self
            .functions
            .get(name)
            .ok_or_else(|| CodegenError::UndefinedFunction(name.to_string(), pos))?;

        let mut arg_values = Vec::new();
        for arg in args {
            let val = self.generate_expression(arg)?;
            arg_values.push(val.into());
        }

        self.builder
            .build_call(function, &arg_values, "call")
            ?;

        Ok(())
    }

    /// Generate a type cast
    fn generate_cast(
        &mut self,
        expr: &Expression,
        target_type: &crate::lexer::LangType,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let value = self.generate_expression(expr)?;
        let target_llvm_type = lang_type_to_llvm(self.context, target_type)?;
        self.cast_value(value, target_llvm_type, &expr.expr_type, target_type)
    }

    /// Cast a value to a target LLVM type
    fn cast_value(
        &self,
        value: BasicValueEnum<'ctx>,
        target_llvm_type: BasicTypeEnum<'ctx>,
        source_lang_type: &crate::lexer::LangType,
        target_lang_type: &crate::lexer::LangType,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        // If types already match, no cast needed
        if value.get_type() == target_llvm_type {
            return Ok(value);
        }

        // Determine target lang type properties from LLVM type
        let target_is_pointer = matches!(target_llvm_type, BasicTypeEnum::PointerType(_));
        let target_is_float = matches!(target_llvm_type, BasicTypeEnum::FloatType(_));
        let target_is_int = matches!(target_llvm_type, BasicTypeEnum::IntType(_));

        // Handle pointer casts
        if target_is_pointer {
            return if source_lang_type.pointer_depth == 0 {
                Ok(self
                    .builder
                    .build_int_to_ptr(
                        value.into_int_value(),
                        target_llvm_type.into_pointer_type(),
                        "inttoptr",
                    )?.into())
            } else {
                Ok(self
                    .builder
                    .build_pointer_cast(
                        value.into_pointer_value(),
                        target_llvm_type.into_pointer_type(),
                        "ptrcast",
                    )?.into())
            }
        }

        // Handle int to float
        if target_is_float && value.is_int_value() {
            let int_val = value.into_int_value();
            let is_signed = matches!(source_lang_type.base, TypeBase::SInt);

            return Ok(if is_signed {
                self.builder
                    .build_signed_int_to_float(
                        int_val,
                        target_llvm_type.into_float_type(),
                        "sitofp",
                    )
                    .map(Into::into)?
            } else {
                self.builder
                    .build_unsigned_int_to_float(
                        int_val,
                        target_llvm_type.into_float_type(),
                        "uitofp",
                    )
                    .map(Into::into)?
            });
        }

        // Handle float to int (RT-3: use target type's signedness)
        if target_is_int && value.is_float_value() {
            let float_val = value.into_float_value();
            let target_int_type = target_llvm_type.into_int_type();
            let target_is_signed = matches!(target_lang_type.base, TypeBase::SInt);
            return Ok(if target_is_signed {
                self.builder
                    .build_float_to_signed_int(float_val, target_int_type, "fptosi")
                    .map(Into::into)?
            } else {
                self.builder
                    .build_float_to_unsigned_int(float_val, target_int_type, "fptoui")
                    .map(Into::into)?
            });
        }

        // Handle pointer to int
        if target_is_int && value.is_pointer_value() {
            let ptr_val = value.into_pointer_value();
            let target_int_type = target_llvm_type.into_int_type();
            return Ok(self.builder
                .build_ptr_to_int(ptr_val, target_int_type, "ptrtoint")
                .map(Into::into)?);
        }

        // Handle int to int (resize)
        if target_is_int && value.is_int_value() {
            let int_val = value.into_int_value();
            let target_int_type = target_llvm_type.into_int_type();
            let source_bits = int_val.get_type().get_bit_width();
            let target_bits = target_int_type.get_bit_width();
            let is_signed = matches!(source_lang_type.base, TypeBase::SInt);

            return match target_bits.cmp(&source_bits) {
                std::cmp::Ordering::Greater => {
                    // Extend
                    Ok(if is_signed {
                        self.builder
                            .build_int_s_extend(int_val, target_int_type, "sext")
                            .map(Into::into)?
                    } else {
                        self.builder
                            .build_int_z_extend(int_val, target_int_type, "zext")
                            .map(Into::into)?
                    })
                }
                std::cmp::Ordering::Less => {
                    // Truncate
                    Ok(self.builder
                        .build_int_truncate(int_val, target_int_type, "trunc")
                        .map(Into::into)?)
                }
                std::cmp::Ordering::Equal => {
                    // Same size, no cast needed
                    Ok(value)
                }
            };
        }

        // If we can't handle the cast, return the value as-is
        Ok(value)
    }

    /// Generate a constant expression (for global initializers)
    fn generate_constant_expression(
        &mut self,
        expr: &Expression,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        match &expr.kind {
            ExprKind::Literal(lit) => self.generate_constant_literal(lit, &expr.expr_type),
            ExprKind::Alloc { alloc_type: lang_type, count } => self.generate_alloc(lang_type, count),
            _ => Err(CodegenError::InvalidOperation(
                "Non-constant expression in global initializer".to_string(),
                expr.pos,
            )),
        }
    }

    /// Generate a constant literal (without using the builder)
    #[allow(clippy::cast_sign_loss)]
    fn generate_constant_literal(
        &self,
        lit: &LiteralValue,
        ty: &crate::lexer::LangType,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        match lit {
            LiteralValue::Integer(val) => {
                let llvm_type = lang_type_to_llvm(self.context, ty)?;
                match llvm_type {
                    BasicTypeEnum::IntType(int_ty) => {
                        Ok(int_ty.const_int(*val as u64, true).into())
                    }
                    _ => Err(CodegenError::TypeError(
                        "Integer literal must have integer type".to_string(),
                        crate::lexer::Position::new(0, 0),
                    )),
                }
            }

            LiteralValue::Float(val) => {
                let llvm_type = lang_type_to_llvm(self.context, ty)?;
                match llvm_type {
                    BasicTypeEnum::FloatType(float_ty) => Ok(float_ty.const_float(*val).into()),
                    _ => Err(CodegenError::TypeError(
                        "Float literal must have float type".to_string(),
                        crate::lexer::Position::new(0, 0),
                    )),
                }
            }

            LiteralValue::String(index) => {
                let string_name = format!(".str.{index}");
                let global_info = self.global_variables.get(&string_name).expect(
                    "Internal error: String literal global not found",
                );

                let i8_ptr_type = self.context.ptr_type(AddressSpace::default());
                Ok(global_info.ptr.const_cast(i8_ptr_type).into())
            }
        }
    }

    /// Convert a value to a boolean (i1) for conditionals
    fn value_to_bool(&self, value: BasicValueEnum<'ctx>) -> Result<IntValue<'ctx>, CodegenError> {
        if value.is_int_value() {
            let int_val = value.into_int_value();
            // Compare with zero
            let zero = int_val.get_type().const_zero();
            Ok(self.builder
                .build_int_compare(IntPredicate::NE, int_val, zero, "tobool")?)
        } else if value.is_float_value() {
            let float_val = value.into_float_value();
            let zero = float_val.get_type().const_zero();
            Ok(self.builder
                .build_float_compare(inkwell::FloatPredicate::ONE, float_val, zero, "tobool")?)
        } else {
            Err(CodegenError::TypeError(
                "Cannot convert value to boolean".to_string(),
                crate::lexer::Position::new(0, 0),
            ))
        }
    }

    /// Check if the current block has a terminator
    fn block_has_terminator(&self) -> bool {
        self.builder
            .get_insert_block()
            .and_then(inkwell::basic_block::BasicBlock::get_terminator)
            .is_some()
    }

    /// Get a zero value for a type
    fn get_zero_value(
        &self,
        ty: &crate::lexer::LangType,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let llvm_type = lang_type_to_llvm(self.context, ty)?;
        Ok(llvm_type.const_zero())
    }

    // Scope management (RT-11: unified variable tracking)
    fn enter_scope(&mut self) {
        self.variables.push(HashMap::new());
    }

    fn exit_scope(&mut self) {
        self.variables.pop();
    }

    fn add_variable(&mut self, name: String, ptr: PointerValue<'ctx>, llvm_type: BasicTypeEnum<'ctx>, lang_type: LangType) {
        if let Some(scope) = self.variables.last_mut() {
            scope.insert(name, LocalVar { ptr, llvm_type, lang_type });
        }
    }

    fn lookup_var_info(&self, name: &str) -> Option<LocalVar<'ctx>> {
        for scope in self.variables.iter().rev() {
            if let Some(var) = scope.get(name) {
                return Some(LocalVar { ptr: var.ptr, llvm_type: var.llvm_type, lang_type: var.lang_type });
            }
        }
        self.global_variables.get(name).map(|g| LocalVar { ptr: g.ptr, llvm_type: g.llvm_type, lang_type: g.lang_type })
    }

    /// Get the LLVM module
    pub fn module(&self) -> &Module<'ctx> {
        &self.module
    }

    /// Get a target machine for the current platform
    /// 
    /// # Errors
    /// Returns `CodegenError` if the target machine cannot be created
    /// 
    /// # Panics
    /// Panics if target machine creation fails unexpectedly
    pub fn get_target_machine(&self) -> Result<TargetMachine, CodegenError> {
        let opt = OptimizationLevel::Default;
        let reloc = RelocMode::Default;
        let model = CodeModel::Default;
        let target_machine = self
            .target
            .create_target_machine(
                &TargetMachine::get_default_triple(),
                "generic",
                "",
                opt,
                reloc,
                model,
            )
            .context("failed to create target machine").unwrap();
        Ok(target_machine)
    }

    /// Run optimization passes on the module
    /// 
    /// # Arguments
    /// * `level` - Optimization level (0-3), where:
    ///   - 0: No optimizations (default)
    ///   - 1: Basic optimizations
    ///   - 2: Standard optimizations (recommended for release)
    ///   - 3: Aggressive optimizations
    /// 
    /// # Errors
    /// Returns `CodegenError` if the passes fail to run
    pub fn optimize(&self, level: u8) -> Result<(), CodegenError> {
        if level == 0 {
            return Ok(());
        }

        let target_machine = self.get_target_machine()?;
        
        // Build the pass pipeline string based on optimization level
        let passes = match level {
            1 => "default<O1>",
            3 => "default<O3>",
            _ => "default<O2>", // 2 or any other value defaults to O2
        };

        let pass_options = PassBuilderOptions::create();
        pass_options.set_verify_each(true);
        pass_options.set_loop_interleaving(true);
        pass_options.set_loop_vectorization(true);
        pass_options.set_loop_unrolling(true);
        pass_options.set_merge_functions(true);

        self.module
            .run_passes(passes, &target_machine, pass_options)
            .map_err(|e| CodegenError::InvalidOperation(
                format!("Failed to run optimization passes: {}", e.to_string()),
                crate::lexer::Position { line: 0, column: 0 },
            ))
    }

    /// Print the LLVM IR to a string
    pub fn print_ir_to_string(&self) -> String {
        self.module.print_to_string().to_string()
    }

    /// Write LLVM IR to a file
    /// # Panics
    /// When writing to the file fails
    /// # Errors
    /// Never
    pub fn write_ir_to_file(&self, path: &std::path::Path) -> Result<(), CodegenError> {
        self.module
            .print_to_file(path).expect("Failed to write LLVM IR to file");
        Ok(())
    }
    
fn generate_alloc(&mut self, alloc_type: &LangType, count: &Expression) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    if self.current_function.is_none() {
        // --- GLOBAL ALLOCATION ---
        match count.kind {
            ExprKind::Literal(LiteralValue::Integer(val)) => {
                let llvm_type = lang_type_to_llvm(self.context, alloc_type)?;
                
                // Safety check for size
                let array_size = u32::try_from(val).map_err(|_| CodegenError::InvalidOperation(
                    "Global allocation size too large.".to_string(),
                    count.pos,
                ))?;

                // For globals, we MUST create an ArrayType because globals are not dynamic.
                // e.g., [4 x i32]
                let array_type = llvm_type.array_type(array_size);
                
                let global = self.module.add_global(array_type, None, ".global_alloc");
                global.set_initializer(&array_type.const_zero());
                
                Ok(global.as_pointer_value().into())
            }
            _ => Err(CodegenError::InvalidOperation(
                "Global allocation count must be a constant integer".to_string(),
                count.pos,
            ))
        }
    } else {
        let count_value = self.generate_expression(count)?;
        let count_int = count_value.into_int_value();
        let llvm_type = lang_type_to_llvm(self.context, alloc_type)?;
        let alloca = self.builder.build_array_alloca(llvm_type, count_int, "alloca")
            .map_err(|_| CodegenError::InvalidOperation("Failed to build alloca".to_string(), count.pos))?;
        
        Ok(alloca.into())
    }
}
}
