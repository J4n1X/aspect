//! Sum-type storage layout.
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

use crate::codegen::generator::CodeGenerator;
use crate::codegen::CodegenError;
use crate::lexer::{LangType, Position, TypeBase};
use crate::parser::Program;
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
}
