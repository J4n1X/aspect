//! Compile-time constant expression evaluation.
//!
//! `const_eval` is the constant-folding counterpart to `walk_expression`
//! (`super::expressions`). Where `walk_expression` emits runtime LLVM IR via
//! the builder (`RuntimeEmitter`), `const_eval` folds the expression tree in
//! Rust and reconstructs LLVM constants via `ConstantEmitter`, returning an
//! `Err` for any sub-expression that has no constant-folding path.
//!
//! This is the single entry-point for global initializers, `const`-local
//! folding and the constant array/struct fast-paths. The "is this a constant?"
//! predicate is just `const_eval(...).ok()` (see
//! `CodeGenerator::try_fold_constant_expression`).

use inkwell::AddressSpace;
use inkwell::values::BasicValueEnum;

use crate::codegen::CodegenError;
use crate::codegen::expressions::emit_binary_dispatch;
use crate::codegen::generator::CodeGenerator;
use crate::codegen::types::LangTypeExt;
use crate::codegen::value_emitter::{ConstantEmitter, ValueEmitter};
use crate::lexer::{LangType, TypeBase};
use crate::parser::{ExprKind, Expression, LiteralValue};

/// Evaluate `expr` as a compile-time constant, producing an LLVM constant value.
///
/// Returns `Err` when `expr` (or any sub-expression) is not a compile-time
/// constant — the same errors the old `EmitMode::Constant` walk produced.
pub(crate) fn const_eval<'ctx>(
    expr: &Expression,
    cg: &mut CodeGenerator<'ctx>,
) -> Result<BasicValueEnum<'ctx>, CodegenError> {
    match &expr.kind {
        // ── Literals ──────────────────────────────────────────────────────
        ExprKind::Literal(lit) => match lit {
            LiteralValue::Integer(val) => ConstantEmitter {
                context: cg.context,
            }
            .emit_int_literal(*val, &expr.expr_type)
            .map_err(|e| e.with_pos(expr.pos)),
            LiteralValue::Float(val) => ConstantEmitter {
                context: cg.context,
            }
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

        // ── Variable ──────────────────────────────────────────────────────
        ExprKind::Variable(name) => {
            // Check local scope first (const locals store their folded value).
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
            // Fall back to global initializer.
            let global_val = cg
                .module
                .get_global(name)
                .ok_or_else(|| CodegenError::UndefinedVariable(name.clone(), expr.pos))?;
            // A mutable global's runtime value is *not* its initializer, so it
            // cannot be folded — a caller like a local `u64 x = g` would then
            // capture the compile-time start value instead of the current one.
            // The one place folding is correct is another global's initializer,
            // which is order-sensitive static init reading a prior start value;
            // `in_global_init` marks exactly that context. A `const` global is
            // immutable, so its initializer *is* its value everywhere.
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

        // ── Binary operations ─────────────────────────────────────────────
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
                &ConstantEmitter {
                    context: cg.context,
                },
                left_val,
                right_val,
                op,
                &left.expr_type,
                &right.expr_type,
                expr.pos,
            )
        }

        // ── Comparison (no constant path) ─────────────────────────────────
        ExprKind::Comparison { .. } => Err(CodegenError::InvalidOperation(
            "comparison not supported in constant expressions".to_string(),
            expr.pos,
        )),

        // ── Address-of ────────────────────────────────────────────────────
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

        // ── Dereference (no constant path) ────────────────────────────────
        ExprKind::Dereference(_) => Err(CodegenError::InvalidOperation(
            "dereference not supported in constant expressions".to_string(),
            expr.pos,
        )),

        // ── Function call (no constant path) ──────────────────────────────
        ExprKind::FunctionCall { .. } => Err(CodegenError::InvalidOperation(
            "function calls not supported in constant expressions".to_string(),
            expr.pos,
        )),

        // ── Cast ──────────────────────────────────────────────────────────
        ExprKind::Cast {
            expr: inner,
            target_type,
        } => {
            let val = const_eval(inner, cg)?;
            let target_llvm = target_type
                .to_llvm(cg.context)
                .map_err(|e| e.with_pos(expr.pos))?;
            ConstantEmitter {
                context: cg.context,
            }
            .emit_cast(val, target_llvm, &inner.expr_type, target_type, inner.pos)
        }

        // ── Alloc (global alloc materialises a zeroed global) ─────────────
        ExprKind::Alloc { alloc_type, count } => cg.generate_alloc(alloc_type, count),

        // ── Logical NOT ───────────────────────────────────────────────────
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

        // ── Bitwise NOT ───────────────────────────────────────────────────
        ExprKind::BitwiseNot(inner) => {
            let val = const_eval(inner, cg)?.into_int_value();
            Ok(val.const_not().into())
        }

        // ── List initializer (only valid in a variable declaration) ───────
        ExprKind::ListInitializer(_) => Err(CodegenError::InvalidOperation(
            "list initializer is only valid in a variable declaration".to_string(),
            expr.pos,
        )),

        // ── Field access (no constant path) ───────────────────────────────
        ExprKind::FieldAccess { .. } => Err(CodegenError::InvalidOperation(
            "field access not supported in constant expressions".to_string(),
            expr.pos,
        )),

        // ── Struct literal — fold every field into a constant struct ──────
        ExprKind::StructLiteral { struct_id, fields } => {
            let struct_ty = *cg.struct_types.get(struct_id).ok_or_else(|| {
                CodegenError::TypeError(
                    format!("unregistered type-struct id {struct_id}"),
                    expr.pos,
                )
            })?;

            let layout = cg.struct_fields[struct_id].clone();
            let mut vals = Vec::with_capacity(layout.len());
            for (fname, fty) in &layout {
                match fields.iter().find(|(n, _)| n == fname) {
                    Some((_, fexpr)) => vals.push(const_coerced_value(fexpr, cg, Some(fty))?),
                    None => {
                        return Err(CodegenError::TypeError(
                            format!(
                                "missing field '{fname}' in struct literal for type-struct id {struct_id}"
                            ),
                            expr.pos,
                        ));
                    }
                }
            }
            Ok(struct_ty.const_named_struct(&vals).into())
        }

        // ── Function reference: a link-time-constant function address ─────
        ExprKind::FunctionRef(name) => {
            let function = cg
                .functions
                .get(name)
                .copied()
                .ok_or_else(|| CodegenError::UndefinedFunction(name.clone(), expr.pos))?;
            Ok(function.as_global_value().as_pointer_value().into())
        }

        // ── Enum variant value: a compile-time `i32` constant ─────────────
        ExprKind::EnumValue { value, .. } => {
            Ok(cg.context.i32_type().const_int(*value as u64, false).into())
        }

        // ── Indirect call (no constant path) ──────────────────────────────
        ExprKind::IndirectCall { .. } => Err(CodegenError::InvalidOperation(
            "indirect call not supported in constant expressions".to_string(),
            expr.pos,
        )),

        // ── `sizeof(T)` — a fixed `u64` from the target data layout ────────
        ExprKind::SizeOf(ty) => {
            let bytes = cg.sizeof_lang_type(ty, expr.pos)?;
            Ok(cg.context.i64_type().const_int(bytes, false).into())
        }

        // ── `null` — opaque-ptr null constant ─────────────────────────────
        ExprKind::Null => Ok(cg
            .context
            .ptr_type(AddressSpace::default())
            .const_null()
            .into()),

        // ── Value-block executes statements — never a constant ────────────
        ExprKind::ValueBlock(_) => Err(CodegenError::InvalidOperation(
            "value block is not a compile-time constant".to_string(),
            expr.pos,
        )),
        // The typechecker rewrites every `MethodCall` into a `FunctionCall` /
        // `IndirectCall` before codegen; one reaching here means the checker
        // was bypassed.
        ExprKind::MethodCall { .. } => unreachable!("MethodCall is lowered by the typechecker"),
    }
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
        && !matches!(target_ty.base, TypeBase::Struct(_))
    {
        let target_llvm = target_ty
            .to_llvm(cg.context)
            .map_err(|e| e.with_pos(expr.pos))?;
        if val.get_type() != target_llvm {
            return ConstantEmitter {
                context: cg.context,
            }
            .emit_cast(val, target_llvm, &expr.expr_type, target_ty, expr.pos);
        }
    }

    Ok(val)
}
