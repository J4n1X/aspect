use inkwell::types::BasicType;
use inkwell::values::{BasicValueEnum, FunctionValue};

use crate::codegen::generator::CodeGenerator;
use crate::codegen::{CodegenError, LangTypeExt};
use crate::lexer::Position;
use crate::parser::{Expression, Function, LangType};

/// RAII guard that sets `current_function` / `current_function_return_type`
/// on creation and clears them on drop.
pub(crate) struct FunctionScope<'a, 'ctx> {
    gen: &'a mut CodeGenerator<'ctx>,
}

impl<'a, 'ctx> FunctionScope<'a, 'ctx> {
    fn new(
        gen: &'a mut CodeGenerator<'ctx>,
        func: FunctionValue<'ctx>,
        return_type: LangType,
    ) -> Self {
        gen.current_function = Some(func);
        gen.current_function_return_type = Some(return_type);
        Self { gen }
    }
}

impl Drop for FunctionScope<'_, '_> {
    fn drop(&mut self) {
        self.gen.current_function = None;
        self.gen.current_function_return_type = None;
    }
}

impl<'ctx> CodeGenerator<'ctx> {
    fn generate_call_args(
        &mut self,
        name: &str,
        args: &[Expression],
    ) -> Result<Vec<inkwell::values::BasicMetadataValueEnum<'ctx>>, CodegenError> {
        let param_types = self
            .function_lang_params
            .get(name)
            .cloned()
            .unwrap_or_default();

        let mut arg_values = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            let target_ty = param_types.get(i);
            let val = self.generate_coerced_value(arg, target_ty)?;
            arg_values.push(val.into());
        }
        Ok(arg_values)
    }

    /// Declare a function (without body)
    pub(crate) fn declare_function(
        &mut self,
        func: &Function,
    ) -> Result<FunctionValue<'ctx>, CodegenError> {
        // Collect parameter LangTypes for call-site coercion
        let param_lang_types: Vec<LangType> = func.proto.params.iter().map(|(ty, _)| *ty).collect();

        // Convert parameter types to LLVM
        let param_types: Result<Vec<_>, _> = func
            .proto
            .params
            .iter()
            .map(|(ty, _)| ty.to_llvm(self.context))
            .collect();
        let param_types = param_types?;

        // Convert return type
        let return_type = if func.proto.return_type.is_void() {
            None
        } else {
            Some(func.proto.return_type.to_llvm(self.context)?)
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
        self.function_lang_params
            .insert(func.proto.name.clone(), param_lang_types);
        Ok(function)
    }

    /// Generate code for a statement
    pub(crate) fn generate_function(&mut self, func: &Function) -> Result<(), CodegenError> {
        let function = *self.functions.get(&func.proto.name).ok_or_else(|| {
            CodegenError::UndefinedFunction(func.proto.name.clone(), func.proto.pos)
        })?;

        let mut scope = FunctionScope::new(self, function, func.proto.return_type);
        let gen = &mut scope.gen;

        // Create entry block
        let entry_block = gen.context.append_basic_block(function, "entry");
        gen.builder.position_at_end(entry_block);

        // Enter function scope
        gen.enter_scope();

        // Allocate space for parameters and store them (in the entry block)
        for (i, (param_type, param_name)) in func.proto.params.iter().enumerate() {
            let param_value = function
                .get_nth_param(u32::try_from(i).expect("Parameter index out of bounds"))
                .unwrap();
            let param_llvm_type = param_type.to_llvm(gen.context)?;

            let alloca = gen.builder.build_alloca(param_llvm_type, param_name)?;
            gen.builder.build_store(alloca, param_value)?;

            gen.add_variable(
                param_name.clone(),
                alloca,
                param_llvm_type,
                *param_type,
                None,
            );
        }

        // Generate function body (variables are allocated at their declaration site)
        for stmt in &func.body {
            gen.generate_statement(stmt)?;
        }

        // If function doesn't have an explicit return, add one
        if !gen.block_has_terminator() {
            if func.proto.return_type.is_void() {
                gen.builder.build_return(None)?;
            } else {
                let zero = gen.get_zero_value(&func.proto.return_type)?;
                gen.builder.build_return(Some(&zero))?;
            }
        }

        gen.exit_scope();
        // FunctionScope::drop() clears current_function + current_function_return_type

        Ok(())
    }

    /// Generate a function call as an expression (must return a non-void value).
    pub(crate) fn generate_function_call(
        &mut self,
        name: &str,
        args: &[Expression],
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let function = *self
            .functions
            .get(name)
            .ok_or_else(|| CodegenError::UndefinedFunction(name.to_string(), pos))?;

        let arg_values = self.generate_call_args(name, args)?;

        let call_result = self.builder.build_call(function, &arg_values, "call")?;
        call_result
            .try_as_basic_value()
            .basic()
            .ok_or_else(|| CodegenError::MissingReturn(name.to_string(), pos))
    }

    /// Generate a function call as a statement (void returns are acceptable).
    pub(crate) fn generate_function_call_statement(
        &mut self,
        name: &str,
        args: &[Expression],
        pos: Position,
    ) -> Result<(), CodegenError> {
        let function = *self
            .functions
            .get(name)
            .ok_or_else(|| CodegenError::UndefinedFunction(name.to_string(), pos))?;

        let arg_values = self.generate_call_args(name, args)?;

        self.builder.build_call(function, &arg_values, "call")?;
        Ok(())
    }
}
