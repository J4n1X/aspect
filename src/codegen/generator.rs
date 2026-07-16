use anyhow::{Context as AnyhowContext, Result as AnyhowResult};
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
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

    /// Ordered field layout per type-struct id: `(field name, field type)` in
    /// declaration/GEP-index order. A codegen-local index into the shared
    /// registry (the walker is not threaded the `Program`).
    pub(crate) struct_fields: HashMap<u32, Vec<(String, LangType)>>,

    /// Function-pointer signatures by id (indexed by `TypeBase::FnPtr(u32)`).
    /// Cloned from `program.symbols.fnptr_sigs` during `generate`, since
    /// `walk_expression` doesn't carry the `Program` reference.
    pub(crate) fnptr_sigs: Vec<crate::symbol::module::FnPtrSig>,

    /// Source-file registry, copied from `Program` at the start of
    /// `generate`. Used by `format_error` so a codegen error's position
    /// resolves to the file it actually came from — same mechanism as the
    /// parser and type checker.
    pub(crate) source_files: Vec<std::path::PathBuf>,

    pub(crate) scope: ScopeStack<'ctx>,

    pub(crate) current_function: Option<FunctionValue<'ctx>>,

    /// Return type of the function currently being generated — used in `generate_return`.
    pub(crate) current_function_return_type: Option<LangType>,

    // Loop stack for break/continue support; each entry is (break_bb, continue_bb)
    pub(crate) loop_stack: Vec<(BasicBlock<'ctx>, BasicBlock<'ctx>)>,

    /// While generating a value-block expression, the innermost block's
    /// (result slot, exit block, result type), innermost last. A `return`
    /// inside a value-block stores into the slot and branches to the exit
    /// block instead of returning from the function (see `generate_return`).
    pub(crate) value_block_stack: Vec<(
        inkwell::values::PointerValue<'ctx>,
        BasicBlock<'ctx>,
        LangType,
    )>,
}

impl<'ctx> CodeGenerator<'ctx> {
    /// Creates a new `CodeGenerator` for `target` (see [`TargetSpec::host`]
    /// for the host-defaulting case). Sets up the module's LLVM triple and
    /// data layout from `target`, so downstream codegen ABI decisions (e.g.
    /// struct/int alignment) are correct for the target being compiled for,
    /// not necessarily the machine `aspc` itself is running on.
    ///
    /// # Errors
    ///
    /// Returns [`CodegenError::UnsupportedTarget`] when LLVM can't resolve
    /// `target`'s triple to a usable target/target machine — e.g. an
    /// `aarch64-*` triple when only the x86 backend is compiled into this
    /// `aspc` binary (`--features target-x86`), or a malformed triple string.
    pub fn new(
        context: &'ctx Context,
        module_name: &str,
        target: &TargetSpec,
    ) -> Result<Self, CodegenError> {
        let module = context.create_module(module_name);
        let builder = context.create_builder();

        // Initializes whichever backend(s) this binary was built with for
        // the machine it is *running on* — not `target`. That's sufficient
        // for every triple that backend covers (e.g. the x86 backend covers
        // every `x86_64-*` triple, windows-msvc included; it just changes
        // the object format), and any triple outside that coverage fails
        // cleanly at `Target::from_triple` below rather than here.
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
                RelocMode::Default,
                CodeModel::Default,
            )
            .ok_or_else(|| CodegenError::UnsupportedTarget {
                triple: target.triple().to_string(),
                reason: "LLVM could not create a target machine for this triple".to_string(),
            })?;
        module.set_triple(&triple);
        module.set_data_layout(&target_machine.get_target_data().get_data_layout());

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
        })
    }

    /// Generate LLVM IR from a program
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

        // Generate global variables
        for global in &program.global_vars {
            if let Err(e) = self.generate_global_variable(global) {
                anyhow::bail!(
                    "{}: failed to generate global variable '{}'",
                    self.format_error(&e),
                    global.name
                );
            }
        }

        // Second pass: Generate function bodies. An `asm fn` has no statement
        // body — its body *is* its instructions — so it takes the inline-asm
        // lowering instead. This dispatch is load-bearing: `generate_function`
        // would walk the empty body and synthesise a `ret 0`, silently
        // producing a function that ignores its own assembly.
        for func in &program.functions {
            let result = match &func.body {
                FunctionBody::Asm(spec) => self.generate_asm_function(func, spec),
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
        Ok(())
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

    /// Get the cached target machine for the current platform.
    pub fn get_target_machine(&self) -> &TargetMachine {
        &self.target_machine
    }

    /// Look up a declared/defined function by name.
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
            // `globaldce` runs even here. It is not an optimization in the
            // sense -O0 disclaims: it deletes only symbols nothing can reach,
            // so no code you could step through changes. Skipping it would put
            // the whole unused stdlib in every debug binary — a program that
            // imports std/io would carry all 375 syscall wrappers.
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

        // Keep O1/O2 close to LLVM defaults, and reserve expensive extras for O3.
        match level {
            1 => {
                // No extras for O1.
            }
            3 => {
                pass_options.set_loop_interleaving(true);
                pass_options.set_loop_slp_vectorization(true);
                pass_options.set_merge_functions(true);
                pass_options.set_call_graph_profile(true);
            }
            _ => {
                // O2 (and out-of-range values that map to O2 pipeline): no extras.
            }
        }

        self.module
            .run_passes(passes, target_machine, pass_options)
            .map_err(|e| {
                CodegenError::InvalidOperation(
                    format!("Failed to run optimization passes: {e}"),
                    crate::lexer::Position::new(0, 0),
                )
            })
    }

    /// Run a named LLVM pass pipeline over the module.
    fn run_pass_pipeline(&self, passes: &str, verify_each: bool) -> Result<(), CodegenError> {
        let pass_options = PassBuilderOptions::create();
        pass_options.set_verify_each(verify_each);
        self.module
            .run_passes(passes, self.get_target_machine(), pass_options)
            .map_err(|e| {
                CodegenError::InvalidOperation(
                    format!("Failed to run pass pipeline '{passes}': {e}"),
                    crate::lexer::Position::new(0, 0),
                )
            })
    }

    /// Print the LLVM IR to a string
    pub fn print_ir_to_string(&self) -> String {
        self.module.print_to_string().to_string()
    }

    /// Write LLVM IR to a file
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

    /// Write target object code to a file
    ///
    /// # Errors
    /// Returns `CodegenError` when object emission fails
    pub fn write_object_to_file(&self, path: &std::path::Path) -> Result<(), CodegenError> {
        let target_machine = self.get_target_machine();
        target_machine
            .write_to_file(&self.module, FileType::Object, path)
            .map_err(|e| {
                CodegenError::InvalidOperation(
                    format!(
                        "Failed to write object file to '{}': {e}",
                        path.display()
                    ),
                    crate::lexer::Position::new(0, 0),
                )
            })
    }

    /// JIT-compile the module and run `func_name`, forwarding `args` to it.
    ///
    /// Each `GenericValue` must match the corresponding LLVM parameter type
    /// (build them via `IntType::create_generic_value`,
    /// `FloatType::create_generic_value`, or
    /// `GenericValue::create_generic_value_of_pointer`). Any backing storage
    /// referenced by pointer arguments must remain valid for the duration of
    /// the call.
    ///
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

    /// JIT-compile and call `main(u32 argc, u8** argv) -> i32`, forwarding
    /// `args` as the program's argv. The caller controls the full argv
    /// (including the conventional `argv[0]` program-name slot).
    ///
    /// Returns the value returned by `main`, truncated to `i32`.
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
