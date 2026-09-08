//! Compile-time mode: nodes fold to LLVM constants, with no builder and no
//! control flow. Every node it cannot express is refused by an inherited
//! [`Eval`] default.

use inkwell::AddressSpace;

use crate::codegen::CodegenError;
use crate::codegen::comptime_eval::{comptime_struct_literal, comptime_sum_construct};
use crate::codegen::eval::{Eval, EvalResult, Val, walk};
use crate::codegen::expressions::emit_binary_dispatch;
use crate::codegen::generator::CodeGenerator;
use crate::codegen::types::LangTypeExt;
use crate::codegen::value_emitter::ValueEmitter;
use crate::lexer::Position;
use crate::parser::{BinaryOp, ExprKind, Expression};
use crate::symbol::ids::{StructId, SumId};

pub(crate) struct ComptimeEval;

impl<'ctx> Eval<'ctx> for ComptimeEval {
    const MODE: &'static str = "constant expressions";

    fn emitter<'a>(cg: &'a CodeGenerator<'ctx>) -> impl ValueEmitter<'ctx> + 'a {
        cg.constant_emitter()
    }

    fn variable(cg: &mut CodeGenerator<'ctx>, name: &str, pos: Position) -> EvalResult<'ctx> {
        for scope in cg.scope.iter_scopes() {
            if let Some(var) = scope.get(name) {
                return var.const_value.ok_or_else(|| {
                    CodegenError::InvalidOperation(
                        format!("variable '{name}' is not a compile-time constant"),
                        pos,
                    )
                });
            }
        }
        let global_val = cg
            .module
            .get_global(name)
            .ok_or_else(|| CodegenError::UndefinedVariable(name.to_string(), pos))?;
        // A mutable global's runtime value is *not* its initializer, so it
        // can't be folded — except inside another global's initializer,
        // order-sensitive static init reading a prior start value, which
        // `in_global_init` marks. A `const` global is immutable everywhere.
        if !global_val.is_constant() && !cg.in_global_init {
            return Err(CodegenError::InvalidOperation(
                format!("global '{name}' is not a compile-time constant"),
                pos,
            ));
        }
        if global_val.get_value_type().is_array_type() {
            return Ok(global_val.as_pointer_value().into());
        }
        global_val.get_initializer().ok_or_else(|| {
            CodegenError::InvalidOperation(
                format!(
                    "global '{name}' has no constant initializer; \
                     declare it before referencing it in another global initializer"
                ),
                pos,
            )
        })
    }

    fn string_literal(
        cg: &mut CodeGenerator<'ctx>,
        index: usize,
        _pos: Position,
    ) -> EvalResult<'ctx> {
        let ptr = cg
            .scope
            .lookup_global(&CodeGenerator::string_literal_name(index))
            .expect("Internal error: String literal global not found")
            .ptr;
        let i8_ptr_type = cg.context.ptr_type(AddressSpace::default());
        Ok(ptr.const_cast(i8_ptr_type).into())
    }

    fn reference(cg: &mut CodeGenerator<'ctx>, inner: &Expression) -> EvalResult<'ctx> {
        match &inner.kind {
            ExprKind::Variable(name) => {
                let ptr = cg
                    .scope
                    .lookup_global(name.as_str())
                    .ok_or_else(|| CodegenError::UndefinedVariable(name.clone(), inner.pos))?
                    .ptr;
                Ok(ptr.into())
            }
            ExprKind::Dereference(inner2) => walk::<Self>(cg, inner2),
            ExprKind::FieldAccess { .. } => Err(CodegenError::InvalidOperation(
                "address-of field not supported in constant expressions".to_string(),
                inner.pos,
            )),
            _ => Err(CodegenError::InvalidOperation(
                "Cannot take address of non-lvalue".to_string(),
                inner.pos,
            )),
        }
    }

    /// No control flow to short-circuit with, so both operands fold and the
    /// emitter treats `&&`/`||` as ordinary integer ops.
    fn logical(
        cg: &mut CodeGenerator<'ctx>,
        op: &BinaryOp,
        left: &Expression,
        right: &Expression,
        pos: Position,
    ) -> EvalResult<'ctx> {
        let left_val = walk::<Self>(cg, left)?;
        let right_val = walk::<Self>(cg, right)?;
        if left.expr_type.pointer_depth > 0 || right.expr_type.pointer_depth > 0 {
            return Err(CodegenError::InvalidOperation(
                "pointer arithmetic not supported in constant expressions".to_string(),
                pos,
            ));
        }
        emit_binary_dispatch(
            &Self::emitter(cg),
            left_val,
            right_val,
            op,
            &left.expr_type,
            &right.expr_type,
            pos,
        )
    }

    /// Do pointer arithmetic in constant space.
    /// This only works if the pointer is constant, too, so no pointers of
    /// runtime values, allocations, etc.
    fn pointer_arithmetic(
        cg: &mut CodeGenerator<'ctx>,
        op: &BinaryOp,
        left: &Expression,
        right: &Expression,
        left_val: Val<'ctx>,
        right_val: Val<'ctx>,
        pos: Position,
    ) -> EvalResult<'ctx> {
        let left_is_ptr = left.expr_type.pointer_depth > 0;
        if left_is_ptr && right.expr_type.pointer_depth > 0 {
            return Err(CodegenError::InvalidOperation(
                "pointer arithmetic only allowed with integers".to_string(),
                left.pos,
            ));
        }

        let (ptr_expr, ptr_val, int_val) = if left_is_ptr {
            (left, left_val, right_val)
        } else {
            (right, right_val, left_val)
        };
        let ptr = ptr_val.into_pointer_value();
        if !ptr.is_const() {
            return Err(CodegenError::InvalidOperation(
                "pointer arithmetic needs a link-time-constant base in constant expressions"
                    .to_string(),
                pos,
            ));
        }
        let index = int_val.into_int_value();
        let pointee_type = cg
            .lang_type_to_llvm(&ptr_expr.expr_type.pointee())
            .map_err(|e| e.with_pos(ptr_expr.pos))?;

        let index = match op {
            BinaryOp::Add => index,
            BinaryOp::Sub if left_is_ptr => index.const_neg(),
            _ => {
                return Err(CodegenError::InvalidOperation(
                    format!("operator {op:?} not supported for pointers"),
                    left.pos,
                ));
            }
        };
        Ok(unsafe { ptr.const_in_bounds_gep(pointee_type, &[index]) }.into())
    }

    fn struct_literal(
        cg: &mut CodeGenerator<'ctx>,
        struct_id: StructId,
        fields: &[(String, Expression)],
        pos: Position,
    ) -> EvalResult<'ctx> {
        comptime_struct_literal(&struct_id, fields, pos, cg)
    }

    fn sum_construct(
        cg: &mut CodeGenerator<'ctx>,
        sum_id: SumId,
        variant: u32,
        args: &[Expression],
        pos: Position,
    ) -> EvalResult<'ctx> {
        comptime_sum_construct(&sum_id, &variant, args, pos, cg)
    }

    fn unary_not(cg: &mut CodeGenerator<'ctx>, inner: &Expression) -> EvalResult<'ctx> {
        let raw = walk::<Self>(cg, inner)?;
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
}
