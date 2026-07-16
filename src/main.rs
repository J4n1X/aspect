use anyhow::{Context, Result};
use clap::{Args, Parser as ClapParser, Subcommand, ValueEnum};
use inkwell::context::Context as LLVMContext;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use aspect::codegen::CodeGenerator;
use aspect::preprocessor::{PreprocessedSource, Preprocessor};
use aspect::parser::{Parser, Program};
use aspect::target::TargetSpec;
use aspect::typechecker::TypeChecker;

#[derive(ClapParser)]
#[command(name = "aspc")]
#[command(about = "Compiler for the Aspect programming language", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum EmitTarget {
    Ir,
    Obj,
    Exe,
}

/// Preprocessor flags shared by every subcommand that lexes source.
#[derive(Args)]
struct PreprocArgs {
    /// Preprocessor define: NAME (flag) or NAME=VALUE (repeatable)
    #[arg(short = 'D', long = "define", value_name = "NAME[=VALUE]")]
    defines: Vec<String>,

    /// Module search root for `$import` (repeatable; module system pending)
    #[arg(short = 'I', long = "include-dir", value_name = "DIR")]
    include_dirs: Vec<PathBuf>,

    /// Compilation target triple, e.g. `x86_64-unknown-linux-gnu` or
    /// `x86_64-pc-windows-msvc`. Defaults to the host triple. Seeds the
    /// `OS_*`/`ARCH_*` preprocessor defines that drive `$ifdef` in every
    /// subcommand, and additionally selects the LLVM target machine for
    /// `compile`/`interpret`.
    #[arg(long = "target", value_name = "TRIPLE", default_value_t = TargetSpec::host().triple().to_string())]
    target: String,
}

impl PreprocArgs {
    /// The resolved compilation target: `--target` if given, the host
    /// triple otherwise (clap already fills in the default).
    fn target_spec(&self) -> TargetSpec {
        TargetSpec::parse(&self.target)
    }
}

#[derive(Subcommand)]
enum Commands {
    /// Tokenize the input file and print tokens
    Lex {
        /// Input file path
        #[arg(value_name = "FILE")]
        file: PathBuf,

        #[command(flatten)]
        preproc: PreprocArgs,
    },
    /// Parse the input file and print the AST
    Parse {
        /// Input file path
        #[arg(value_name = "FILE")]
        file: PathBuf,

        #[command(flatten)]
        preproc: PreprocArgs,
    },
    /// Compile the input file and emit a selected artifact target
    Compile {
        /// Input file path
        #[arg(value_name = "FILE")]
        file: PathBuf,

        #[command(flatten)]
        preproc: PreprocArgs,

        /// Output target kind
        #[arg(short = 'e', long = "emit", value_enum, default_value_t = EmitTarget::Ir)]
        emit: EmitTarget,

        /// Output file path (defaults to stdout)
        #[arg(short, long, value_name = "OUTPUT")]
        output: Option<PathBuf>,

        /// Print IR to stdout even when writing to file
        #[arg(short, long)]
        print: bool,

        /// Optimization level (0-3)
        #[arg(
            short = 'O',
            long = "optimize",
            value_name = "LEVEL",
            default_value = "0"
        )]
        opt_level: u8,

        /// Verify LLVM IR after each optimization pass (slower, useful for debugging)
        #[arg(long)]
        verify_each: bool,
    },
    /// Compile and JIT-execute the input file in-process
    Interpret {
        /// Input file path
        #[arg(value_name = "FILE")]
        file: PathBuf,

        #[command(flatten)]
        preproc: PreprocArgs,
        /// Optimization level (0-3)
        #[arg(
            short = 'O',
            long = "optimize",
            value_name = "LEVEL",
            default_value = "0"
        )]
        opt_level: u8,
        /// Arguments forwarded to the interpreted program's `main(argc, argv)`.
        /// Use `--` to separate them from this CLI's own flags.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, value_name = "ARGS")]
        program_args: Vec<String>,
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Lex { file, preproc } => lex_file(&file, &preproc)?,
        Commands::Parse { file, preproc } => parse_file(&file, &preproc)?,
        Commands::Compile {
            file,
            preproc,
            emit,
            output,
            print,
            opt_level,
            verify_each,
        } => compile_file(
            &file,
            &preproc,
            emit,
            output.as_deref(),
            print,
            opt_level,
            verify_each,
        )?,
        Commands::Interpret {
            file,
            preproc,
            opt_level,
            program_args,
        } => {
            interpret_file(&file, &preproc, opt_level, &program_args)?;
        }
    }

    Ok(())
}

// ── Shared pipeline stages ───────────────────────────────────────────────────

/// Preprocess `path`: seed `-D` defines and `-I` search roots from the CLI,
/// then tokenize with directive expansion. Errors are formatted with their
/// originating file/position via the driver's file registry.
fn preprocess_source(path: &Path, preproc: &PreprocArgs) -> Result<PreprocessedSource> {
    let mut pp = Preprocessor::for_target(&preproc.target_spec());
    for dir in &preproc.include_dirs {
        pp.add_include_dir(dir.clone());
    }
    for spec in &preproc.defines {
        if let Err(e) = pp.add_cli_define(spec) {
            anyhow::bail!("{}", pp.format_error(&e));
        }
    }
    pp.preprocess(path)
        .map_err(|e| anyhow::anyhow!("{}", pp.format_error(&e)))
        .with_context(|| format!("failed to tokenize '{}'", path.display()))
}

/// Tokenize `path` (expanding preprocessor directives) and parse it into a
/// `Program`, formatting parse errors with their originating file/position.
fn parse_program_from(path: &Path, preproc: &PreprocArgs) -> Result<Program> {
    let pp = preprocess_source(path, preproc)?;

    let mut parser = Parser::new(pp.tokens)
        .with_source_files(pp.files)
        .with_module_info(pp.modules, pp.imports);
    parser.parse_program().map_err(|errors| {
        let msgs: Vec<String> = errors.iter().map(|e| parser.format_error(e)).collect();
        anyhow::anyhow!("{}", msgs.join("\n"))
    })
}

/// Full front end: parse, then type-check (stamping resolved types onto the
/// AST). Every command that reaches codegen goes through here.
fn build_program(path: &Path, preproc: &PreprocArgs) -> Result<Program> {
    let mut program = parse_program_from(path, preproc)?;

    let mut typechecker = TypeChecker::new();
    typechecker.check_program(&mut program).map_err(|errors| {
        let mut err_msg = String::new();
        for error in &errors {
            let _ = writeln!(err_msg, "{}", typechecker.format_error(error));
        }
        anyhow::anyhow!(
            "Type checking failed for '{}':\n{}",
            path.display(),
            err_msg.trim_end()
        )
    })?;

    Ok(program)
}

/// Back-end setup shared by `compile` and `interpret`: generate LLVM IR for
/// `program` (module named after the file stem, targeting `preproc.target`)
/// and run optimization passes when `opt_level > 0`.
fn build_codegen<'ctx>(
    context: &'ctx LLVMContext,
    path: &Path,
    preproc: &PreprocArgs,
    program: &Program,
    opt_level: u8,
    verify_each: bool,
) -> Result<CodeGenerator<'ctx>> {
    let module_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("module");

    let mut codegen = CodeGenerator::new(context, module_name, &preproc.target_spec())
        .with_context(|| format!("failed to set up code generation for '{}'", path.display()))?;
    codegen
        .generate(program)
        .with_context(|| format!("failed to generate code for '{}'", path.display()))?;

    if opt_level > 0 {
        codegen
            .optimize(opt_level, verify_each)
            .with_context(|| format!("failed to optimize code for '{}'", path.display()))?;
    }

    Ok(codegen)
}

// ── Subcommands ──────────────────────────────────────────────────────────────

fn lex_file(path: &Path, preproc: &PreprocArgs) -> Result<()> {
    let pp = preprocess_source(path, preproc)?;

    println!("Tokens:");
    println!("-------");
    for token in &pp.tokens {
        let file = pp
            .files
            .get(token.pos.file_id as usize)
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| path.display().to_string());
        println!(
            "{}:{}:{} {:?} {}",
            file, token.pos.line, token.pos.column, token.kind, token.lexeme
        );
    }

    println!("\nTotal tokens: {}", pp.tokens.len());

    Ok(())
}

fn parse_file(path: &Path, preproc: &PreprocArgs) -> Result<()> {
    let program = parse_program_from(path, preproc)?;

    println!("Program AST:");
    println!("============\n");

    if !program.global_vars.is_empty() {
        println!("Global Variables:");
        for global in &program.global_vars {
            println!(
                "  {} {} = {:?}",
                global.var_type, global.name, global.initializer
            );
        }
        println!();
    }

    if !program.string_literals.is_empty() {
        println!("String Literals:");
        for (i, s) in program.string_literals.iter().enumerate() {
            println!("  [{i}]: \"{s}\"");
        }
        println!();
    }

    println!("Functions:");
    for func in &program.functions {
        print!("  fn {}(", func.proto.name);
        for (i, (param_type, param_name)) in func.proto.params.iter().enumerate() {
            if i > 0 {
                print!(", ");
            }
            print!("{param_type} {param_name}");
        }
        println!(") -> {}", func.proto.return_type);

        if func.proto.is_extern {
            println!("    [extern]");
        } else {
            println!("    body: {} statements", func.body.len());
            if !func.body.is_empty() {
                println!("    statements:");
                for (i, stmt) in func.body.iter().enumerate() {
                    println!("      [{i}]: {stmt:#?}");
                }
            }
        }
    }

    println!("\nParsing completed successfully!");

    Ok(())
}

fn compile_file(
    path: &Path,
    preproc: &PreprocArgs,
    emit: EmitTarget,
    output: Option<&std::path::Path>,
    print: bool,
    opt_level: u8,
    verify_each: bool,
) -> Result<()> {
    let program = build_program(path, preproc)?;
    let context = LLVMContext::create();
    let codegen = build_codegen(&context, path, preproc, &program, opt_level, verify_each)?;

    match emit {
        EmitTarget::Ir => {
            if let Some(output_path) = output {
                codegen
                    .write_ir_to_file(output_path)
                    .with_context(|| format!("failed to write IR to '{}'", output_path.display()))?;
                if print {
                    let ir = codegen.print_ir_to_string();
                    println!("{ir}");
                } else {
                    println!("LLVM IR written to: {}", output_path.display());
                }
            } else {
                let ir = codegen.print_ir_to_string();
                println!("{ir}");
            }
        }
        EmitTarget::Obj => {
            let output_path = output
                .map(ToOwned::to_owned)
                .unwrap_or_else(|| path.with_extension("o"));

            codegen
                .write_object_to_file(&output_path)
                .with_context(|| {
                    format!("failed to write object file to '{}'", output_path.display())
                })?;

            println!("Object file written to: {}", output_path.display());

            if print {
                let ir = codegen.print_ir_to_string();
                println!("{ir}");
            }
        }
        EmitTarget::Exe => {
            anyhow::bail!(
                "--emit exe is accepted but not implemented yet; use --emit ir or --emit obj"
            );
        }
    }

    Ok(())
}

fn interpret_file(
    path: &Path,
    preproc: &PreprocArgs,
    opt_level: u8,
    program_args: &[String],
) -> Result<()> {
    let program = build_program(path, preproc)?;
    let context = LLVMContext::create();
    let codegen = build_codegen(&context, path, preproc, &program, opt_level, false)?;

    // argv[0] is the source path by C convention; user args follow.
    let path_str = path.display().to_string();
    let mut argv: Vec<&str> = Vec::with_capacity(program_args.len() + 1);
    argv.push(&path_str);
    argv.extend(program_args.iter().map(String::as_str));

    let result = codegen
        .jit_execute_main(&argv, opt_level)
        .with_context(|| format!("failed to execute 'main' function in '{}'", path.display()))?;

    println!("Execution result: {result}");

    Ok(())
}
