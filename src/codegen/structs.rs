//! Type-struct code generation: LLVM type registration, type lowering through
//! the struct cache, and the address (lvalue) path that field access, field
//! assignment, and `&expr` all rely on.

use inkwell::types::BasicTypeEnum;
use inkwell::values::PointerValue;

use crate::codegen::generator::CodeGenerator;
use crate::codegen::{CodegenError, LangTypeExt};
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

        for info in program.symbols.structs() {
            let field_types: Result<Vec<BasicTypeEnum<'ctx>>, _> = info
                .fields
                .iter()
                .map(|f| self.lang_type_to_llvm(&f.ty))
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
    ) -> Result<BasicTypeEnum<'ctx>, CodegenError> {
        if ty.pointer_depth == 0
            && ty.array_size.is_none()
            && let TypeBase::Struct(id) = ty.base
        {
            let st = self.struct_types.get(&id).ok_or_else(|| {
                CodegenError::TypeError(
                    format!("unregistered type-struct id {id}"),
                    Position::new(0, 0),
                )
            })?;
            return Ok((*st).into());
        }
        ty.to_llvm(self.context)
    }

    /// Field layout index and type for `field` of struct `id`.
    pub(crate) fn struct_field(&self, id: u32, field: &str) -> Option<(usize, LangType)> {
        let fields = self.struct_fields.get(&id)?;
        fields
            .iter()
            .position(|(n, _)| n == field)
            .map(|idx| (idx, fields[idx].1))
    }

    /// Compute the address (and type) of an lvalue expression: a variable, a
    /// dereference, or a field access. This is the single address-producing
    /// path used by field reads, field assignment, and `&expr`.
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
                    let struct_ty = self.lang_type_to_llvm(&bt)?;
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
