//! `switch` statement codegen: discriminant extraction, per-arm case values,
//! sum-payload binder copies, and the default/trap else edge.

use inkwell::basic_block::BasicBlock;
use inkwell::types::IntType;
use inkwell::values::{FunctionValue, IntValue, PointerValue};
use inkwell::IntPredicate;

use crate::codegen::generator::CodeGenerator;
use crate::codegen::CodegenError;
use crate::lexer::{LangType, Position, TypeBase};
use crate::parser::{ExprKind, Expression, LiteralValue, Statement, SwitchArm, SwitchPattern};

impl<'ctx> CodeGenerator<'ctx> {
    /// Lower a `switch`: evaluate the scrutinee exactly once, LLVM `switch`
    /// over the discriminant (a sum's `i32` tag, or the int/bool/enum value),
    /// per-arm blocks with payload bindings copied out of the scrutinee slot.
    /// The else edge is the `default` block when present; otherwise a
    /// `llvm.trap` block at -O0 (debuggable halt on forged tags) and a bare
    /// `unreachable` under optimization — see `emit_switch_else`.
    pub(crate) fn generate_switch(
        &mut self,
        scrutinee: &Expression,
        arms: &[SwitchArm],
        default: Option<&[Statement]>,
        pos: Position,
    ) -> Result<(), CodegenError> {
        let function = self
            .current_function
            .ok_or(CodegenError::UnexpectedStatement(pos))?;

        let (disc, sum_slot) = self.switch_discriminant(scrutinee, function, pos)?;

        let else_bb = self.context.append_basic_block(
            function,
            if default.is_some() {
                "switch.default"
            } else {
                "switch.trap"
            },
        );
        let merge_bb = self.context.append_basic_block(function, "switch.end");

        let disc_ty = disc.get_type();
        let mut cases = Vec::new();
        let mut arm_blocks = Vec::with_capacity(arms.len());
        for arm in arms {
            let bb = self.context.append_basic_block(function, "switch.case");
            for pattern in &arm.patterns {
                cases.push((self.switch_case_value(pattern, disc_ty)?, bb));
            }
            arm_blocks.push(bb);
        }
        self.builder.build_switch(disc, else_bb, &cases)?;

        for (arm, bb) in arms.iter().zip(arm_blocks) {
            self.builder.position_at_end(bb);
            self.enter_scope();
            if let Some(SwitchPattern::SumVariant { variant, binders }) = arm.patterns.first()
                && binders.iter().any(Option::is_some)
            {
                let sum_slot = sum_slot.expect("binding pattern without a sum scrutinee");
                self.bind_switch_arm_payload(function, sum_slot, *variant, binders, arm.pos)?;
            }
            for stmt in &arm.body {
                self.generate_statement(stmt)?;
            }
            self.exit_scope();
            if !self.block_has_terminator() {
                self.builder.build_unconditional_branch(merge_bb)?;
            }
        }

        self.emit_switch_else(else_bb, default, merge_bb, pos)
    }

    /// Evaluate the scrutinee once. A sum scrutinee is stored into an
    /// entry-block slot and its tag loaded (the slot is what arm bindings
    /// later GEP into); anything else is the int/bool/enum value directly —
    /// a `bool` normalizes from `i8` to `i1` so the switch constants share
    /// one width with `switch_case_value`'s bool arm.
    fn switch_discriminant(
        &mut self,
        scrutinee: &Expression,
        function: FunctionValue<'ctx>,
        pos: Position,
    ) -> Result<(IntValue<'ctx>, Option<(PointerValue<'ctx>, u32)>), CodegenError> {
        let s_ty = scrutinee.expr_type;

        let mut sum_slot = None;
        let disc = if s_ty.pointer_depth <= 1
            && !s_ty.is_array()
            && let TypeBase::Sum(sum_id) = s_ty.base
        {
            let storage = *self.sum_types.get(&sum_id).ok_or_else(|| {
                CodegenError::TypeError(format!("unregistered sum id {sum_id}"), pos)
            })?;
            // A pointer scrutinee auto-derefs: the pointer already addresses
            // the sum, so it *is* the slot — no whole-value copy. Null is the
            // user's problem, like any deref.
            let slot = if s_ty.pointer_depth == 1 {
                self.generate_expression(scrutinee)?.into_pointer_value()
            } else {
                let value = self.generate_expression(scrutinee)?;
                let slot =
                    self.build_entry_alloca(function, storage.into(), "switch.scrut", pos)?;
                self.builder.build_store(slot, value)?;
                slot
            };
            let tag_ptr = self
                .builder
                .build_struct_gep(storage, slot, 0, "switch.tag")?;
            sum_slot = Some((slot, sum_id));
            self.builder
                .build_load(self.sum_tag_type(sum_id, pos)?, tag_ptr, "tag")?
                .into_int_value()
        } else {
            let v = self.generate_expression(scrutinee)?.into_int_value();
            // Bool variables load as `i8` (comparisons already yield `i1`) —
            // normalize so the switch constants share one width.
            if s_ty.base == TypeBase::Bool && v.get_type().get_bit_width() > 1 {
                self.builder.build_int_compare(
                    IntPredicate::NE,
                    v,
                    v.get_type().const_zero(),
                    "switch.bool",
                )?
            } else {
                v
            }
        };
        Ok((disc, sum_slot))
    }

    /// The discriminant-width constant a `case` pattern matches against.
    fn switch_case_value(
        &self,
        pattern: &SwitchPattern,
        disc_ty: IntType<'ctx>,
    ) -> Result<IntValue<'ctx>, CodegenError> {
        match pattern {
            SwitchPattern::SumVariant { variant, .. } => {
                Ok(disc_ty.const_int(u64::from(*variant), false))
            }
            SwitchPattern::Const(e) => match &e.kind {
                ExprKind::Literal(LiteralValue::Integer(v)) => {
                    // `as u64` keeps the two's-complement bits; LLVM
                    // truncates to the discriminant width, so negative
                    // labels land correctly at any width.
                    Ok(disc_ty.const_int(*v as u64, false))
                }
                ExprKind::Literal(LiteralValue::Bool(b)) => {
                    Ok(disc_ty.const_int(u64::from(*b), false))
                }
                ExprKind::EnumValue { value, .. } => Ok(disc_ty.const_int(*value as u64, false)),
                _ => Err(CodegenError::InvalidOperation(
                    "non-constant case pattern survived checking".to_string(),
                    e.pos,
                )),
            },
        }
    }

    /// Bindings are copies: GEP the payload through the variant's bare
    /// payload struct at storage field 1 (the uniform offset) and copy each
    /// bound field into its own local.
    fn bind_switch_arm_payload(
        &mut self,
        function: FunctionValue<'ctx>,
        sum_slot: (PointerValue<'ctx>, u32),
        variant: u32,
        binders: &[Option<(String, LangType)>],
        pos: Position,
    ) -> Result<(), CodegenError> {
        let (slot, sum_id) = sum_slot;
        let storage = self.sum_types[&sum_id];
        let payload_ptr = self
            .builder
            .build_struct_gep(storage, slot, 1, "switch.payload")?;
        let payload_ty = self
            .sum_payload_type(sum_id, variant as usize)
            .map_err(|e| e.with_pos(pos))?
            .expect("binding pattern on a payload-less variant");
        for (i, binder) in binders.iter().enumerate() {
            let Some((name, field_ty)) = binder else { continue };
            let field_ptr = self.builder.build_struct_gep(
                payload_ty,
                payload_ptr,
                u32::try_from(i).expect("payload field index fits u32"),
                name,
            )?;
            let llvm_ty = if field_ty.is_array() {
                self.lang_type_to_llvm_array(field_ty)
                    .map_err(|e| e.with_pos(pos))?
                    .into()
            } else {
                self.lang_type_to_llvm(field_ty).map_err(|e| e.with_pos(pos))?
            };
            let alloca = self.build_entry_alloca(function, llvm_ty, name, pos)?;
            self.copy_sum_payload_field(alloca, field_ptr, *field_ty, pos)?;
            self.add_variable(name.clone(), alloca, llvm_ty, *field_ty, None);
        }
        Ok(())
    }

    /// The else edge: the `default` body when present, otherwise a cold
    /// `llvm.trap`. Either way, ends with the builder positioned at `merge_bb`.
    fn emit_switch_else(
        &mut self,
        else_bb: BasicBlock<'ctx>,
        default: Option<&[Statement]>,
        merge_bb: BasicBlock<'ctx>,
        pos: Position,
    ) -> Result<(), CodegenError> {
        self.builder.position_at_end(else_bb);
        if let Some(default_body) = default {
            self.enter_scope();
            for stmt in default_body {
                self.generate_statement(stmt)?;
            }
            self.exit_scope();
            if !self.block_has_terminator() {
                self.builder.build_unconditional_branch(merge_bb)?;
            }
        } else {
            // Coverage-complete switch: the else edge is unreachable through
            // any legal program — only a forged tag (`u0*` bridge, stale
            // pointer, out-of-range `as`-cast enum) lands here. At -O0 that
            // gets a debuggable `llvm.trap` (never libc `abort`: freestanding
            // targets link no libc). Under optimization the forgery is
            // undefined behavior and the edge is a bare `unreachable`, so
            // LLVM keeps the full range assumption (jump tables need no
            // bounds check and the arm never burdens branch layout).
            if self.opt_level > 0 {
                self.builder.build_unreachable()?;
                self.builder.position_at_end(merge_bb);
                return Ok(());
            }
            let trap = inkwell::intrinsics::Intrinsic::find("llvm.trap")
                .and_then(|i| i.get_declaration(&self.module, &[]))
                .ok_or_else(|| {
                    CodegenError::InvalidOperation(
                        "llvm.trap intrinsic unavailable".to_string(),
                        pos,
                    )
                })?;
            self.builder.build_call(trap, &[], "")?;
            self.builder.build_unreachable()?;
        }

        self.builder.position_at_end(merge_bb);
        Ok(())
    }
}
