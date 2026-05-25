use anyhow::{Context as AnyhowContext, Result as AnyhowResult};
use inkwell::basic_block::BasicBlock;
use inkwell::builder::Builder;
use inkwell::context::Context;
use inkwell::module::Module;
use inkwell::passes::PassBuilderOptions;
use inkwell::targets::{CodeModel, InitializationConfig, RelocMode, Target, TargetMachine};
use inkwell::values::FunctionValue;
use inkwell::OptimizationLevel;
use std::collections::HashMap;

use crate::codegen::scope::ScopeStack;
use crate::codegen::CodegenError;
use crate::parser::{LangType, Program};

pub struct CodeGenerator<'ctx> {
    pub(crate) context: &'ctx Context,
    pub(crate) module: Module<'ctx>,
    pub(crate) builder: Builder<'ctx>,
    pub(crate) target: Target,

    pub(crate) functions: HashMap<String, FunctionValue<'ctx>>,

    /// Parameter LangTypes per function name — needed for arg coercion at call sites.
    pub(crate) function_lang_params: HashMap<String, Vec<LangType>>,

    pub(crate) scope: ScopeStack<'ctx>,

    pub(crate) current_function: Option<FunctionValue<'ctx>>,

    /// Return type of the function currently being generated — used in `generate_return`.
    pub(crate) current_function_return_type: Option<LangType>,

    // Loop stack for break/continue support; each entry is (break_bb, continue_bb)
    pub(crate) loop_stack: Vec<(BasicBlock<'ctx>, BasicBlock<'ctx>)>,
}

impl<'ctx> CodeGenerator<'ctx> {
    /// Creates a new `CodeGenerator` with the given LLVM context and module name.
    ///
    /// # Panics
    ///
    /// Panics if the default target triple cannot be resolved to a valid target.
    #[must_use]
    pub fn new(context: &'ctx Context, module_name: &str) -> Self {
        let module = context.create_module(module_name);
        let builder = context.create_builder();

        // Initialize target
        Target::initialize_native(&InitializationConfig::default())
            .expect("Failed to initialize native target");

        // TODO: Make target configurable
        let triple = TargetMachine::get_default_triple();
        let target = Target::from_triple(&triple).expect("Failed to get target from triple");

        // Set module triple and data layout so LLVM uses the correct ABI alignments
        // (e.g. i64 → align 8 on x86-64 instead of defaulting to align 4).
        let target_machine = target
            .create_target_machine(
                &triple,
                "generic",
                "",
                OptimizationLevel::None,
                RelocMode::Default,
                CodeModel::Default,
            )
            .expect("Failed to create target machine for data layout");
        module.set_triple(&triple);
        module.set_data_layout(&target_machine.get_target_data().get_data_layout());

        Self {
            context,
            module,
            builder,
            target,
            functions: HashMap::new(),
            function_lang_params: HashMap::new(),
            scope: ScopeStack::new(),
            current_function: None,
            current_function_return_type: None,
            loop_stack: Vec::new(),
        }
    }

    /// Generate LLVM IR from a program
    /// # Errors
    /// Returns `CodegenError` if any of the nested functions fail
    /// # Panics
    /// Panics if target machine creation fails, which should not happen with valid targets
    pub fn generate(&mut self, program: &Program) -> AnyhowResult<()> {
        // Generate global string literals first (they might be referenced by globals)
        for (i, s) in program.string_literals.iter().enumerate() {
            self.generate_string_literal(i, s);
        }

        // First pass: Declare all functions (for forward references)
        for func in &program.functions {
            self.declare_function(func)
                .with_context(|| format!("failed to declare function '{}'", func.proto.name))?;
        }

        // Generate global variables
        for global in &program.global_vars {
            self.generate_global_variable(global)
                .with_context(|| format!("failed to generate global variable '{}'", global.name))?;
        }

        // Second pass: Generate function bodies
        for func in &program.functions {
            if !func.proto.is_extern {
                self.generate_function(func).with_context(|| {
                    format!("failed to generate function '{}'", func.proto.name)
                })?;
            }
        }
        Ok(())
    }
    /// Get the LLVM module
    pub fn module(&self) -> &Module<'ctx> {
        &self.module
    }

    /// Get a target machine for the current platform
    ///
    /// # Errors
    /// Returns `CodegenError` if the target machine cannot be created
    ///
    /// # Panics
    /// Panics if target machine creation fails unexpectedly
    pub fn get_target_machine(&self) -> Result<TargetMachine, CodegenError> {
        let opt = OptimizationLevel::Default;
        let reloc = RelocMode::Default;
        let model = CodeModel::Default;
        let target_machine = self
            .target
            .create_target_machine(
                &TargetMachine::get_default_triple(),
                "generic",
                "",
                opt,
                reloc,
                model,
            )
            .context("failed to create target machine")
            .unwrap();
        Ok(target_machine)
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
            return Ok(());
        }

        let target_machine = self.get_target_machine()?;

        // Build the pass pipeline string based on optimization level
        let passes = match level {
            1 => "default<O1>",
            3 => "default<O3>",
            _ => "default<O2>", // 2 or any other value defaults to O2
        };

        let pass_options = PassBuilderOptions::create();
        // Verifying after each pass helps debugging invalid IR but can be expensive.
        pass_options.set_verify_each(verify_each);
        pass_options.set_loop_interleaving(true);
        pass_options.set_merge_functions(true);
        pass_options.set_loop_slp_vectorization(true);
        pass_options.set_call_graph_profile(true);

        self.module
            .run_passes(passes, &target_machine, pass_options)
            .map_err(|e| {
                CodegenError::InvalidOperation(
                    format!("Failed to run optimization passes: {e}"),
                    crate::lexer::Position { line: 0, column: 0 },
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
}
