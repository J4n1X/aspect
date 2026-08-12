//! Runtime mode: every node lowers to LLVM IR through the builder.


use crate::codegen::CodegenError;
use crate::codegen::eval::{Eval, EvalResult, Val, walk};
use crate::codegen::expressions::{
    emit_comparison, emit_pointer_arithmetic, emit_reference, emit_short_circuit,
    emit_unary_not, emit_variable_load,
};
use crate::codegen::generator::CodeGenerator;
use crate::codegen::types::LangTypeExt;
use crate::codegen::value_emitter::ValueEmitter;
use crate::lexer::{LangType, Position};
use crate::parser::{BinaryOp, ComparisonOp, Expression, LiteralValue, Statement};
use crate::symbol::ids::{StructId, SumId};

pub(crate) struct RuntimeEval;

impl<'ctx> Eval<'ctx> for RuntimeEval {
    const MODE: &'static str = "runtime code";

    fn emitter<'a>(cg: &'a CodeGenerator<'ctx>) -> impl ValueEmitter<'ctx> + 'a {
        cg.runtime_emitter()
    }

    fn variable(cg: &mut CodeGenerator<'ctx>, name: &str, pos: Position) -> EvalResult<'ctx> {
        emit_variable_load(cg, name, pos)
    }

    fn string_literal(cg: &mut CodeGenerator<'ctx>, index: usize, _pos: Position)
    -> EvalResult<'ctx> {
        cg.emit_string_ptr(index)
    }

    fn reference(cg: &mut CodeGenerator<'ctx>, inner: &Expression) -> EvalResult<'ctx> {
        emit_reference(cg, inner)
    }

    fn logical(
        cg: &mut CodeGenerator<'ctx>,
        op: &BinaryOp,
        left: &Expression,
        right: &Expression,
        pos: Position,
    ) -> EvalResult<'ctx> {
        emit_short_circuit(cg, op, left, right, pos)
    }

    fn struct_literal(
        cg: &mut CodeGenerator<'ctx>,
        struct_id: StructId,
        fields: &[(String, Expression)],
        pos: Position,
    ) -> EvalResult<'ctx> {
        cg.emit_struct_literal(struct_id, fields, pos)
    }

    fn sum_construct(
        cg: &mut CodeGenerator<'ctx>,
        sum_id: SumId,
        variant: u32,
        args: &[Expression],
        pos: Position,
    ) -> EvalResult<'ctx> {
        cg.emit_sum_construct(sum_id, variant, args, pos)
    }

    fn unary_not(cg: &mut CodeGenerator<'ctx>, inner: &Expression) -> EvalResult<'ctx> {
        emit_unary_not(cg, inner)
    }

    fn bitwise_not(cg: &mut CodeGenerator<'ctx>, inner: &Expression) -> EvalResult<'ctx> {
        let val = walk::<Self>(cg, inner)?.into_int_value();
        Ok(cg.builder.build_not(val, "bnottmp")?.into())
    }

    fn scalar_literal(
        cg: &mut CodeGenerator<'ctx>,
        lit: &LiteralValue,
        ty: &LangType,
        pos: Position,
    ) -> EvalResult<'ctx> {
        match lit {
            LiteralValue::Integer(v) => 
                cg.runtime_emitter().emit_int_literal(*v, ty)
                .map_err(|e| e.with_pos(pos)),
            LiteralValue::Float(v) => 
                cg.runtime_emitter().emit_float_literal(*v, ty)
                .map_err(|e| e.with_pos(pos)),
            LiteralValue::Bool(b) => Ok(cg.context.bool_type().const_int(u64::from(*b), false).into()),
            LiteralValue::String(_) => 
                unreachable!("walk routes directly to string_literal")
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
        cg.runtime_emitter().emit_cast(val, target_llvm, &inner.expr_type, target, inner.pos)
    }

    // ── Overrides of the refusing defaults ────────────────────────────────

    fn comparison(
        cg: &mut CodeGenerator<'ctx>,
        left: &Expression,
        op: &ComparisonOp,
        right: &Expression,
        _pos: Position,
    ) -> EvalResult<'ctx> {
        emit_comparison(cg, left, op, right)
    }

    fn function_call(
        cg: &mut CodeGenerator<'ctx>,
        name: &str,
        args: &[Expression],
        pos: Position,
    ) -> EvalResult<'ctx> {
        cg.generate_function_call(name, args, pos)
    }

    fn indirect_call(
        cg: &mut CodeGenerator<'ctx>,
        callee: &Expression,
        args: &[Expression],
        _pos: Position,
    ) -> EvalResult<'ctx> {
        cg.generate_indirect_call(callee, args)
    }

    fn pointer_arithmetic(
        cg: &mut CodeGenerator<'ctx>,
        op: &BinaryOp,
        left: &Expression,
        right: &Expression,
        left_val: Val<'ctx>,
        right_val: Val<'ctx>,
        _pos: Position,
    ) -> EvalResult<'ctx> {
        emit_pointer_arithmetic(cg, op, left, right, left_val, right_val)
    }

    fn is_probe(
        cg: &mut CodeGenerator<'ctx>,
        scrutinee: &Expression,
        sum_id: SumId,
        variant: u32,
        binders: &[Option<(String, LangType)>],
        pos: Position,
    ) -> EvalResult<'ctx> {
        if binders.is_empty() {
            let (matched, _slot) = cg.emit_sum_probe(scrutinee, sum_id, variant, pos)?;
            return Ok(matched.into());
        }
        cg.emit_is_binding(scrutinee, sum_id, variant, binders, pos)
    }

    fn dereference(
        cg: &mut CodeGenerator<'ctx>,
        inner: &Expression,
        pos: Position,
    ) -> EvalResult<'ctx> {
        let ptr = walk::<Self>(cg, inner)?;
        if inner.expr_type.pointer_depth == 0 {
            return Err(CodegenError::TypeError(
                "Cannot dereference a non-pointer type".to_string(),
                pos,
            ));
        }
        // Cache-aware lowering
        let pointee_type = cg
            .lang_type_to_llvm(&inner.expr_type.pointee())
            .map_err(|e| e.with_pos(inner.pos))?;
        Ok(cg
            .builder
            .build_load(pointee_type, ptr.into_pointer_value(), "deref")?)
    }

    fn field_access(cg: &mut CodeGenerator<'ctx>, expr: &Expression) -> EvalResult<'ctx> {
        let (field_ptr, field_ty) = cg.emit_address(expr)?;
        if field_ty.is_array() {
            // Decay array to pointer
            return Ok(field_ptr.into());
        }
        let field_llvm = cg
            .lang_type_to_llvm(&field_ty)
            .map_err(|e| e.with_pos(expr.pos))?;
        Ok(cg.builder.build_load(field_llvm, field_ptr, "field")?)
    }

    fn value_block(
        cg: &mut CodeGenerator<'ctx>,
        stmts: &[Statement],
        ty: LangType,
        pos: Position,
    ) -> EvalResult<'ctx> {
        cg.generate_value_block(stmts, ty, pos)
    }
}
