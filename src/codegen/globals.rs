use inkwell::module::Linkage;
use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, FloatValue, IntValue, PointerValue, StructValue};
use inkwell::AddressSpace;

use crate::lexer::Position;
use crate::codegen::const_eval::const_eval;
use crate::codegen::generator::CodeGenerator;
use crate::codegen::scope::GlobalVarInfo;
use crate::codegen::CodegenError;
use crate::parser::{ExprKind, Expression, GlobalVar, LangType};

impl<'ctx> CodeGenerator<'ctx> {
    pub(crate) fn generate_global_variable(
        &mut self,
        global: &GlobalVar,
    ) -> Result<(), CodegenError> {
        let (global_type, _is_array) = if global.var_type.is_array() {
            // Cache-aware: resolves type-struct elements too.
            (
                self.lang_type_to_llvm_array(&global.var_type)
                    .map_err(|e| e.with_pos(global.pos))?
                    .into(),
                true,
            )
        } else {
            (
                self.lang_type_to_llvm(&global.var_type)
                    .map_err(|e| e.with_pos(global.pos))?,
                false,
            )
        };

        let global_var =
            self.module
                .add_global(global_type, Some(AddressSpace::default()), &global.name);

        // Linkage follows `export` (foreign-visible), not `public` (Aspect
        // module visibility): a `public` global stays internally linked so
        // `globaldce` can strip it when unused.
        if global.export {
            global_var.set_linkage(Linkage::External);
        } else {
            global_var.set_linkage(Linkage::Private);
        }

        if let Some(init_expr) = &global.initializer {
            // A global initializer legitimately reads another global's start
            // value, which is why declaration order is significant here. Flag
            // the context so `const_eval` folds those references (it refuses
            // them for a runtime/local initializer).
            let prev_in_global_init = self.in_global_init;
            self.in_global_init = true;
            let folded = if let ExprKind::ListInitializer(elements) = &init_expr.kind {
                self.generate_constant_array_value(&global.var_type, elements, global.pos)
            } else {
                // Cast the constant to the declared global type if widths differ
                // (e.g. integer literal emitted as i32 into a u8/i16/i64 global).
                const_eval(init_expr, self)
                    .map(|v| coerce_constant_to_type(v, global_type))
            };
            self.in_global_init = prev_in_global_init;
            global_var.set_initializer(&folded?);
        } else {
            global_var.set_initializer(&global_type.const_zero());
        }

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

    /// The single authority for the string-literal naming scheme shared with
    /// `emit_string_ptr`.
    pub(crate) fn string_literal_name(index: usize) -> String {
        format!(".str.{index}")
    }

    pub(crate) fn generate_string_literal(&mut self, index: usize, value: &str) {
        let string_name = Self::string_literal_name(index);
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
                lang_type: LangType::U8_PTR,
            },
        );
    }

    pub(crate) fn generate_constant_array_value(
        &mut self,
        var_type: &LangType,
        elements: &[Expression],
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let elem_lang_type = var_type.element_type();
        // Cache-aware: resolves type-struct elements through the named-struct
        // cache (the context-only `to_llvm` can't).
        let elem_llvm_type = self
            .lang_type_to_llvm(&elem_lang_type)
            .map_err(|e| e.with_pos(pos))?;
        let array_size = var_type.array_size.unwrap_or(0) as usize;

        // Fold every element via the const-evaluator; a genuinely non-constant
        // element surfaces its own error.
        let mut const_vals: Vec<BasicValueEnum<'ctx>> = Vec::with_capacity(array_size);
        for elem in elements {
            const_vals.push(const_eval(elem, self)?);
        }

        while const_vals.len() < array_size {
            const_vals.push(elem_llvm_type.const_zero());
        }

        // Literals are emitted at their natural width (e.g. i32), so cast each
        // value to the exact element type before building the array.
        match elem_llvm_type {
            BasicTypeEnum::IntType(int_ty) => {
                let vals: Vec<IntValue> = const_vals
                    .iter()
                    .map(|v| {
                        coerce_constant_to_type(*v, elem_llvm_type).into_int_value()
                    })
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
            // Struct-literal elements fold (via `const_eval`) to
            // `const_named_struct` values of this same cached struct type;
            // assemble them into a `[N x %T]` ConstantArray.
            BasicTypeEnum::StructType(struct_ty) => {
                let vals: Vec<StructValue> =
                    const_vals.iter().map(|v| v.into_struct_value()).collect();
                Ok(struct_ty.const_array(&vals).into())
            }
            _ => Err(CodegenError::InvalidOperation(
                format!("unsupported element type for constant array: {elem_llvm_type}"),
                pos,
            )),
        }
    }
}

/// Casts an integer constant to `target` if widths differ — the only case
/// where natural-width literals can mismatch. Floats/pointers pass through.
fn coerce_constant_to_type<'ctx>(
    val: BasicValueEnum<'ctx>,
    target: BasicTypeEnum<'ctx>,
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
