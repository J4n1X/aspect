//! Sum-type storage layout, plus the construction and pattern-match codegen
//! that reads/writes it (`SumConstruct`, both `is` forms, and — via
//! `copy_sum_payload_field` — `switch`'s binder copies in `statements.rs`).
//! `StructLiteral` stays in `structs.rs`/inline in `expressions.rs` instead
//! of moving here alongside it: a struct literal builds its aggregate
//! directly with `insertvalue` and never touches this module's
//! payload-offset machinery, so there's nothing to share.
//!
//! A sum value is `{ i32 tag, [k x iN] }`: the payload array's element width N
//! is the largest payload alignment, and **every** variant's payload starts at
//! the array — a *uniform* offset, never packed into the tag's padding gap.
//! This is what makes whole-value copies (`load`/`store` of the storage
//! struct) safe: LLVM does not preserve padding bytes across first-class
//! aggregate copies, so any payload byte outside the two fields could be
//! silently dropped. With the uniform offset, all payload bytes live inside
//! field 1 by construction. Construction (and later, destructuring) GEPs the
//! payload through the variant's bare payload struct `{ field0, field1, … }`
//! at field 1's address — relative offsets agree because the array's
//! alignment is ≥ every payload field's.

use inkwell::types::BasicTypeEnum;
use inkwell::values::{BasicValueEnum, IntValue, PointerValue};

use crate::codegen::generator::CodeGenerator;
use crate::codegen::CodegenError;
use crate::lexer::{LangType, Position, TypeBase};
use crate::parser::{Expression, Program};
use crate::symbol::module::SumInfo;

impl<'ctx> CodeGenerator<'ctx> {
    /// Create the named opaque type for every sum, before struct bodies are
    /// set — a struct field of sum type resolves against this cache. Also
    /// seeds the codegen-local variant-field cache (the expression walker is
    /// not threaded the `Program`, mirroring `struct_fields`).
    pub(crate) fn register_sums_opaque(&mut self, program: &Program) {
        for info in program.symbols.sums() {
            let llvm = self.context.opaque_struct_type(&info.name);
            self.sum_types.insert(info.id, llvm);
            let variants = info
                .variants
                .iter()
                .map(|v| v.fields.iter().map(|(_, ty)| *ty).collect())
                .collect();
            self.sum_variant_fields.insert(info.id, variants);
        }
    }

    /// The bare payload struct `{ field0, field1, … }` of one variant, or
    /// `None` for a payload-less variant. Construction and destructuring GEP
    /// through this type at the storage's field-1 address.
    pub(crate) fn sum_payload_type(
        &self,
        sum_id: u32,
        variant: usize,
    ) -> Result<Option<inkwell::types::StructType<'ctx>>, crate::codegen::TypeLoweringError> {
        let fields = &self.sum_variant_fields[&sum_id][variant];
        if fields.is_empty() {
            return Ok(None);
        }
        let elems = self.sum_field_llvm_types(fields)?;
        Ok(Some(self.context.struct_type(&elems, false)))
    }

    fn sum_field_llvm_types(
        &self,
        fields: &[LangType],
    ) -> Result<Vec<BasicTypeEnum<'ctx>>, crate::codegen::TypeLoweringError> {
        fields
            .iter()
            .map(|ty| {
                if ty.is_array() {
                    self.lang_type_to_llvm_array(ty).map(Into::into)
                } else {
                    self.lang_type_to_llvm(ty)
                }
            })
            .collect()
    }

    /// Fill in every sum's storage body. A payload holding another sum by
    /// value can't be sized until that sum's body is set, so this iterates to
    /// fixpoint; by-value cycles are rejected at parse time, so each round
    /// resolves at least one sum.
    pub(crate) fn register_sum_bodies(&mut self, program: &Program) -> Result<(), CodegenError> {
        let mut pending: Vec<&SumInfo> = program.symbols.sums().collect();
        while !pending.is_empty() {
            let before = pending.len();
            let mut still_waiting = Vec::new();
            for info in pending {
                if self.sum_layout_ready(info) {
                    self.set_sum_body(info)?;
                } else {
                    still_waiting.push(info);
                }
            }
            pending = still_waiting;
            if pending.len() == before {
                return Err(CodegenError::TypeError(
                    format!(
                        "cannot lay out sum `{}` — by-value containment cycle survived parsing",
                        pending[0].name
                    ),
                    Position::new(0, 0),
                ));
            }
        }
        Ok(())
    }

    /// True when every type reachable from `info`'s payloads through by-value
    /// members (struct fields, nested payloads, arrays) has a computable size —
    /// i.e. no reachable sum is still opaque.
    fn sum_layout_ready(&self, info: &SumInfo) -> bool {
        info.variants
            .iter()
            .flat_map(|v| v.fields.iter())
            .all(|(_, ty)| self.type_layout_ready(ty))
    }

    fn type_layout_ready(&self, ty: &LangType) -> bool {
        if ty.pointer_depth > 0 {
            return true;
        }
        match ty.base {
            TypeBase::Sum(id) => self
                .sum_types
                .get(&id)
                .is_some_and(|t| !t.is_opaque()),
            TypeBase::Struct(id) => self
                .struct_fields
                .get(&id)
                .is_some_and(|fields| fields.iter().all(|(_, fty)| self.type_layout_ready(fty))),
            _ => true,
        }
    }

    fn set_sum_body(&mut self, info: &SumInfo) -> Result<(), CodegenError> {
        let target_data = self.target_machine.get_target_data();
        let tag: BasicTypeEnum<'ctx> = self.context.i32_type().into();

        // Size/align of the largest *bare payload* struct — not `{i32, …}`
        // views: the payload starts at the uniform offset for every variant.
        let mut max_payload: u64 = 0;
        let mut max_align: u32 = 4;
        for variant in &info.variants {
            if variant.fields.is_empty() {
                continue;
            }
            let field_tys: Vec<LangType> =
                variant.fields.iter().map(|(_, ty)| *ty).collect();
            let elems = self
                .sum_field_llvm_types(&field_tys)
                .map_err(|e| e.without_pos())?;
            let payload = self.context.struct_type(&elems, false);
            max_payload = max_payload.max(target_data.get_store_size(&payload));
            max_align = max_align.max(target_data.get_abi_alignment(&payload));
        }

        let body: Vec<BasicTypeEnum<'ctx>> = if max_payload == 0 {
            // Payload-less sum: tag only.
            vec![tag]
        } else {
            // A payload array of alignment-width ints reserves the space,
            // forces the storage type to the max payload alignment, and —
            // because the array is field 1 — pins every payload byte inside a
            // real field (whole-value copies preserve fields, not padding).
            let unit_bits = std::num::NonZero::new(max_align * 8)
                .expect("payload alignment is never zero");
            let unit = self
                .context
                .custom_width_int_type(unit_bits)
                .expect("payload unit width is a small power of two");
            let units = max_payload.div_ceil(u64::from(max_align));
            vec![
                tag,
                unit.array_type(u32::try_from(units).expect("sum payload unit count fits u32"))
                    .into(),
            ]
        };
        self.sum_types[&info.id].set_body(&body, false);
        Ok(())
    }

    // ─── Construction / pattern-match ─────────────────────────────────────

    /// Shared spine of both `is` forms: evaluate the sum scrutinee once into
    /// an entry-block slot, load the tag, compare against the variant's
    /// constant. Returns the `i1` and the slot (the binding form GEPs
    /// payloads out of it).
    pub(crate) fn emit_sum_probe(
        &mut self,
        scrutinee: &Expression,
        sum_id: u32,
        variant: u32,
        pos: Position,
    ) -> Result<(IntValue<'ctx>, PointerValue<'ctx>), CodegenError> {
        let function = self
            .current_function
            .ok_or(CodegenError::UnexpectedStatement(pos))?;
        let storage = *self
            .sum_types
            .get(&sum_id)
            .ok_or_else(|| CodegenError::TypeError(format!("unregistered sum id {sum_id}"), pos))?;
        let value = self.generate_expression(scrutinee)?;
        let slot = self.build_entry_alloca(function, storage.into(), "is.scrut", pos)?;
        self.builder.build_store(slot, value)?;
        let tag_ptr = self.builder.build_struct_gep(storage, slot, 0, "is.tag")?;
        let tag = self
            .builder
            .build_load(self.context.i32_type(), tag_ptr, "tag")?
            .into_int_value();
        let matched = self.builder.build_int_compare(
            inkwell::IntPredicate::EQ,
            tag,
            self.context.i32_type().const_int(u64::from(variant), false),
            "is",
        )?;
        Ok((matched, slot))
    }

    /// `SumName.Variant(args…)`: write the tag, then each payload field at
    /// the variant's bare payload struct offset (see the module doc for why
    /// `insertvalue` on the storage type can't be used here).
    pub(crate) fn emit_sum_construct(
        &mut self,
        sum_id: u32,
        variant: u32,
        args: &[Expression],
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let storage = *self
            .sum_types
            .get(&sum_id)
            .ok_or_else(|| CodegenError::TypeError(format!("unregistered sum id {sum_id}"), pos))?;
        let tmp = self.builder.build_alloca(storage, "sum.tmp")?;
        let tag_ptr = self.builder.build_struct_gep(storage, tmp, 0, "sum.tag")?;
        self.builder.build_store(
            tag_ptr,
            self.context.i32_type().const_int(u64::from(variant), false),
        )?;
        if !args.is_empty() {
            let payload_ptr = self.builder.build_struct_gep(storage, tmp, 1, "sum.payload")?;
            let payload_ty = self
                .sum_payload_type(sum_id, variant as usize)
                .map_err(|e| e.with_pos(pos))?
                .expect("variant with args has a payload type");
            let field_tys = self.sum_variant_fields[&sum_id][variant as usize].clone();
            for (i, (arg, fty)) in args.iter().zip(field_tys).enumerate() {
                let fptr = self.builder.build_struct_gep(
                    payload_ty,
                    payload_ptr,
                    u32::try_from(i).expect("payload field index fits u32"),
                    "sum.field",
                )?;
                if fty.is_array() {
                    // Array payloads arrive decayed to a pointer — copy the
                    // elements into the payload slot.
                    let src = self.generate_coerced_value(arg, None)?.into_pointer_value();
                    self.copy_sum_payload_field(fptr, src, fty, arg.pos)?;
                } else {
                    let val = self.generate_coerced_value(arg, Some(&fty))?;
                    self.builder.build_store(fptr, val)?;
                }
            }
        }
        Ok(self.builder.build_load(storage, tmp, "sum.val")?)
    }

    /// `scrutinee is Variant(a, _, b)`: probe, then — only when at least one
    /// binder is named — copy the matched fields into fresh locals on the
    /// success edge.
    pub(crate) fn emit_is_binding(
        &mut self,
        scrutinee: &Expression,
        sum_id: u32,
        variant: u32,
        binders: &[Option<(String, LangType)>],
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        let function = self
            .current_function
            .ok_or(CodegenError::UnexpectedStatement(pos))?;
        let (matched, slot) = self.emit_sum_probe(scrutinee, sum_id, variant, pos)?;

        // Binder allocas exist unconditionally (entry block); the copies run
        // only on the matched edge, so a later `&&`-conjunct — which
        // short-circuiting guarantees only evaluates after a match — reads
        // initialized locals, and the success block does too.
        if binders.iter().any(Option::is_some) {
            let bind_bb = self.context.append_basic_block(function, "is.bind");
            let cont_bb = self.context.append_basic_block(function, "is.cont");
            let storage = self.sum_types[&sum_id];
            let payload_ty = self
                .sum_payload_type(sum_id, variant as usize)
                .map_err(|e| e.with_pos(pos))?
                .expect("binding pattern on a payload-less variant");

            let mut copies = Vec::new();
            for (i, binder) in binders.iter().enumerate() {
                let Some((name, field_ty)) = binder else { continue };
                let llvm_ty = if field_ty.is_array() {
                    self.lang_type_to_llvm_array(field_ty)
                        .map_err(|e| e.with_pos(pos))?
                        .into()
                } else {
                    self.lang_type_to_llvm(field_ty).map_err(|e| e.with_pos(pos))?
                };
                let alloca = self.build_entry_alloca(function, llvm_ty, name, pos)?;
                self.add_variable(name.clone(), alloca, llvm_ty, *field_ty, None);
                copies.push((i, alloca, *field_ty));
            }

            self.builder.build_conditional_branch(matched, bind_bb, cont_bb)?;
            self.builder.position_at_end(bind_bb);
            let payload_ptr = self.builder.build_struct_gep(storage, slot, 1, "is.payload")?;
            for (i, alloca, field_ty) in copies {
                let field_ptr = self.builder.build_struct_gep(
                    payload_ty,
                    payload_ptr,
                    u32::try_from(i).expect("payload field index fits u32"),
                    "is.field",
                )?;
                self.copy_sum_payload_field(alloca, field_ptr, field_ty, pos)?;
            }
            self.builder.build_unconditional_branch(cont_bb)?;
            self.builder.position_at_end(cont_bb);
        }
        Ok(matched.into())
    }

    /// Copy one payload field from `src` to `dst`: a `build_memcpy` for
    /// arrays (which never fit in a register), a load+store for everything
    /// else. Shared by sum construction, `is` binding, and `switch`'s
    /// binder-copy loop in `statements.rs` — all three GEP a field out of (or
    /// into) a sum's payload the same way.
    pub(crate) fn copy_sum_payload_field(
        &mut self,
        dst: PointerValue<'ctx>,
        src: PointerValue<'ctx>,
        field_ty: LangType,
        pos: Position,
    ) -> Result<(), CodegenError> {
        if field_ty.is_array() {
            let arr_ty = self
                .lang_type_to_llvm_array(&field_ty)
                .map_err(|e| e.with_pos(pos))?;
            let bytes = self.sizeof_lang_type(&field_ty, pos)?;
            let align = self.target_machine.get_target_data().get_abi_alignment(&arr_ty);
            self.builder.build_memcpy(
                dst,
                align,
                src,
                align,
                self.context.i64_type().const_int(bytes, false),
            )?;
        } else {
            let llvm_ty = self.lang_type_to_llvm(&field_ty).map_err(|e| e.with_pos(pos))?;
            let v = self.builder.build_load(llvm_ty, src, "field")?;
            self.builder.build_store(dst, v)?;
        }
        Ok(())
    }
}
