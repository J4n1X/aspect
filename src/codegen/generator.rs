use anyhow::{Context as AnyhowContext, Result as AnyhowResult};
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::{FlagBehavior, Module};
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{CodeModel, FileType, InitializationConfig, RelocMode, Target, TargetMachine};
use inkwell::values::{FunctionValue, GenericValue};
use inkwell::OptimizationLevel;
use std::collections::HashMap;
use std::ffi::CString;
use std::os::raw::c_char;

use crate::codegen::scope::ScopeStack;
use crate::codegen::CodegenError;
use crate::parser::{FunctionBody, LangType, Program};
use crate::target::TargetSpec;

pub struct CodeGenerator<'ctx> {
    pub(crate) context: &'ctx Context,
    pub(crate) module: Module<'ctx>,
    pub(crate) builder: Builder<'ctx>,
    pub(crate) target_machine: TargetMachine,

    pub(crate) functions: HashMap<String, FunctionValue<'ctx>>,

    /// Parameter LangTypes per function name — needed for arg coercion at call sites.
    pub(crate) function_lang_params: HashMap<String, Vec<LangType>>,

    /// Return LangType per function name — needed at call sites to detect
    /// struct-by-value (`sret`) returns.
    pub(crate) function_return_types: HashMap<String, LangType>,

    /// While generating a struct-returning function, the hidden `sret`
    /// out-pointer that `return` stores through. `None` for scalar/void returns.
    pub(crate) current_sret: Option<inkwell::values::PointerValue<'ctx>>,

    /// Named LLVM struct type per type-struct id (built in the registration pass).
    pub(crate) struct_types: HashMap<u32, inkwell::types::StructType<'ctx>>,

    /// `(field name, field type)` in GEP-index order — a codegen-local copy of
    /// the shared registry (the walker is not threaded the `Program`).
    pub(crate) struct_fields: HashMap<u32, Vec<(String, LangType)>>,

    /// Indexed by `TypeBase::FnPtr(u32)`; cloned from `program.symbols` during
    /// `generate` since `walk_expression` doesn't carry the `Program`.
    pub(crate) fnptr_sigs: Vec<crate::symbol::module::FnPtrSig>,

    /// Copied from `Program` so `format_error` resolves a codegen error's
    /// position to the file it came from.
    pub(crate) source_files: Vec<std::path::PathBuf>,

    pub(crate) scope: ScopeStack<'ctx>,

    pub(crate) current_function: Option<FunctionValue<'ctx>>,

    /// Return type of the function currently being generated — used in `generate_return`.
    pub(crate) current_function_return_type: Option<LangType>,

    /// For break/continue: each entry is `(break_bb, continue_bb)`.
    pub(crate) loop_stack: Vec<(BasicBlock<'ctx>, BasicBlock<'ctx>)>,

    /// `(result slot, exit block, result type)` per enclosing value-block,
    /// innermost last. A `return` inside one stores into the slot and branches
    /// to the exit block instead of returning from the function.
    pub(crate) value_block_stack: Vec<(
        inkwell::values::PointerValue<'ctx>,
        BasicBlock<'ctx>,
        LangType,
    )>,

    /// True only while folding a *global* initializer, which is order-sensitive
    /// static init: reading another global there yields its start value, so
    /// folding a global reference is correct. Everywhere else a mutable global
    /// is a runtime load, so `const_eval` must refuse to fold it.
    pub(crate) in_global_init: bool,

    /// True when the program defines its own libc allocation function
    /// (STD_NO_LIBC allocator). Such a `malloc`/`free` doesn't honour libc's
    /// contracts, so every function is tagged `"no-builtins"` to keep the
    /// optimizer's name-based allocation reasoning away from it. Off by default.
    pub(crate) disable_builtins: bool,

    /// Callees named only inside `asm fn` bodies — no IR reference exists, so
    /// `globaldce` would strip them. Pinned in `@llvm.used` by `emit_asm_retained`.
    pub(crate) asm_retained: Vec<FunctionValue<'ctx>>,
}

impl<'ctx> CodeGenerator<'ctx> {
    /// Sets the module's LLVM triple and data layout from `target`, so ABI
    /// decisions (struct/int alignment) are correct for the target compiled
    /// for, not necessarily the machine `aspc` runs on.
    ///
    /// # Errors
    ///
    /// Returns [`CodegenError::UnsupportedTarget`] when LLVM can't resolve
    /// `target`'s triple to a usable target machine — e.g. an `aarch64-*`
    /// triple when only the x86 backend is built in, or a malformed triple.
    pub fn new(
        context: &'ctx Context,
        module_name: &str,
        target: &TargetSpec,
    ) -> Result<Self, CodegenError> {
        Self::new_with_reloc(context, module_name, target, RelocMode::Default)
    }

    /// Like [`Self::new`], but selects the LLVM **relocation model** `reloc` —
    /// how `aspc` emits position-independent code. [`RelocMode::PIC`]
    /// additionally stamps a `PIC Level` flag (value 2), matching `-fPIC`;
    /// every other model leaves the module unflagged.
    ///
    /// # Errors
    ///
    /// Same as [`Self::new`]: [`CodegenError::UnsupportedTarget`] when LLVM
    /// can't resolve `target`'s triple to a usable target machine.
    pub fn new_with_reloc(
        context: &'ctx Context,
        module_name: &str,
        target: &TargetSpec,
        reloc: RelocMode,
    ) -> Result<Self, CodegenError> {
        let module = context.create_module(module_name);
        let builder = context.create_builder();

        // Initializes the backend for the machine `aspc` runs on — sufficient
        // for every triple that backend covers (the x86 backend covers every
        // `x86_64-*` triple, just changing object format). A triple outside its
        // coverage fails cleanly at `Target::from_triple` below.
        Target::initialize_native(&InitializationConfig::default()).map_err(|reason| {
            CodegenError::UnsupportedTarget {
                triple: target.triple().to_string(),
                reason,
            }
        })?;

        let triple = target.llvm_triple();
        let llvm_target = Target::from_triple(&triple).map_err(|e| CodegenError::UnsupportedTarget {
            triple: target.triple().to_string(),
            reason: e.to_string(),
        })?;

        // Set module triple and data layout so LLVM uses the correct ABI alignments
        // (e.g. i64 → align 8 on x86-64 instead of defaulting to align 4).
        let target_machine = llvm_target
            .create_target_machine(
                &triple,
                "generic",
                "",
                OptimizationLevel::None,
                reloc,
                CodeModel::Default,
            )
            .ok_or_else(|| CodegenError::UnsupportedTarget {
                triple: target.triple().to_string(),
                reason: "LLVM could not create a target machine for this triple".to_string(),
            })?;
        module.set_triple(&triple);
        module.set_data_layout(&target_machine.get_target_data().get_data_layout());

        // clang uses `FlagBehavior::Max` for this flag, which inkwell 0.9 lacks;
        // `Override` records the identical value and behaves the same for the
        // single-module compilation `aspc` performs (merge semantics only differ
        // when two flagged modules are IR-linked, which never happens here).
        if reloc == RelocMode::PIC {
            module.add_basic_value_flag(
                "PIC Level",
                FlagBehavior::Override,
                context.i32_type().const_int(2, false),
            );
        }

        Ok(Self {
            context,
            module,
            builder,
            target_machine,
            functions: HashMap::new(),
            function_lang_params: HashMap::new(),
            function_return_types: HashMap::new(),
            current_sret: None,
            struct_types: HashMap::new(),
            struct_fields: HashMap::new(),
            fnptr_sigs: Vec::new(),
            source_files: Vec::new(),
            scope: ScopeStack::new(),
            current_function: None,
            current_function_return_type: None,
            loop_stack: Vec::new(),
            value_block_stack: Vec::new(),
            in_global_init: false,
            disable_builtins: false,
            asm_retained: Vec::new(),
        })
    }

    /// # Errors
    /// Returns `CodegenError` if any of the nested functions fail
    /// # Panics
    /// Panics if target machine creation fails, which should not happen with valid targets
    pub fn generate(&mut self, program: &Program) -> AnyhowResult<()> {
        // Seed the file registry so any error we hit below resolves to the
        // file it actually came from, not the entry source.
        self.source_files = program.source_files.clone();

        // Generate global string literals first (they might be referenced by globals)
        for (i, s) in program.string_literals.iter().enumerate() {
            self.generate_string_literal(i, s);
        }

        // Register type-struct LLVM types before anything references them.
        if let Err(e) = self.register_structs(program) {
            anyhow::bail!("{}: failed to register type-struct layouts", self.format_error(&e));
        }

        // Seed the codegen-local FnPtr signature cache from the shared registry.
        self.fnptr_sigs = program.symbols.all_fnptr_sigs().to_vec();

        // Does this program define its own allocator (STD_NO_LIBC)? A defined
        // `malloc`/`calloc`/`realloc`/`free` means every function must opt out
        // of the optimizer's libc-allocation reasoning — see `disable_builtins`.
        self.disable_builtins = program.functions.iter().any(|f| {
            !matches!(f.body, FunctionBody::Extern)
                && matches!(f.proto.name.as_str(), "malloc" | "calloc" | "realloc" | "free")
        });

        // First pass: Declare all functions (for forward references)
        for func in &program.functions {
            if let Err(e) = self.declare_function(func) {
                anyhow::bail!(
                    "{}: failed to declare function '{}'",
                    self.format_error(&e),
                    func.proto.name
                );
            }
        }

        for global in &program.global_vars {
            if let Err(e) = self.generate_global_variable(global) {
                anyhow::bail!(
                    "{}: failed to generate global variable '{}'",
                    self.format_error(&e),
                    global.name
                );
            }
        }

        // Second pass: function bodies. The asm/naked dispatch is load-bearing:
        // `generate_function` would walk their empty statement body and
        // synthesise a `ret 0`, silently ignoring the actual assembly.
        for func in &program.functions {
            let result = match &func.body {
                FunctionBody::Asm(spec) => self.generate_asm_function(func, spec),
                FunctionBody::Naked(spec) => self.generate_naked_function(func, spec),
                FunctionBody::Aspect(stmts) => self.generate_function(func, stmts),
                FunctionBody::Extern => Ok(()),
            };
            if let Err(e) = result {
                anyhow::bail!(
                    "{}: failed to generate function '{}'",
                    self.format_error(&e),
                    func.proto.name
                );
            }
        }

        self.emit_asm_retained();
        self.emit_fltused_if_windows();
        Ok(())
    }

    /// Pin `asm_retained` callees in the module's one `@llvm.used` array,
    /// surviving `globaldce` and `--gc-sections`. `@llvm.used`, not
    /// `compiler.used`: the sole reference is asm text the linker can't follow
    /// either. LangRef fixes the form — ptr array, appending, `llvm.metadata`.
    fn emit_asm_retained(&mut self) {
        if self.asm_retained.is_empty() {
            return;
        }
        // LLVM rejects a repeated entry in `@llvm.used`.
        let mut seen = std::collections::HashSet::new();
        let ptrs: Vec<inkwell::values::PointerValue<'ctx>> = self
            .asm_retained
            .iter()
            .filter(|f| seen.insert(f.as_global_value().as_pointer_value()))
            .map(|f| f.as_global_value().as_pointer_value())
            .collect();

        let ptr_ty = self.context.ptr_type(inkwell::AddressSpace::default());
        let array = ptr_ty.const_array(&ptrs);
        let used = self.module.add_global(array.get_type(), None, "llvm.used");
        used.set_initializer(&array);
        used.set_linkage(inkwell::module::Linkage::Appending);
        used.set_section(Some("llvm.metadata"));
    }

    /// The `_fltused` marker MSVC's CRT references to pull in FP support.
    /// Detecting actual FP use would need a full AST walk, and a spurious 4-byte
    /// symbol is harmless, so we emit it for every Windows target. External
    /// linkage keeps it through `globaldce`; a no-op on non-Windows targets.
    fn emit_fltused_if_windows(&mut self) {
        let is_windows = self
            .module
            .get_triple()
            .as_str()
            .to_string_lossy()
            .to_ascii_lowercase()
            .contains("windows");
        if !is_windows || self.module.get_global("_fltused").is_some() {
            return;
        }
        let i32_ty = self.context.i32_type();
        let fltused = self.module.add_global(i32_ty, None, "_fltused");
        fltused.set_initializer(&i32_ty.const_zero());
        fltused.set_linkage(inkwell::module::Linkage::External);
    }

    /// Format a codegen error with the originating source file prepended,
    /// looking up the file via the error's `pos.file_id`. Mirrors the
    /// parser/typechecker formatters.
    #[must_use]
    pub fn format_error(&self, err: &CodegenError) -> String {
        let Some(pos) = err.position() else {
            return err.to_string();
        };
        match self.source_files.get(pos.file_id as usize) {
            Some(path) => format!("{}:{}:{}: {}", path.display(), pos.line, pos.column, err),
            None => err.to_string(),
        }
    }
    pub fn module(&self) -> &Module<'ctx> {
        &self.module
    }

    pub fn get_target_machine(&self) -> &TargetMachine {
        &self.target_machine
    }

    pub fn get_function(&self, name: &str) -> Option<FunctionValue<'ctx>> {
        self.functions.get(name).copied()
    }

    /// Run optimization passes on the module
    ///
    /// # Arguments
    /// * `level` - Optimization level (0-3), where:
    ///   - 0: No optimizations (default)
    ///   - 1: Basic optimizations
    ///   - 2: Standard optimizations (recommended for release)
    ///   - 3: Aggressive optimizations
    ///
    /// # Errors
    /// Returns `CodegenError` if the passes fail to run
    pub fn optimize(&self, level: u8, verify_each: bool) -> Result<(), CodegenError> {
        if level == 0 {
            // `globaldce` runs even at -O0: it deletes only unreachable symbols,
            // so nothing steppable changes. Skipping it would drag the whole
            // unused stdlib into every debug binary.
            return self.run_pass_pipeline("globaldce", verify_each);
        }

        let target_machine = self.get_target_machine();

        let passes = match level {
            1 => "default<O1>",
            3 => "default<O3>",
            _ => "default<O2>", // 2 or any other value defaults to O2
        };

        let pass_options = PassBuilderOptions::create();
        // Verifying after each pass helps debugging invalid IR but can be expensive.
        pass_options.set_verify_each(verify_each);

        // O1/O2 stay close to LLVM defaults; reserve the expensive extras for O3.
        match level {
            1 => {}
            3 => {
                pass_options.set_loop_interleaving(true);
                pass_options.set_loop_slp_vectorization(true);
                pass_options.set_merge_functions(true);
                pass_options.set_call_graph_profile(true);
            }
            _ => {}
        }

        self.module
            .run_passes(passes, target_machine, pass_options)
            .map_err(|e| {
                CodegenError::Internal(format!("Failed to run optimization passes: {e}"))
            })
    }

    fn run_pass_pipeline(&self, passes: &str, verify_each: bool) -> Result<(), CodegenError> {
        let pass_options = PassBuilderOptions::create();
        pass_options.set_verify_each(verify_each);
        self.module
            .run_passes(passes, self.get_target_machine(), pass_options)
            .map_err(|e| {
                CodegenError::Internal(format!("Failed to run pass pipeline '{passes}': {e}"))
            })
    }

    pub fn print_ir_to_string(&self) -> String {
        self.module.print_to_string().to_string()
    }

    /// # Panics
    /// When writing to the file fails
    /// # Errors
    /// Never
    pub fn write_ir_to_file(&self, path: &std::path::Path) -> Result<(), CodegenError> {
        self.module
            .print_to_file(path)
            .expect("Failed to write LLVM IR to file");
        Ok(())
    }

    /// # Errors
    /// Returns `CodegenError` when object emission fails
    pub fn write_object_to_file(&self, path: &std::path::Path) -> Result<(), CodegenError> {
        let target_machine = self.get_target_machine();
        target_machine
            .write_to_file(&self.module, FileType::Object, path)
            .map_err(|e| {
                CodegenError::Internal(format!(
                    "Failed to write object file to '{}': {e}",
                    path.display()
                ))
            })
    }

    /// Each `GenericValue` must match the corresponding LLVM parameter type,
    /// and any storage behind pointer arguments must stay valid for the call.
    /// Returns the integer return value as `u64`, or `0` for void functions.
    ///
    /// # Errors
    /// Returns an error if the execution engine cannot be created, the
    /// function is not found, or the argument count does not match the
    /// function's parameter count.
    pub fn jit_execute(
        &self,
        func_name: &str,
        args: &[&GenericValue<'ctx>],
        opt_level: u8,
    ) -> AnyhowResult<u64> {
        let level = match opt_level {
            0 => OptimizationLevel::None,
            1 => OptimizationLevel::Less,
            2 => OptimizationLevel::Default,
            3 => OptimizationLevel::Aggressive,
            _ => OptimizationLevel::Default, // Default to O2 for out-of-range values
        };

        let execution_engine = self
            .module
            .create_jit_execution_engine(level)
            .context("failed to create JIT execution engine")?;

        let func = *self
            .functions
            .get(func_name)
            .ok_or_else(|| anyhow::anyhow!("function '{}' not found for JIT execution", func_name))?;

        let expected = func.count_params() as usize;
        if expected != args.len() {
            anyhow::bail!(
                "function '{func_name}' expects {expected} argument(s), got {}",
                args.len()
            );
        }

        unsafe {
            let result = execution_engine.run_function(func, args);
            if func.get_type().get_return_type().is_none() {
                // Void return type: return 0 by convention
                Ok(0)
            } else {
                Ok(result.as_int(false))
            }
        }
    }

    /// Calls `main(u32 argc, u8** argv) -> i32` with `args` as argv (the caller
    /// controls the full argv, including the `argv[0]` program-name slot).
    /// Returns `main`'s value truncated to `i32`.
    ///
    /// # Errors
    /// Returns an error if `main` is missing or has the wrong arity, any
    /// argument contains an interior null byte, `args.len()` exceeds `u32::MAX`,
    /// or the underlying JIT call fails.
    pub fn jit_execute_main(&self, args: &[&str], opt_level: u8) -> AnyhowResult<i32> {
        let main_func = self
            .get_function("main")
            .ok_or_else(|| anyhow::anyhow!("no 'main' function in module"))?;
        let param_count = main_func.count_params();
        if param_count != 2 {
            anyhow::bail!(
                "'main' must take 2 parameters (u32 argc, u8** argv), but takes {param_count}"
            );
        }

        // Keep CStrings and the pointer array alive on the stack so the raw
        // pointer we hand to LLVM stays valid for the synchronous JIT call.
        let cstrings: Vec<CString> = args
            .iter()
            .map(|s| CString::new(*s))
            .collect::<Result<_, _>>()
            .context("program arguments must not contain interior null bytes")?;
        let mut argv_ptrs: Vec<*mut c_char> =
            cstrings.iter().map(|cs| cs.as_ptr() as *mut c_char).collect();
        argv_ptrs.push(std::ptr::null_mut()); // null-terminate argv per C convention

        let argc = u32::try_from(args.len())
            .context("too many program arguments to fit in u32 argc")?;
        let argc_gv = self
            .context
            .i32_type()
            .create_generic_value(u64::from(argc), false);
        let argv_gv =
            unsafe { GenericValue::create_generic_value_of_pointer(&mut argv_ptrs[0]) };

        let raw = self.jit_execute("main", &[&argc_gv, &argv_gv], opt_level)?;
        Ok(raw as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::Parser;
    use crate::target::TargetSpec;
    use crate::typechecker::TypeChecker;

    /// A program that reads and writes a mutable global — the case whose
    /// symbol access differs between relocation models (absolute address vs
    /// PC-relative), so it exercises the position-independent path in a way a
    /// leaf function with no global references would not.
    const SRC: &str = r#"
u32 counter = 0

fn main(u32 argc, u8 **argv) -> i32 {
    counter = counter + 1
    return counter as i32
}
"#;

    fn codegen_for(context: &Context, reloc: RelocMode) -> CodeGenerator<'_> {
        let tokens = crate::lexer::tokenize(SRC.to_string()).expect("lex");
        let mut parser = Parser::new(tokens);
        let mut program = parser.parse_program().expect("parse");
        let mut tc = TypeChecker::new();
        tc.check_program(&mut program).expect("typecheck");
        let mut codegen =
            CodeGenerator::new_with_reloc(context, "reloc_test", &TargetSpec::host(), reloc)
                .expect("codegen setup");
        codegen.generate(&program).expect("generate");
        codegen
    }

    #[test]
    fn pic_reloc_stamps_the_module_with_a_pic_level_flag() {
        let context = Context::create();
        let ir = codegen_for(&context, RelocMode::PIC).print_ir_to_string();
        assert!(
            ir.contains("PIC Level"),
            "PIC build should record a `PIC Level` module flag:\n{ir}"
        );
    }

    #[test]
    fn the_default_reloc_leaves_the_module_unflagged() {
        let context = Context::create();
        let ir = codegen_for(&context, RelocMode::Default).print_ir_to_string();
        assert!(
            !ir.contains("PIC Level"),
            "non-PIC build must not record a `PIC Level` module flag:\n{ir}"
        );
    }

    #[test]
    fn pic_and_static_emit_different_object_code() {
        // The relocation model must reach the *target machine*, not merely the
        // module flag: a mutable global's access lowers to an absolute
        // relocation under `static` and a PC-relative one under `pic`, so the
        // two builds produce different object bytes for the same source.
        let ctx_static = Context::create();
        let cg_static = codegen_for(&ctx_static, RelocMode::Static);
        let static_obj = cg_static
            .get_target_machine()
            .write_to_memory_buffer(&cg_static.module, FileType::Object)
            .expect("emit static object");

        let ctx_pic = Context::create();
        let cg_pic = codegen_for(&ctx_pic, RelocMode::PIC);
        let pic_obj = cg_pic
            .get_target_machine()
            .write_to_memory_buffer(&cg_pic.module, FileType::Object)
            .expect("emit pic object");

        assert_ne!(
            static_obj.as_slice(),
            pic_obj.as_slice(),
            "pic and static builds must produce different object code"
        );
    }
}
