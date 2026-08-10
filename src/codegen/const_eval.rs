//! Compile-time constant expression evaluation.
//!
//! The constant-folding counterpart to `walk_expression`: folds the expression
//! tree in Rust and reconstructs LLVM constants via `ConstantEmitter`,
//! returning `Err` for any sub-expression with no constant-folding path. Drives
//! global initializers, `const`-local folding, and the constant array/struct
//! fast-paths.

use inkwell::AddressSpace;
use inkwell::values::BasicValueEnum;

use crate::codegen::CodegenError;
use crate::codegen::expressions::emit_binary_dispatch;
use crate::codegen::generator::CodeGenerator;
use crate::codegen::types::LangTypeExt;
use crate::codegen::value_emitter::ValueEmitter;
use crate::lexer::{LangType, Position, TypeBase};
use crate::parser::{ExprKind, Expression, LiteralValue};

/// Evaluate `expr` as a compile-time constant, producing an LLVM constant value.
///
/// Returns `Err` when `expr` (or any sub-expression) is not a compile-time constant.
pub(crate) fn const_eval<'ctx>(
    expr: &Expression,
    cg: &mut CodeGenerator<'ctx>,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    match &expr.kind {
        ExprKind::Literal(lit) => match lit {
            LiteralValue::Integer(val) => cg
                .constant_emitter()
                .emit_int_literal(*val, &expr.expr_type)
                .map_err(|e| e.with_pos(expr.pos)),
            LiteralValue::Float(val) => cg
                .constant_emitter()
                .emit_float_literal(*val, &expr.expr_type)
                .map_err(|e| e.with_pos(expr.pos)),
            // A string literal is a global pointer; cast it to `u8*` as a
            // link-time constant (no builder instruction).
            LiteralValue::String(index) => {
                let ptr = cg
                    .scope
                    .lookup_global(&CodeGenerator::string_literal_name(*index))
                    .expect("Internal error: String literal global not found")
                    .ptr;
                let i8_ptr_type = cg.context.ptr_type(AddressSpace::default());
                Ok(ptr.const_cast(i8_ptr_type).into())
            }
            // Boolean literal: an i1 value (zero-extended to i8 when stored).
            LiteralValue::Bool(b) => Ok(cg
                .context
                .bool_type()
                .const_int(u64::from(*b), false)
                .into()),
        },

        ExprKind::Variable(name) => {
            // Const locals store their folded value.
            for scope in cg.scope.iter_scopes() {
                if let Some(var) = scope.get(name) {
                    return var.const_value.ok_or_else(|| {
                        CodegenError::InvalidOperation(
                            format!("variable '{name}' is not a compile-time constant"),
                            expr.pos,
                        )
                    });
                }
            }
            let global_val = cg
                .module
                .get_global(name)
                .ok_or_else(|| CodegenError::UndefinedVariable(name.clone(), expr.pos))?;
            // A mutable global's runtime value is *not* its initializer, so it
            // can't be folded — except inside another global's initializer,
            // order-sensitive static init reading a prior start value, which
            // `in_global_init` marks. A `const` global is immutable everywhere.
            if !global_val.is_constant() && !cg.in_global_init {
                return Err(CodegenError::InvalidOperation(
                    format!("global '{name}' is not a compile-time constant"),
                    expr.pos,
                ));
            }
            global_val.get_initializer().ok_or_else(|| {
                CodegenError::InvalidOperation(
                    format!(
                        "global '{name}' has no constant initializer; \
                         declare it before referencing it in another global initializer"
                    ),
                    expr.pos,
                )
            })
        }

        ExprKind::Binary { left, op, right } => {
            let left_val = const_eval(left, cg)?;
            let right_val = const_eval(right, cg)?;

            // Pointer arithmetic lowers to a GEP — never a constant.
            if left.expr_type.pointer_depth > 0 || right.expr_type.pointer_depth > 0 {
                return Err(CodegenError::InvalidOperation(
                    "pointer arithmetic not supported in constant expressions".to_string(),
                    expr.pos,
                ));
            }

            emit_binary_dispatch(
                &cg.constant_emitter(),
                left_val,
                right_val,
                op,
                &left.expr_type,
                &right.expr_type,
                expr.pos,
            )
        }

        ExprKind::Comparison { .. } => Err(CodegenError::InvalidOperation(
            "comparison not supported in constant expressions".to_string(),
            expr.pos,
        )),

        ExprKind::Reference(inner) => match &inner.kind {
            ExprKind::Variable(name) => {
                let ptr = cg
                    .scope
                    .lookup_global(name.as_str())
                    .ok_or_else(|| CodegenError::UndefinedVariable(name.clone(), inner.pos))?
                    .ptr;
                Ok(ptr.into())
            }
            ExprKind::Dereference(inner2) => const_eval(inner2, cg),
            ExprKind::FieldAccess { .. } => Err(CodegenError::InvalidOperation(
                "address-of field not supported in constant expressions".to_string(),
                inner.pos,
            )),
            _ => Err(CodegenError::InvalidOperation(
                "Cannot take address of non-lvalue".to_string(),
                inner.pos,
            )),
        },

        ExprKind::Dereference(_) => Err(CodegenError::InvalidOperation(
            "dereference not supported in constant expressions".to_string(),
            expr.pos,
        )),

        ExprKind::FunctionCall { .. } => Err(CodegenError::InvalidOperation(
            "function calls not supported in constant expressions".to_string(),
            expr.pos,
        )),

        ExprKind::Cast {
            expr: inner,
            target_type,
        } => {
            let val = const_eval(inner, cg)?;
            let target_llvm = target_type
                .to_llvm(cg.context)
                .map_err(|e| e.with_pos(expr.pos))?;
            cg.constant_emitter()
                .emit_cast(val, target_llvm, &inner.expr_type, target_type, inner.pos)
        }

        ExprKind::Alloc { alloc_type, count } => cg.generate_alloc(alloc_type, count),

        ExprKind::UnaryNot(inner) => {
            let raw = const_eval(inner, cg)?;
            // `!p` on a pointer is a null test — runtime only, pointers don't fold.
            if raw.is_pointer_value() {
                return Err(CodegenError::InvalidOperation(
                    "logical NOT of a pointer not supported in constant expressions".to_string(),
                    inner.pos,
                ));
            }
            let val = raw.into_int_value();
            let n = val.get_zero_extended_constant().ok_or_else(|| {
                CodegenError::InvalidOperation(
                    "constant integer not representable as u64".to_string(),
                    inner.pos,
                )
            })?;
            Ok(cg
                .context
                .bool_type()
                .const_int(u64::from(n == 0), false)
                .into())
        }

        ExprKind::BitwiseNot(inner) => {
            let val = const_eval(inner, cg)?.into_int_value();
            Ok(val.const_not().into())
        }

        ExprKind::ListInitializer(_) => Err(CodegenError::InvalidOperation(
            "list initializer is only valid in a variable declaration".to_string(),
            expr.pos,
        )),

        ExprKind::FieldAccess { .. } => Err(CodegenError::InvalidOperation(
            "field access not supported in constant expressions".to_string(),
            expr.pos,
        )),

        ExprKind::SumConstruct {
            sum_id,
            variant,
            args,
        } => const_eval_sum_construct(sum_id, variant, args, expr.pos, cg),

        // No sum value exists at compile time to probe.
        ExprKind::Is { .. } | ExprKind::IsBinding { .. } => Err(CodegenError::InvalidOperation(
            "`is` is not supported in constant expressions".to_string(),
            expr.pos,
        )),

        ExprKind::StructLiteral { struct_id, fields } => {
            const_eval_struct_literal(struct_id, fields, expr.pos, cg)
        }

        // A link-time-constant function address.
        ExprKind::FunctionRef(name) => {
            let function = cg
                .functions
                .get(name)
                .copied()
                .ok_or_else(|| CodegenError::UndefinedFunction(name.clone(), expr.pos))?;
            Ok(function.as_global_value().as_pointer_value().into())
        }

        ExprKind::EnumValue { value, .. } => {
            Ok(cg.context.i32_type().const_int(*value as u64, false).into())
        }

        ExprKind::IndirectCall { .. } => Err(CodegenError::InvalidOperation(
            "indirect call not supported in constant expressions".to_string(),
            expr.pos,
        )),

        ExprKind::SizeOf(ty) => {
            let bytes = cg.sizeof_lang_type(ty, expr.pos)?;
            Ok(cg.context.i64_type().const_int(bytes, false).into())
        }

        ExprKind::Null => Ok(cg
            .context
            .ptr_type(AddressSpace::default())
            .const_null()
            .into()),

        ExprKind::ValueBlock(_) => Err(CodegenError::InvalidOperation(
            "value block is not a compile-time constant".to_string(),
            expr.pos,
        )),
    }
}

/// Foldable only as a *global* initializer (a local constructs at runtime —
/// `try_fold` just falls through). The constant's type is an anonymous
/// padded mirror of the `{ i32 tag, [k x iN] }` storage: payload fields keep
/// their natural types, and explicit `[N x i8]` padding pins every offset to
/// the uniform layout — no endianness games serializing fields into the unit
/// array.
fn const_eval_sum_construct<'ctx>(
    sum_id: &u32,
    variant: &u32,
    args: &[Expression],
    pos: Position,
    cg: &mut CodeGenerator<'ctx>,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    if !cg.in_global_init {
        return Err(CodegenError::InvalidOperation(
            "sum construction is not a constant expression".to_string(),
            pos,
        ));
    }
    let storage = *cg
        .sum_types
        .get(sum_id)
        .ok_or_else(|| CodegenError::TypeError(format!("unregistered sum id {sum_id}"), pos))?;
    let target_data = cg.target_machine.get_target_data();
    let total = target_data.get_store_size(&storage);
    let tag_ty = cg.sum_tag_type(*sum_id, pos)?;
    let tag = tag_ty.const_int(u64::from(*variant), false);
    let tag_size = u64::from(tag_ty.get_bit_width() / 8);
    let syms = std::rc::Rc::clone(&cg.symbols);
    let field_tys = &syms.type_def(*sum_id).as_sum().variants[*variant as usize].fields;

    let mut members: Vec<BasicValueEnum> = vec![tag.into()];
    if field_tys.is_empty() {
        // Fill to the storage size exactly — an under-filled global would let
        // storage-typed reads run past the allocation.
        if total > tag_size {
            let pad = u32::try_from(total - tag_size).expect("padding fits u32");
            members.push(cg.context.i8_type().array_type(pad).const_zero().into());
        }
    } else {
        let payload_ty = cg
            .sum_payload_type(*sum_id, *variant as usize)
            .map_err(|e| e.with_pos(pos))?
            .expect("variant with args has a payload type");
        let payload_size = target_data.get_store_size(&payload_ty);
        let payload_off = u64::from(target_data.get_abi_alignment(&storage));
        if payload_off > tag_size {
            let pad = u32::try_from(payload_off - tag_size).expect("padding fits u32");
            members.push(cg.context.i8_type().array_type(pad).const_zero().into());
        }
        let mut fields = Vec::with_capacity(args.len());
        for (arg, (_, fty)) in args.iter().zip(field_tys) {
            fields.push(const_coerced_value(arg, cg, Some(fty))?);
        }
        members.push(cg.context.const_struct(&fields, false).into());
        let tail = total - payload_off - payload_size;
        if tail > 0 {
            let tail = u32::try_from(tail).expect("padding fits u32");
            members.push(cg.context.i8_type().array_type(tail).const_zero().into());
        }
    }
    Ok(cg.context.const_struct(&members, false).into())
}

fn const_eval_struct_literal<'ctx>(
    struct_id: &u32,
    fields: &[(String, Expression)],
    pos: Position,
    cg: &mut CodeGenerator<'ctx>,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    let struct_ty = *cg.struct_types.get(struct_id).ok_or_else(|| {
        CodegenError::TypeError(format!("unregistered type-struct id {struct_id}"), pos)
    })?;

    let syms = std::rc::Rc::clone(&cg.symbols);
    let layout = &syms.type_def(*struct_id).as_struct().fields;
    let mut vals = Vec::with_capacity(layout.len());
    for declared in layout {
        let (fname, fty) = (&declared.name, &declared.ty);
        // A folded sum has an anonymous padded type that can't embed
        // in a named struct constant (member types must match).
        if fty.pointer_depth == 0
            && !fty.is_array()
            && matches!(fty.base, crate::lexer::TypeBase::Sum(_))
        {
            return Err(CodegenError::InvalidOperation(
                "sum-typed fields are not supported in constant struct initializers yet"
                    .to_string(),
                pos,
            ));
        }
        match fields.iter().find(|(n, _)| n == fname) {
            Some((_, fexpr)) => vals.push(const_coerced_value(fexpr, cg, Some(fty))?),
            None => {
                return Err(CodegenError::TypeError(
                    format!(
                        "missing field '{fname}' in struct literal for type-struct id {struct_id}"
                    ),
                    pos,
                ));
            }
        }
    }
    Ok(struct_ty.const_named_struct(&vals).into())
}

/// Constant counterpart to `CodeGenerator::generate_coerced_value`: evaluate
/// `expr` as a constant and coerce it to `target` when the types differ.
fn const_coerced_value<'ctx>(
    expr: &Expression,
    cg: &mut CodeGenerator<'ctx>,
    target: Option<&LangType>,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    // Fast path: literal assigned to a scalar target — emit at target type
    // with overflow check.
    if let Some(target_ty) = target
        && target_ty.pointer_depth == 0
        && !target_ty.is_array()
        && let ExprKind::Literal(lit @ (LiteralValue::Integer(_) | LiteralValue::Float(_))) =
            &expr.kind
    {
        return cg.generate_literal_typed(lit, target_ty, expr.pos);
    }

    let val = const_eval(expr, cg)?;

    // Auto-widen to target if types differ. Struct values are aggregates and
    // are copied as-is.
    if let Some(target_ty) = target
        && target_ty.pointer_depth == 0
        && !target_ty.is_array()
        && !matches!(target_ty.base, TypeBase::Struct(_) | TypeBase::Sum(_))
    {
        let target_llvm = target_ty
            .to_llvm(cg.context)
            .map_err(|e| e.with_pos(expr.pos))?;
        if val.get_type() != target_llvm {
            return cg
                .constant_emitter()
                .emit_cast(val, target_llvm, &expr.expr_type, target_ty, expr.pos);
        }
    }

    Ok(val)
}
