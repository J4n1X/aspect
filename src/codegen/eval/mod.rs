//! The one expression walk, shared by runtime code-generation
//! ([`super::expressions`]) and constant folding ([`super::comptime_eval`]).
//!
//! Both were the same traversal written twice: identical recursion, differing
//! only in which [`ValueEmitter`] materialises the leaves and in which nodes
//! the mode can express at all. `Eval` captures exactly that difference —
//! everything else lives once, in `walk`.

use inkwell::AddressSpace;
use inkwell::values::BasicValueEnum;

use crate::codegen::CodegenError;
use crate::codegen::expressions::emit_binary_dispatch;
use crate::codegen::generator::CodeGenerator;
use crate::codegen::types::LangTypeExt;
use crate::codegen::value_emitter::ValueEmitter;
use crate::lexer::{LangType, Position};
use crate::parser::{BinaryOp, ComparisonOp, ExprKind, Expression, LiteralValue, Statement};
use crate::symbol::ids::{StructId, SumId};

pub(crate) mod comptime;
pub(crate) mod runtime;

pub(crate) type Val<'ctx> = BasicValueEnum<'ctx>;
pub(crate) type EvalResult<'ctx> = Result<Val<'ctx>, CodegenError>;

/// The refusal every defaulted method returns. `what` names the construct;
/// [`Eval::MODE`] names the context, so messages read as full sentences.
pub(crate) fn unsupported<'ctx>(what: &str, mode: &str, pos: Position) -> EvalResult<'ctx> {
    Err(CodegenError::InvalidOperation(
        format!("{what} not supported in {mode}"),
        pos,
    ))
}

/// One evaluation mode. Implementors are zero-sized markers; every method is
/// associated (no `self`) so [`walk`] can dispatch statically without carrying
/// a mode value alongside the `CodeGenerator`.
pub(crate) trait Eval<'ctx>: Sized {
    /// Identifies the mode in error diagnostics. 
    const MODE: &'static str;

    fn emitter<'a>(cg: &'a CodeGenerator<'ctx>) -> impl ValueEmitter<'ctx> + 'a;

    // ── Required: every mode has a real, differing implementation ──────────

    fn variable(cg: &mut CodeGenerator<'ctx>, name: &str, pos: Position) -> EvalResult<'ctx>;

    fn string_literal(cg: &mut CodeGenerator<'ctx>, index: usize, pos: Position)
    -> EvalResult<'ctx>;

    fn reference(cg: &mut CodeGenerator<'ctx>, inner: &Expression) -> EvalResult<'ctx>;

    /// `&&` / `||`, given **unfolded** operands. Runtime must not evaluate the
    /// right operand when the left decides — both for C semantics and because
    /// an `is`-chain conjunct reads bindings its predecessor stores only on the
    /// true edge. 
    fn logical(
        cg: &mut CodeGenerator<'ctx>,
        op: &BinaryOp,
        left: &Expression,
        right: &Expression,
        pos: Position,
    ) -> EvalResult<'ctx>;

    fn struct_literal(
        cg: &mut CodeGenerator<'ctx>,
        struct_id: StructId,
        fields: &[(String, Expression)],
        pos: Position,
    ) -> EvalResult<'ctx>;

    fn sum_construct(
        cg: &mut CodeGenerator<'ctx>,
        sum_id: SumId,
        variant: u32,
        args: &[Expression],
        pos: Position,
    ) -> EvalResult<'ctx>;

    fn unary_not(cg: &mut CodeGenerator<'ctx>, inner: &Expression) -> EvalResult<'ctx>;

    // ── Defaulted: one shared body, the modes differ only in `emitter` ─────

    fn scalar_literal(
        cg: &mut CodeGenerator<'ctx>,
        lit: &LiteralValue,
        ty: &LangType,
        pos: Position,
    ) -> EvalResult<'ctx> {
        match lit {
            LiteralValue::Integer(v) => Self::emitter(cg)
                .emit_int_literal(*v, ty)
                .map_err(|e| e.with_pos(pos)),
            LiteralValue::Float(v) => Self::emitter(cg)
                .emit_float_literal(*v, ty)
                .map_err(|e| e.with_pos(pos)),
            LiteralValue::Bool(b) => Ok(cg
                .context
                .bool_type()
                .const_int(u64::from(*b), false)
                .into()),
            LiteralValue::String(_) => unreachable!("walk routes String to string_literal"),
        }
    }

    fn cast(
        cg: &mut CodeGenerator<'ctx>,
        inner: &Expression,
        target: &LangType,
        pos: Position,
    ) -> EvalResult<'ctx> {
        let val = walk::<Self>(cg, inner)?;
        let target_llvm = target.to_llvm(cg.context).map_err(|e| e.with_pos(pos))?;
        Self::emitter(cg).emit_cast(val, target_llvm, &inner.expr_type, target, inner.pos)
    }

    fn bitwise_not(cg: &mut CodeGenerator<'ctx>, inner: &Expression) -> EvalResult<'ctx> {
        let val = walk::<Self>(cg, inner)?.into_int_value();
        Self::emitter(cg).emit_not(val)
    }

    // ── Defaulted: absent means "this mode cannot express it" ──────────────

    fn comparison(
        _cg: &mut CodeGenerator<'ctx>,
        _left: &Expression,
        _op: &ComparisonOp,
        _right: &Expression,
        pos: Position,
    ) -> EvalResult<'ctx> {
        unsupported("comparison", Self::MODE, pos)
    }

    fn dereference(
        _cg: &mut CodeGenerator<'ctx>,
        _inner: &Expression,
        pos: Position,
    ) -> EvalResult<'ctx> {
        unsupported("pointer dereference", Self::MODE, pos)
    }

    fn field_access(_cg: &mut CodeGenerator<'ctx>, expr: &Expression) -> EvalResult<'ctx> {
        unsupported("field access", Self::MODE, expr.pos)
    }

    fn function_call(
        _cg: &mut CodeGenerator<'ctx>,
        _name: &str,
        _args: &[Expression],
        pos: Position,
    ) -> EvalResult<'ctx> {
        unsupported("function call", Self::MODE, pos)
    }

    fn indirect_call(
        _cg: &mut CodeGenerator<'ctx>,
        _callee: &Expression,
        _args: &[Expression],
        pos: Position,
    ) -> EvalResult<'ctx> {
        unsupported("indirect call", Self::MODE, pos)
    }

    fn is_probe(
        _cg: &mut CodeGenerator<'ctx>,
        _scrutinee: &Expression,
        _sum_id: SumId,
        _variant: u32,
        pos: Position,
    ) -> EvalResult<'ctx> {
        unsupported("`is`", Self::MODE, pos)
    }

    fn is_binding(
        _cg: &mut CodeGenerator<'ctx>,
        _scrutinee: &Expression,
        _sum_id: SumId,
        _variant: u32,
        _binders: &[Option<(String, LangType)>],
        pos: Position,
    ) -> EvalResult<'ctx> {
        unsupported("`is`", Self::MODE, pos)
    }

    fn value_block(
        _cg: &mut CodeGenerator<'ctx>,
        _stmts: &[Statement],
        _ty: LangType,
        pos: Position,
    ) -> EvalResult<'ctx> {
        // TODO: This could maybe, in the far future, work in const mode.
        // For that, we'd need JIT to run.
        unsupported("value block", Self::MODE, pos)
    }

    /// `ptr ± int`, reached only when an operand is pointer-shaped. Runtime
    /// lowers it to an in-bounds GEP. Constants are currently not supported.
    /// Operands arrive already folded — `walk` folds before the pointer test,
    /// as both walkers did, so an override must not re-walk them.
    fn pointer_arithmetic(
        _cg: &mut CodeGenerator<'ctx>,
        _op: &BinaryOp,
        _left: &Expression,
        _right: &Expression,
        _left_val: Val<'ctx>,
        _right_val: Val<'ctx>,
        pos: Position,
    ) -> EvalResult<'ctx> {
        // TODO: We could definitely support this in const mode.
        unsupported("pointer arithmetic", Self::MODE, pos)
    }
}

/// The single traversal. Nodes identical in both modes are handled here;
/// everything else delegates to `E`.
pub(crate) fn walk<'ctx, E: Eval<'ctx>>(
    cg: &mut CodeGenerator<'ctx>,
    expr: &Expression,
) -> EvalResult<'ctx> {
    match &expr.kind {
        ExprKind::Binary { left, op, right } => {
            if matches!(op, BinaryOp::LogicalAnd | BinaryOp::LogicalOr) {
                return E::logical(cg, op, left, right, expr.pos);
            }
            let left_val = walk::<E>(cg, left)?;
            let right_val = walk::<E>(cg, right)?;
            if left.expr_type.pointer_depth > 0 || right.expr_type.pointer_depth > 0 {
                return E::pointer_arithmetic(
                    cg, op, left, right, left_val, right_val, expr.pos,
                );
            }
            emit_binary_dispatch(
                &E::emitter(cg),
                left_val,
                right_val,
                op,
                &left.expr_type,
                &right.expr_type,
                expr.pos,
            )
        }

        ExprKind::Null => Ok(cg
            .context
            .ptr_type(AddressSpace::default())
            .const_null()
            .into()),

        ExprKind::SizeOf(ty) => {
            let bytes = cg.sizeof_lang_type(ty, expr.pos)?;
            Ok(cg.context.i64_type().const_int(bytes, false).into())
        }

        ExprKind::EnumValue { value, .. } => {
            Ok(cg.context.i32_type().const_int(*value as u64, false).into())
        }

        ExprKind::FunctionRef(name) => {
            let function = cg
                .functions
                .get(name)
                .copied()
                .ok_or_else(|| CodegenError::UndefinedFunction(name.clone(), expr.pos))?;
            Ok(function.as_global_value().as_pointer_value().into())
        }

        ExprKind::Alloc { alloc_type, count } => cg.generate_alloc(alloc_type, count),

        ExprKind::ListInitializer(_) => Err(CodegenError::InvalidOperation(
            "list initializer is only valid in a variable declaration".to_string(),
            expr.pos,
        )),

        ExprKind::Literal(LiteralValue::String(index)) => {
            E::string_literal(cg, *index, expr.pos)
        }
        ExprKind::Literal(lit) => E::scalar_literal(cg, lit, &expr.expr_type, expr.pos),

        ExprKind::Variable(name) => E::variable(cg, name, expr.pos),
        ExprKind::Comparison { left, op, right } => E::comparison(cg, left, op, right, expr.pos),
        ExprKind::Reference(inner) => E::reference(cg, inner),
        ExprKind::Dereference(inner) => E::dereference(cg, inner, expr.pos),
        ExprKind::UnaryNot(inner) => E::unary_not(cg, inner),
        ExprKind::BitwiseNot(inner) => E::bitwise_not(cg, inner),

        ExprKind::Cast {
            expr: inner,
            target_type,
        } => E::cast(cg, inner, target_type, expr.pos),

        ExprKind::FunctionCall { name, args } => E::function_call(cg, name, args, expr.pos),
        ExprKind::IndirectCall { callee, args } => E::indirect_call(cg, callee, args, expr.pos),
        ExprKind::FieldAccess { .. } => E::field_access(cg, expr),

        ExprKind::StructLiteral { struct_id, fields } => {
            E::struct_literal(cg, *struct_id, fields, expr.pos)
        }
        ExprKind::SumConstruct {
            sum_id,
            variant,
            args,
        } => E::sum_construct(cg, *sum_id, *variant, args, expr.pos),

        ExprKind::Is {
            scrutinee,
            sum_id,
            variant,
        } => E::is_probe(cg, scrutinee, *sum_id, *variant, expr.pos),
        ExprKind::IsBinding {
            scrutinee,
            sum_id,
            variant,
            binders,
        } => E::is_binding(cg, scrutinee, *sum_id, *variant, binders, expr.pos),

        ExprKind::ValueBlock(stmts) => E::value_block(cg, stmts, expr.expr_type, expr.pos),
    }
}
