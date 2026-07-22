//! Type-struct code generation: LLVM type registration, type lowering through
//! the struct cache, and the address (lvalue) path that field access, field
//! assignment, and `&expr` all rely on.

use inkwell::types::{BasicType, BasicTypeEnum};
use inkwell::values::PointerValue;

use crate::codegen::generator::CodeGenerator;
use crate::codegen::{CodegenError, LangTypeExt, TypeLoweringError};
use crate::lexer::{LangType, Position, TypeBase};
use crate::parser::{ExprKind, Expression, Program};

/// True when `ty` is a type-struct passed/stored *by value* (not a pointer or
/// array). These cross function boundaries via the `sret`/`byval` ABI.
pub(crate) fn is_struct_value(ty: &LangType) -> bool {
    ty.pointer_depth == 0 && ty.array_size.is_none() && matches!(ty.base, TypeBase::Struct(_))
}

impl<'ctx> CodeGenerator<'ctx> {
    /// Build the named LLVM struct type for every type-struct, then fill in the
    /// bodies. Two passes (opaque-then-body) so by-value/self-referential field
    /// types can refer to structs declared later.
    pub(crate) fn register_structs(&mut self, program: &Program) -> Result<(), CodegenError> {
        for info in program.symbols.structs() {
            let llvm_struct = self.context.opaque_struct_type(&info.name);
            self.struct_types.insert(info.id, llvm_struct);
            let fields = info.fields.iter().map(|f| (f.name.clone(), f.ty)).collect();
            self.struct_fields.insert(info.id, fields);
        }

        // `FieldInfo` records no declaration site, so a bad field type here
        // can't name its own line — the one fabricated position left in codegen.
        for info in program.symbols.structs() {
            let field_types: Result<Vec<BasicTypeEnum<'ctx>>, _> = info
                .fields
                .iter()
                .map(|f| {
                    self.lang_type_to_llvm(&f.ty)
                        .map_err(|e| e.without_pos())
                })
                .collect();
            let field_types = field_types?;
            self.struct_types[&info.id].set_body(&field_types, false);
        }
        Ok(())
    }

    /// Lower a `LangType` to LLVM, resolving struct *values* through the cache.
    /// Pointers/arrays decay to `ptr` exactly as [`LangTypeExt::to_llvm`].
    pub(crate) fn lang_type_to_llvm(
        &self,
        ty: &LangType,
    ) -> Result<BasicTypeEnum<'ctx>, TypeLoweringError> {
        if ty.pointer_depth == 0
            && ty.array_size.is_none()
            && let TypeBase::Struct(id) = ty.base
        {
            let st = self.struct_types.get(&id).ok_or_else(|| {
                TypeLoweringError(format!("unregistered type-struct id {id}"))
            })?;
            return Ok((*st).into());
        }
        ty.to_llvm(self.context)
    }

    /// Lower an array `LangType` to `[N x T]`, resolving type-struct elements
    /// through the cache (unlike the value-only `LangTypeExt::to_llvm`).
    pub(crate) fn lang_type_to_llvm_array(
        &self,
        ty: &LangType,
    ) -> Result<inkwell::types::ArrayType<'ctx>, TypeLoweringError> {
        let array_size = ty
            .array_size
            .ok_or_else(|| TypeLoweringError("Expected array type".to_string()))?;
        // The element strips only the array dimension; pointer depth stays part
        // of the element type (`(i32*)[3]` allocates `[3 x ptr]`).
        let elem_ty = LangType {
            array_size: None,
            ..*ty
        };
        Ok(self.lang_type_to_llvm(&elem_ty)?.array_type(array_size))
    }

    /// Field layout index and type for `field` of struct `id`.
    pub(crate) fn struct_field(&self, id: u32, field: &str) -> Option<(usize, LangType)> {
        let fields = self.struct_fields.get(&id)?;
        fields
            .iter()
            .position(|(n, _)| n == field)
            .map(|idx| (idx, fields[idx].1))
    }

    /// Byte size of a `LangType` against the target data layout — powers
    /// `sizeof(T)`. Consults the cached LLVM struct type so padding is included.
    pub(crate) fn sizeof_lang_type(
        &self,
        ty: &LangType,
        pos: Position,
    ) -> Result<u64, CodegenError> {
        // Arrays: `[N x T]` => N * sizeof(T) with the element's pointer
        // depth preserved (e.g. `(i32*)[3]` is 3 pointer-widths, not 12).
        if let Some(n) = ty.array_size {
            let mut element = *ty;
            element.array_size = None;
            let elem_size = self.sizeof_lang_type(&element, pos)?;
            return Ok(elem_size * u64::from(n));
        }
        let target_data = self.target_machine.get_target_data();
        if ty.pointer_depth > 0 {
            return Ok(u64::from(
                target_data.get_pointer_byte_size(None),
            ));
        }
        match ty.base {
            TypeBase::SInt | TypeBase::UInt | TypeBase::SFloat => Ok(u64::from(ty.size_bits) / 8),
            TypeBase::Bool => Ok(1),
            TypeBase::Void => Err(CodegenError::TypeError(
                "sizeof(u0) is not defined".to_string(),
                pos,
            )),
            TypeBase::Struct(id) => {
                let struct_ty = self.struct_types.get(&id).ok_or_else(|| {
                    CodegenError::TypeError(
                        format!("unregistered type-struct id {id} in sizeof"),
                        pos,
                    )
                })?;
                Ok(target_data.get_store_size(struct_ty))
            }
            // A bare function-pointer value is a pointer.
            TypeBase::FnPtr(_) => Ok(u64::from(target_data.get_pointer_byte_size(None))),
            // An enum is represented as an `i32` — 4 bytes.
            TypeBase::Enum(_) => Ok(4),
            // Never reaches codegen — an unresolved obligation is a fatal error.
            TypeBase::Unresolved => {
                unreachable!("unresolved type reached codegen — an unresolved obligation escaped the checker")
            }
        }
    }

    /// Address (and type) of an lvalue — the single address-producing path
    /// used by field reads, field assignment, and `&expr`.
    pub(crate) fn emit_address(
        &mut self,
        expr: &Expression,
    ) -> Result<(PointerValue<'ctx>, LangType), CodegenError> {
        match &expr.kind {
            ExprKind::Variable(name) => {
                let v = self
                    .scope
                    .lookup_any(name)
                    .ok_or_else(|| CodegenError::UndefinedVariable(name.clone(), expr.pos))?;
                Ok((v.ptr(), v.lang_type()))
            }
            ExprKind::Dereference(inner) => {
                // The address is the pointer value itself.
                let ptr = self.generate_expression(inner)?.into_pointer_value();
                Ok((ptr, inner.expr_type.pointee()))
            }
            ExprKind::FieldAccess { base, field } => {
                let (struct_addr, id) = self.struct_address_of(base)?;
                let (idx, field_ty) = self.struct_field(id, field).ok_or_else(|| {
                    CodegenError::TypeError(
                        format!("unknown field '{field}' on type-struct id {id}"),
                        expr.pos,
                    )
                })?;
                let struct_ty = self.struct_types[&id];
                let field_ptr = self.builder.build_struct_gep(
                    struct_ty,
                    struct_addr,
                    u32::try_from(idx).expect("field index out of range"),
                    field,
                )?;
                Ok((field_ptr, field_ty))
            }
            _ => Err(CodegenError::InvalidOperation(
                "cannot take the address of a non-lvalue expression".to_string(),
                expr.pos,
            )),
        }
    }

    /// Resolve the address of the struct that `base` denotes, plus its id.
    /// A struct *value* yields the address of its storage; a single-level
    /// pointer-to-struct auto-dereferences (its value *is* the address).
    fn struct_address_of(
        &mut self,
        base: &Expression,
    ) -> Result<(PointerValue<'ctx>, u32), CodegenError> {
        let bt = base.expr_type;
        let TypeBase::Struct(id) = bt.base else {
            return Err(CodegenError::TypeError(
                format!("expected a type-struct, found '{bt}'"),
                base.pos,
            ));
        };
        match bt.pointer_depth {
            0 => {
                // An addressable lvalue yields its storage directly; an rvalue
                // struct (literal, function return, ...) is materialised into a
                // temporary slot so its fields can be read.
                let addr = if matches!(
                    base.kind,
                    ExprKind::Variable(_) | ExprKind::Dereference(_) | ExprKind::FieldAccess { .. }
                ) {
                    self.emit_address(base)?.0
                } else {
                    let val = self.generate_expression(base)?;
                    let struct_ty = self
                        .lang_type_to_llvm(&bt)
                        .map_err(|e| e.with_pos(base.pos))?;
                    let tmp = self.builder.build_alloca(struct_ty, "struct.tmp")?;
                    self.builder.build_store(tmp, val)?;
                    tmp
                };
                Ok((addr, id))
            }
            1 => {
                let ptr = self.generate_expression(base)?.into_pointer_value();
                Ok((ptr, id))
            }
            _ => Err(CodegenError::TypeError(
                format!("cannot access a field through '{bt}'"),
                base.pos,
            )),
        }
    }
}
