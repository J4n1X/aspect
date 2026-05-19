use inkwell::types::{BasicTypeEnum};
use inkwell::values::{BasicValueEnum, FloatValue, IntValue, PointerValue};
use inkwell::AddressSpace;

use crate::codegen::expressions::{walk_expression, EmitMode};
use crate::codegen::generator::CodeGenerator;
use crate::codegen::scope::GlobalVarInfo;
use crate::codegen::{CodegenError, LangTypeExt};
use crate::lexer::TypeBase;
use crate::parser::{ExprKind, Expression, GlobalVar, LangType};

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
                // Cast the constant to the declared global type if widths differ
                // (e.g. integer literal emitted as i32 into a u8/i16/i64 global).
                let coerced = coerce_constant_to_type(init_value, global_type, self.context);
                global_var.set_initializer(&coerced);
            }
        } else {
            global_var.set_initializer(&global_type.const_zero());
        }

        // Check if the global is constant, and set the LLVM global accordingly.
        if global.var_type.is_const {
            global_var.set_constant(true);
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

        // Build ConstantArray for the element type.
        // Literals are emitted at their natural width (e.g. i32), so cast each
        // value to the exact element type before building the array.
        match elem_llvm_type {
            BasicTypeEnum::IntType(int_ty) => {
                let vals: Vec<IntValue> = const_vals
                    .iter()
                    .map(|v| coerce_constant_to_type(*v, elem_llvm_type, self.context).into_int_value())
                    .collect();
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
}

/// Cast a constant `BasicValueEnum` to `target_type` if the widths differ.
/// Only handles integer types (the only case where natural-width literals can mismatch).
/// Float and pointer constants are returned unchanged.
fn coerce_constant_to_type<'ctx>(
    val: BasicValueEnum<'ctx>,
    target: BasicTypeEnum<'ctx>,
    _ctx: &'ctx inkwell::context::Context,
) -> BasicValueEnum<'ctx> {
    if val.get_type() == target {
        return val;
    }
    if let (BasicValueEnum::IntValue(iv), BasicTypeEnum::IntType(int_ty)) = (val, target) {
        let raw = iv.get_zero_extended_constant().unwrap_or(0);
        return int_ty.const_int(raw, false).into();
    }
    val
}
