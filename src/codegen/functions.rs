use inkwell::AddressSpace;
use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::module::Linkage;
use inkwell::types::{AnyType, BasicMetadataTypeEnum, BasicType};
use inkwell::values::{BasicValueEnum, FunctionValue};

use crate::codegen::generator::CodeGenerator;
use crate::codegen::structs::is_struct_value;
use crate::codegen::{CodegenError, LangTypeExt};
use crate::lexer::{Position, TypeBase};
use crate::parser::{Expression, Function, FunctionBody, LangType, Statement};

/// Prepared LLVM call arguments plus the optional `sret` result slot
/// (its pointer and struct type) that the caller must load after the call.
type PreparedCallArgs<'ctx> = (
    Vec<inkwell::values::BasicMetadataValueEnum<'ctx>>,
    Option<(
        inkwell::values::PointerValue<'ctx>,
        inkwell::types::BasicTypeEnum<'ctx>,
    )>,
);

/// Symbols an entry point needs to keep, whatever the source says: `main` is
/// called by the C runtime and looked up by name by the JIT, `_start` is the
/// linker's default entry for a freestanding binary.
const IMPLICITLY_PUBLIC: [&str; 2] = ["main", "_start"];

/// `None` means LLVM's default (external).
///
/// A whole program is a *single* LLVM module, so internal linkage is free at
/// the call site and lets `globaldce` delete an unreachable function;
/// defaulting to external leaves the whole unused stdlib in every binary.
/// `extern fn` must stay external — internal linkage on a body-less
/// declaration is invalid IR. Linkage tracks the `export` axis, not `public`:
/// `public` is parse-time module visibility and stays internally linked.
fn linkage_for(func: &Function) -> Option<Linkage> {
    match func.body {
        FunctionBody::Extern => None,
        _ if IMPLICITLY_PUBLIC.contains(&func.proto.name.as_str()) => None,
        _ if func.proto.export => None,
        _ => Some(Linkage::Internal),
    }
}

/// RAII guard that sets `current_function` / `current_function_return_type`
/// on creation and clears them on drop.
pub(crate) struct FunctionScope<'a, 'ctx> {
    cg: &'a mut CodeGenerator<'ctx>,
}

impl<'a, 'ctx> FunctionScope<'a, 'ctx> {
    fn new(
        cg: &'a mut CodeGenerator<'ctx>,
        func: FunctionValue<'ctx>,
        return_type: LangType,
    ) -> Self {
        cg.current_function = Some(func);
        cg.current_function_return_type = Some(return_type);
        Self { cg }
    }
}

impl Drop for FunctionScope<'_, '_> {
    fn drop(&mut self) {
        self.cg.current_function = None;
        self.cg.current_function_return_type = None;
    }
}

impl<'ctx> CodeGenerator<'ctx> {
    /// Build the LLVM argument list for a call, applying the struct ABI:
    /// a hidden `sret` slot pointer is prepended for struct-returning callees,
    /// and struct *value* arguments are spilled to a temp and passed by pointer
    /// (`byval`). Returns the prepared args plus the sret slot (if any).
    fn build_call_args(
        &mut self,
        name: &str,
        args: &[Expression],
    ) -> Result<PreparedCallArgs<'ctx>, CodegenError> {
        let param_types = self
            .function_lang_params
            .get(name)
            .cloned()
            .unwrap_or_default();
        let ret_ty = self.function_return_types.get(name).copied();

        let mut arg_values = Vec::with_capacity(args.len() + 1);

        // sret: caller allocates the result slot and passes it as arg 0.
        let sret_slot = if let Some(rt) = ret_ty.filter(is_struct_value) {
            let struct_ty = self
                .lang_type_to_llvm(&rt)
                .map_err(|e| match args.first() {
                    Some(a) => e.with_pos(a.pos),
                    None => e.without_pos(),
                })?;
            let slot = self.builder.build_alloca(struct_ty, "sret.tmp")?;
            arg_values.push(slot.into());
            Some((slot, struct_ty))
        } else {
            None
        };

        for (i, arg) in args.iter().enumerate() {
            let target_ty = param_types.get(i);
            if let Some(t) = target_ty.filter(|t| is_struct_value(t)) {
                // byval: materialise the value into a temp and pass its address.
                let val = self.generate_coerced_value(arg, Some(t))?;
                let struct_ty = self.lang_type_to_llvm(t).map_err(|e| e.with_pos(arg.pos))?;
                let tmp = self.builder.build_alloca(struct_ty, "byval.tmp")?;
                self.builder.build_store(tmp, val)?;
                arg_values.push(tmp.into());
            } else {
                let val = self.generate_coerced_value(arg, target_ty)?;
                arg_values.push(val.into());
            }
        }
        Ok((arg_values, sret_slot))
    }

    /// `sret(%Struct)` / `byval(%Struct)` type attribute for a struct value type.
    fn struct_abi_attribute(&self, kind: &str, ty: &LangType) -> Attribute {
        let TypeBase::Struct(id) = ty.base else {
            unreachable!("struct_abi_attribute called on non-struct type");
        };
        let struct_ty = self.struct_types[&id];
        self.context.create_type_attribute(
            Attribute::get_named_enum_kind_id(kind),
            struct_ty.as_any_type_enum(),
        )
    }

    /// Linkage is decided here — see [`linkage_for`].
    pub(crate) fn declare_function(
        &mut self,
        func: &Function,
    ) -> Result<FunctionValue<'ctx>, CodegenError> {
        let param_lang_types: Vec<LangType> = func.proto.params.iter().map(|(ty, _)| *ty).collect();

        let ret_ty = func.proto.return_type;
        let ret_is_struct = is_struct_value(&ret_ty);
        let ptr_ty = self.context.ptr_type(AddressSpace::default());

        // Build the LLVM parameter list. A struct-by-value return prepends a
        // hidden `sret` pointer; struct-by-value params are lowered to `byval`
        // pointers.
        let mut llvm_params: Vec<BasicMetadataTypeEnum<'ctx>> = Vec::new();
        if ret_is_struct {
            llvm_params.push(ptr_ty.into());
        }
        for (ty, _) in &func.proto.params {
            if is_struct_value(ty) {
                llvm_params.push(ptr_ty.into());
            } else {
                llvm_params.push(
                    ty.to_llvm(self.context)
                        .map_err(|e| e.with_pos(func.proto.pos))?
                        .into(),
                );
            }
        }

        // Return type: `void` for struct (sret) or void returns.
        let fn_type = if ret_is_struct || ret_ty.is_void() {
            self.context.void_type().fn_type(&llvm_params, false)
        } else {
            ret_ty
                .to_llvm(self.context)
                .map_err(|e| e.with_pos(func.proto.pos))?
                .fn_type(&llvm_params, false)
        };

        let function = self
            .module
            .add_function(&func.proto.name, fn_type, linkage_for(func));

        // With a program-defined allocator (STD_NO_LIBC), no emitted function
        // may be optimized against libc's contracts — a name-matched `calloc`
        // would draw the optimizer's allocation-size/`noalias`/zeroed reasoning
        // and miscompile callers at -O2. `nobuiltin` on the definition plus the
        // `"no-builtins"` (`-fno-builtin`) attribute stops that both ways.
        if self.disable_builtins && !matches!(func.body, FunctionBody::Extern) {
            let kind_id = Attribute::get_named_enum_kind_id("nobuiltin");
            function.add_attribute(
                AttributeLoc::Function,
                self.context.create_enum_attribute(kind_id, 0),
            );
            function.add_attribute(
                AttributeLoc::Function,
                self.context.create_string_attribute("no-builtins", ""),
            );
        }

        // Attach sret / byval type attributes and name the real parameters.
        let offset = u32::from(ret_is_struct);
        if ret_is_struct {
            let attr = self.struct_abi_attribute("sret", &ret_ty);
            function.add_attribute(AttributeLoc::Param(0), attr);
        }
        for (i, (ty, param_name)) in func.proto.params.iter().enumerate() {
            let idx = u32::try_from(i).expect("Parameter index out of bounds") + offset;
            let param = function.get_nth_param(idx).unwrap();
            param.set_name(param_name);
            if is_struct_value(ty) {
                let attr = self.struct_abi_attribute("byval", ty);
                function.add_attribute(AttributeLoc::Param(idx), attr);
            }
        }

        self.functions.insert(func.proto.name.clone(), function);
        self.function_lang_params
            .insert(func.proto.name.clone(), param_lang_types);
        self.function_return_types
            .insert(func.proto.name.clone(), ret_ty);
        Ok(function)
    }

    pub(crate) fn generate_function(
        &mut self,
        func: &Function,
        stmts: &[Statement],
    ) -> Result<(), CodegenError> {
        let function = *self.functions.get(&func.proto.name).ok_or_else(|| {
            CodegenError::UndefinedFunction(func.proto.name.clone(), func.proto.pos)
        })?;

        let mut scope = FunctionScope::new(self, function, func.proto.return_type);
        let cg = &mut scope.cg;

        let entry_block = cg.context.append_basic_block(function, "entry");
        cg.builder.position_at_end(entry_block);

        cg.enter_scope();

        // Capture the hidden sret out-pointer (param 0) for struct returns.
        let ret_is_struct = is_struct_value(&func.proto.return_type);
        cg.current_sret = if ret_is_struct {
            Some(function.get_nth_param(0).unwrap().into_pointer_value())
        } else {
            None
        };
        let offset = u32::from(ret_is_struct);

        for (i, (param_type, param_name)) in func.proto.params.iter().enumerate() {
            let idx = u32::try_from(i).expect("Parameter index out of bounds") + offset;
            let param_value = function.get_nth_param(idx).unwrap();

            if is_struct_value(param_type) {
                // `byval`: the incoming pointer already addresses a caller-made
                // copy — use it directly as the variable's storage, no re-copy.
                let struct_ty = cg
                    .lang_type_to_llvm(param_type)
                    .map_err(|e| e.with_pos(func.proto.pos))?;
                cg.add_variable(
                    param_name.clone(),
                    param_value.into_pointer_value(),
                    struct_ty,
                    *param_type,
                    None,
                );
            } else {
                let param_llvm_type = param_type
                    .to_llvm(cg.context)
                    .map_err(|e| e.with_pos(func.proto.pos))?;
                let alloca = cg.builder.build_alloca(param_llvm_type, param_name)?;
                cg.builder.build_store(alloca, param_value)?;
                cg.add_variable(
                    param_name.clone(),
                    alloca,
                    param_llvm_type,
                    *param_type,
                    None,
                );
            }
        }

        for stmt in stmts {
            cg.generate_statement(stmt)?;
        }

        // Synthesise a return when the body left the block open.
        /*if !cg.block_has_terminator() {
            if ret_is_struct {
                // Store a zeroed struct through the sret pointer, return void.
                let struct_ty = cg
                    .lang_type_to_llvm(&func.proto.return_type)
                    .map_err(|e| e.with_pos(func.proto.pos))?;
                let sret_ptr = cg.current_sret.expect("sret pointer set for struct return");
                cg.builder.build_store(sret_ptr, struct_ty.const_zero())?;
                cg.builder.build_return(None)?;
            } else if func.proto.return_type.is_void() {
                cg.builder.build_return(None)?;
            } else {
                let zero = cg
                    .get_zero_value(&func.proto.return_type)
                    .map_err(|e| e.with_pos(func.proto.pos))?;
                cg.builder.build_return(Some(&zero))?;
            }
        }*/

        // If there's no return statement, we throw
        if !cg.block_has_terminator() {
            if func.proto.return_type.is_void() {
                cg.builder.build_return(None)?;
            } else {
                cg.builder.build_unreachable()?; // typechecker guarantees all reachable paths returned
            }
        }

        cg.current_sret = None;
        cg.exit_scope();
        // FunctionScope::drop() clears current_function + current_function_return_type

        Ok(())
    }

    /// Emit a call, applying the struct ABI. Returns the result value
    /// (for sret returns, the struct loaded from the caller's slot), or `None`
    /// for a void call.
    fn build_abi_call(
        &mut self,
        name: &str,
        args: &[Expression],
        pos: Position,
    ) -> Result<Option<BasicValueEnum<'ctx>>, CodegenError> {
        let function = *self
            .functions
            .get(name)
            .ok_or_else(|| CodegenError::UndefinedFunction(name.to_string(), pos))?;

        let (arg_values, sret_slot) = self.build_call_args(name, args)?;
        let call_result = self.builder.build_call(function, &arg_values, "call")?;

        if let Some((slot, struct_ty)) = sret_slot {
            // The real result was written through the sret pointer; load it.
            return Ok(Some(self.builder.build_load(
                struct_ty,
                slot,
                "sret.load",
            )?));
        }
        Ok(call_result.try_as_basic_value().basic())
    }

    /// Generate a function call as an expression (must return a non-void value).
    pub(crate) fn generate_function_call(
        &mut self,
        name: &str,
        args: &[Expression],
        pos: Position,
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        self.build_abi_call(name, args, pos)?
            .ok_or_else(|| CodegenError::MissingReturn(name.to_string(), pos))
    }

    /// Generate a function call as a statement (void returns are acceptable).
    pub(crate) fn generate_function_call_statement(
        &mut self,
        name: &str,
        args: &[Expression],
        pos: Position,
    ) -> Result<(), CodegenError> {
        self.build_abi_call(name, args, pos)?;
        Ok(())
    }

    /// Emit an indirect call through a function-pointer value. Returns the
    /// call's basic result, or `None` for a void-returning call. Shared by
    /// the expression and statement codegen paths.
    fn build_indirect_call_inner(
        &mut self,
        callee: &Expression,
        args: &[Expression],
    ) -> Result<Option<BasicValueEnum<'ctx>>, CodegenError> {
        // The call-site position is the callee's own — the parser stamps an
        // `IndirectCall` node with `callee.pos`.
        let pos = callee.pos;
        let id = match callee.expr_type.base {
            TypeBase::FnPtr(id) if callee.expr_type.pointer_depth == 0 => id,
            _ => {
                return Err(CodegenError::TypeError(
                    format!(
                        "callee of indirect call has non-fn-ptr type '{}'",
                        callee.expr_type
                    ),
                    pos,
                ));
            }
        };
        let sig = self.fnptr_sigs.get(id as usize).cloned().ok_or_else(|| {
            CodegenError::TypeError(format!("unregistered fn-ptr signature id {id}"), pos)
        })?;

        let callee_val = self.generate_expression(callee)?;
        let callee_ptr = callee_val.into_pointer_value();

        // Reconstruct the LLVM function type from the registered signature.
        let param_types: Result<Vec<_>, _> = sig
            .params
            .iter()
            .map(|t| self.lang_type_to_llvm(t).map_err(|e| e.with_pos(pos)))
            .collect();
        let param_types = param_types?;
        let param_metas: Vec<BasicMetadataTypeEnum<'ctx>> =
            param_types.iter().map(|t| (*t).into()).collect();
        let fn_ty = if sig.return_type.is_void() {
            self.context.void_type().fn_type(&param_metas, false)
        } else {
            self.lang_type_to_llvm(&sig.return_type)
                .map_err(|e| e.with_pos(pos))?
                .fn_type(&param_metas, false)
        };

        // Coerce each argument to the registered parameter type, just like
        // direct calls. Struct by-value isn't yet plumbed through fn-ptrs.
        let mut arg_values: Vec<inkwell::values::BasicMetadataValueEnum<'ctx>> =
            Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            let target = sig.params.get(i);
            let val = self.generate_coerced_value(arg, target)?;
            arg_values.push(val.into());
        }

        let call =
            self.builder
                .build_indirect_call(fn_ty, callee_ptr, &arg_values, "indirect_call")?;
        Ok(call.try_as_basic_value().basic())
    }

    /// Indirect call used as an expression — errors on a void return.
    pub(crate) fn generate_indirect_call(
        &mut self,
        callee: &Expression,
        args: &[Expression],
    ) -> Result<BasicValueEnum<'ctx>, CodegenError> {
        self.build_indirect_call_inner(callee, args)?
            .ok_or_else(|| CodegenError::MissingReturn("<indirect call>".to_string(), callee.pos))
    }

    /// Indirect call used as a statement — void returns are accepted.
    pub(crate) fn generate_indirect_call_statement(
        &mut self,
        callee: &Expression,
        args: &[Expression],
    ) -> Result<(), CodegenError> {
        self.build_indirect_call_inner(callee, args)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use crate::codegen::CodeGenerator;
    use crate::parser::Parser;
    use crate::target::TargetSpec;
    use crate::typechecker::TypeChecker;
    use inkwell::context::Context;

    /// Compile `source` to an LLVM IR string.
    fn ir_for(source: &str, context: &Context) -> String {
        let tokens = crate::lexer::tokenize(source.to_string()).expect("lex");
        let mut parser = Parser::new(tokens);
        let mut program = parser.parse_program().expect("parse");
        let mut tc = TypeChecker::new();
        tc.check_program(&mut program).expect("typecheck");
        let mut codegen = CodeGenerator::new(context, "linkage_test", &TargetSpec::host())
            .expect("codegen setup");
        codegen.generate(&program).expect("generate");
        codegen.print_ir_to_string()
    }

    /// The `define` line for `name`, panicking if it was never emitted.
    fn define_of<'a>(ir: &'a str, name: &str) -> &'a str {
        ir.lines()
            .find(|l| l.starts_with("define") && l.contains(&format!("@{name}(")))
            .unwrap_or_else(|| panic!("no definition of `{name}` in:\n{ir}"))
    }

    const SRC: &str = r#"
extern fn puts(u8 *s) -> i32

fn helper() -> i32 { return 1 }

public fn visible() -> i32 { return 2 }

export fn exported() -> i32 { return 3 }

fn main(u32 argc, u8 **argv) -> i32 {
    return helper() + visible() + exported() + puts("x")
}
"#;

    /// Internal linkage is what lets `globaldce` delete an unreachable
    /// function. Emitting these as external — LLVM's default, and what
    /// `add_function(.., None)` gives you — leaves the whole unused stdlib in
    /// every binary, and no optimization level can recover it: the linkage is
    /// a promise that some other object file might call in.
    #[test]
    fn a_private_function_gets_internal_linkage() {
        let ctx = Context::create();
        let ir = ir_for(SRC, &ctx);
        assert!(
            define_of(&ir, "helper").contains(" internal "),
            "expected `helper` to be internal, got: {}",
            define_of(&ir, "helper")
        );
    }

    /// `public` is module *visibility*, not linkage: a `public fn` that no
    /// foreign consumer needs stays internally linked (and thus collectable),
    /// exactly like a private one. Only `export` changes the linkage.
    #[test]
    fn a_public_but_unexported_function_is_still_internal() {
        let ctx = Context::create();
        let ir = ir_for(SRC, &ctx);
        assert!(
            define_of(&ir, "visible").contains(" internal "),
            "expected `public fn visible` to be internal, got: {}",
            define_of(&ir, "visible")
        );
    }

    #[test]
    fn export_and_the_entry_point_stay_external() {
        let ctx = Context::create();
        let ir = ir_for(SRC, &ctx);
        for name in ["exported", "main"] {
            assert!(
                !define_of(&ir, name).contains(" internal "),
                "`{name}` must stay external, got: {}",
                define_of(&ir, name)
            );
        }
    }

    /// An `extern fn` is a body-less declaration of something defined
    /// elsewhere; internal linkage on it is invalid IR, not merely useless.
    #[test]
    fn an_extern_declaration_is_never_internalised() {
        let ctx = Context::create();
        let ir = ir_for(SRC, &ctx);
        let decl = ir
            .lines()
            .find(|l| l.starts_with("declare") && l.contains("@puts("))
            .expect("no declaration of `puts`");
        assert!(
            !decl.contains(" internal "),
            "extern declaration must stay external, got: {decl}"
        );
    }
}
