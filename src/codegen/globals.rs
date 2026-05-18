use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::{BasicValueEnum, FloatValue, IntValue, PointerValue};
use inkwell::AddressSpace;

use crate::codegen::expressions::{walk_expression, EmitMode};
use crate::codegen::generator::CodeGenerator;
use crate::codegen::scope::GlobalVarInfo;
use crate::codegen::{CodegenError, LangTypeExt};
use crate::lexer::TypeBase;
use crate::parser::{ExprKind, Expression, GlobalVar, LangType, LiteralValue};

impl<'ctx> CodeGenerator<'ctx> {
    /// Generate a global variable
    pub(crate) fn generate_global_variable(
        &mut self,
        global: &GlobalVar,
    ) -> Result<(), CodegenError> {
        let (global_type, _is_array) = if global.var_type.is_array() {
            (global.var_type.to_llvm_array(self.context)?.into(), true)
        } else {
            (global.var_type.to_llvm(self.context)?, false)
        };

        let global_var =
            self.module
                .add_global(global_type, Some(AddressSpace::default()), &global.name);

        if let Some(init_expr) = &global.initializer {
            if let ExprKind::ListInitializer(elements) = &init_expr.kind {
                // Array literal initializer -> ConstantArray
                let const_array = self.generate_constant_array_value(&global.var_type, elements)?;
                global_var.set_initializer(&const_array);
            } else {
                let init_value = walk_expression(init_expr, self, EmitMode::Constant)?;
                global_var.set_initializer(&init_value);
            }
        } else {
            global_var.set_initializer(&global_type.const_zero());
        }

        self.scope.insert_global(
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
    pub(crate) fn generate_string_literal(&mut self, index: usize, value: &str) {
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
        self.scope.insert_global(
            string_name,
            GlobalVarInfo {
                ptr: global_string.as_pointer_value(),
                llvm_type: ptr_ty.into(),
                lang_type: LangType::new(TypeBase::UInt, 8, 1, false),
            },
        );
    }

    pub(crate) fn generate_constant_array_value(
        &mut self,
        var_type: &LangType,
        elements: &[Expression],
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let elem_lang_type = var_type.element_type();
        let elem_llvm_type = elem_lang_type.to_llvm(self.context)?;
        let array_size = var_type.array_size.unwrap_or(0) as usize;

        // Generate constant values for provided elements (must all be literals)
        let mut const_vals: Vec<BasicValueEnum<'ctx>> = Vec::with_capacity(array_size);
        for elem in elements {
            match &elem.kind {
                ExprKind::Literal(_) => {
                    const_vals.push(walk_expression(elem, self, EmitMode::Constant)?);
                }
                _ => {
                    return Err(CodegenError::InvalidOperation(
                        "constant array initializer elements must be literals".to_string(),
                        elem.pos,
                    ))
                }
            }
        }

        // Zero-pad to array_size
        while const_vals.len() < array_size {
            const_vals.push(elem_llvm_type.const_zero());
        }

        // Build ConstantArray for the element type
        match elem_llvm_type {
            BasicTypeEnum::IntType(int_ty) => {
                let vals: Vec<IntValue> = const_vals.iter().map(|v| v.into_int_value()).collect();
                Ok(int_ty.const_array(&vals).into())
            }
            BasicTypeEnum::FloatType(float_ty) => {
                let vals: Vec<FloatValue> =
                    const_vals.iter().map(|v| v.into_float_value()).collect();
                Ok(float_ty.const_array(&vals).into())
            }
            BasicTypeEnum::PointerType(ptr_ty) => {
                let vals: Vec<PointerValue> =
                    const_vals.iter().map(|v| v.into_pointer_value()).collect();
                Ok(ptr_ty.const_array(&vals).into())
            }
            _ => Err(CodegenError::InvalidOperation(
                format!("unsupported element type for constant array: {elem_llvm_type}"),
                crate::lexer::Position::new(0, 0),
            )),
        }
    }

    pub(crate) fn generate_list_initializer(
        &mut self,
        array_ptr: PointerValue<'ctx>,
        var_type: &LangType,
        elements: &[Expression],
    ) -> Result<(), CodegenError> {
        let elem_lang_type = var_type.element_type();
        let elem_llvm_type = elem_lang_type.to_llvm(self.context)?;
        let array_size = var_type.array_size.unwrap_or(0);
        let array_llvm_type = elem_llvm_type.array_type(array_size);

        // Empty initializer: zero the whole array
        if elements.is_empty() {
            self.builder
                .build_store(array_ptr, array_llvm_type.const_zero())?;
            return Ok(());
        }

        // Fast path: all elements are integer/float literals -> emit a single ConstantArray store
        let all_const = elements.iter().all(|e| {
            matches!(
                e.kind,
                ExprKind::Literal(LiteralValue::Integer(_) | LiteralValue::Float(_))
            )
        });

        if all_const {
            let const_val = self.generate_constant_array_value(var_type, elements)?;
            self.builder.build_store(array_ptr, const_val)?;
            return Ok(());
        }

        // Runtime path: store each element via two-index GEP [0, i]
        // This correctly addresses into a [N x elem] array pointer.
        // i.e gep(array_ptr, [0, i]) = &(*array_ptr)[i]
        for (i, elem_expr) in elements.iter().enumerate() {
            let zero = self.context.i64_type().const_int(0, false);
            let index = self.context.i64_type().const_int(i as u64, false);
            let elem_ptr = unsafe {
                self.builder.build_gep(
                    array_llvm_type,
                    array_ptr,
                    &[zero, index],
                    &format!("list_init.{i}"),
                )?
            };
            let value = self.generate_coerced_value(elem_expr, Some(&elem_lang_type))?;
            self.builder.build_store(elem_ptr, value)?;
        }

        // Zero-fill any remaining slots
        let zero_val = elem_llvm_type.const_zero();
        for i in elements.len()..array_size as usize {
            let zero = self.context.i64_type().const_int(0, false);
            let index = self.context.i64_type().const_int(i as u64, false);
            let elem_ptr = unsafe {
                self.builder.build_gep(
                    array_llvm_type,
                    array_ptr,
                    &[zero, index],
                    &format!("list_init_zero.{i}"),
                )?
            };
            self.builder.build_store(elem_ptr, zero_val)?;
        }
        Ok(())
    }
}
