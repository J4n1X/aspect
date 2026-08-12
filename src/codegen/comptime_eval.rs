//! Compile-time constant expression evaluation.
//!
//! The constant-folding counterpart to `walk_expression`: folds the expression
//! tree in Rust and reconstructs LLVM constants via `ConstantEmitter`,
//! returning `Err` for any sub-expression with no constant-folding path. Drives
//! global initializers, `const`-local folding, and the constant array/struct
//! fast-paths.

use inkwell::values::BasicValueEnum;

use crate::codegen::CodegenError;
use crate::codegen::eval::comptime::ComptimeEval;
use crate::codegen::eval::walk;
use crate::codegen::generator::CodeGenerator;
use crate::codegen::types::LangTypeExt;
use crate::codegen::value_emitter::ValueEmitter;
use crate::lexer::{LangType, Position, TypeBase};
use crate::parser::{ExprKind, Expression, LiteralValue};
use crate::symbol::ids::{StructId, SumId};

/// Evaluate `expr` as a compile-time constant, producing an LLVM constant value.
///
/// Returns `Err` when `expr` (or any sub-expression) is not a compile-time constant.
pub(crate) fn comptime_eval<'ctx>(
    expr: &Expression,
    cg: &mut CodeGenerator<'ctx>,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    walk::<ComptimeEval>(cg, expr)
}

/// Foldable only as a *global* initializer (a local constructs at runtime —
/// `try_fold` just falls through). The constant's type is an anonymous
/// padded mirror of the `{ i32 tag, [k x iN] }` storage: payload fields keep
/// their natural types, and explicit `[N x i8]` padding pins every offset to
/// the uniform layout — no endianness games serializing fields into the unit
/// array.
pub(crate) fn comptime_sum_construct<'ctx>(
    sum_id: &SumId,
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
    let field_tys = &syms[*sum_id].variants[*variant as usize].fields;

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

pub(crate) fn comptime_struct_literal<'ctx>(
    struct_id: &StructId,
    fields: &[(String, Expression)],
    pos: Position,
    cg: &mut CodeGenerator<'ctx>,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    let struct_ty = *cg.struct_types.get(struct_id).ok_or_else(|| {
        CodegenError::TypeError(format!("unregistered type-struct id {struct_id}"), pos)
    })?;

    let syms = std::rc::Rc::clone(&cg.symbols);
    let layout = &syms[*struct_id].fields;
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

    let val = comptime_eval(expr, cg)?;

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
