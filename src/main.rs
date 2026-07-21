use anyhow::{Context, Result};
use clap::{Args, CommandFactory, Parser as ClapParser, Subcommand, ValueEnum};
use inkwell::context::Context as LLVMContext;
use inkwell::targets::RelocMode;
use std::fmt::Write;
use std::path::{Path, PathBuf};
use aspect::codegen::CodeGenerator;
use aspect::preprocessor::{PreprocessedSource, Preprocessor};
use aspect::parser::{FunctionBody, Parser, Program};
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

/// LLVM relocation model, mirroring `llc`'s `-relocation-model`.
#[derive(Copy, Clone, Debug, Eq, PartialEq, ValueEnum)]
enum RelocModel {
    Default,
    Static,
    Pic,
    DynamicNoPic,
}

impl RelocModel {
    fn to_llvm(self) -> RelocMode {
        match self {
            RelocModel::Default => RelocMode::Default,
            RelocModel::Static => RelocMode::Static,
            RelocModel::Pic => RelocMode::PIC,
            RelocModel::DynamicNoPic => RelocMode::DynamicNoPic,
        }
    }
}

/// Bundled so the driver functions threading them stay under a sane argument
/// count. `interpret` leaves `reloc`/`verify_each` at defaults; only `compile`
/// exposes them on the CLI.
struct BackendOpts {
    opt_level: u8,
    reloc: RelocMode,
    verify_each: bool,
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

        /// Relocation model: `pic` emits position-independent code for shared
        /// libraries / PIE; `static` forces absolute addressing; `default`
        /// lets LLVM choose for the target triple
        #[arg(
            short = 'r',
            long = "relocation-model",
            value_enum,
            default_value_t = RelocModel::Default
        )]
        relocation_model: RelocModel,

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
    },
    /// Generate a shell completion script to stdout
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum, value_name = "SHELL")]
        shell: clap_complete::Shell,
    },
}

/// Splices `ASPC_<MODE>_FLAGS` (`<MODE>` = the upper-cased subcommand) in right
/// after the subcommand token, so project-wide flags like `-I lib` need not be
/// retyped. Injected flags precede the user's own, so a single-valued flag on
/// the command line still wins (clap keeps the last) while `-I`/`-D` accumulate.
/// An invalid shell value is warned on stderr and ignored, not fatal.
fn args_with_env_flags() -> Vec<String> {
    inject_env_flags(std::env::args().collect(), |var| std::env::var(var).ok())
}

/// The environment-free core of [`args_with_env_flags`]: `lookup` stands in for
/// `std::env::var` so the rules are testable without mutating the process env.
fn inject_env_flags(mut args: Vec<String>, lookup: impl Fn(&str) -> Option<String>) -> Vec<String> {
    // The subcommand is the first argument (after argv[0]) that names one; the
    // top-level command takes no options, so nothing legitimately precedes it.
    let subcommands: Vec<String> = Cli::command()
        .get_subcommands()
        .map(|c| c.get_name().to_string())
        .collect();
    let Some(sub_idx) = args
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, a)| subcommands.iter().any(|s| s == *a))
        .map(|(i, _)| i)
    else {
        return args;
    };

    let var = format!("ASPC_{}_FLAGS", args[sub_idx].to_ascii_uppercase());
    let Some(value) = lookup(&var) else {
        return args;
    };
    if value.trim().is_empty() {
        return args;
    }

    match shlex::split(&value) {
        Some(extra) => {
            // Splice the tokens in directly after the subcommand so they precede
            // the user's own arguments for it.
            let tail = args.split_off(sub_idx + 1);
            args.extend(extra);
            args.extend(tail);
        }
        None => {
            eprintln!("aspc: warning: {var} is not valid shell syntax and was ignored");
        }
    }
    args
}

fn main() -> Result<()> {
    let cli = Cli::parse_from(args_with_env_flags());

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
            relocation_model,
            verify_each,
        } => compile_file(
            &file,
            &preproc,
            emit,
            output.as_deref(),
            print,
            &BackendOpts {
                opt_level,
                reloc: relocation_model.to_llvm(),
                verify_each,
            },
        )?,
        Commands::Interpret {
            file,
            preproc,
            opt_level,
            program_args,
        } => {
            interpret_file(&file, &preproc, opt_level, &program_args)?;
        }
        Commands::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "aspc", &mut std::io::stdout());
        }
    }

    Ok(())
}

/// Seeds `-D`/`-I` from the CLI, then tokenizes with directive expansion.
/// Errors are formatted with their originating file/position.
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

/// Preprocess then parse `path` into a `Program`, formatting parse errors with
/// their originating file/position.
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

    let mut typechecker = TypeChecker::new().with_target(preproc.target_spec());
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

    // Non-fatal diagnostics: print to stderr, do not affect the exit code.
    for warning in typechecker.warnings() {
        eprintln!("{}", typechecker.format_warning(warning));
    }

    // Governance rules (Phase 2a): judge the typed program. Error judgments
    // fail the build; reports go to stderr like warnings.
    let mut rule_errors = String::new();
    for judgment in aspect::meta::run_rules(&program) {
        let line = aspect::meta::format_judgment(&judgment, &program.source_files);
        match judgment.severity {
            aspect::meta::Severity::Error => {
                let _ = writeln!(rule_errors, "{line}");
            }
            aspect::meta::Severity::Report => eprintln!("{line}"),
        }
    }
    if !rule_errors.is_empty() {
        anyhow::bail!(
            "Rule checking failed for '{}':\n{}",
            path.display(),
            rule_errors.trim_end()
        );
    }

    Ok(program)
}

/// Shared by `compile` and `interpret`: generate LLVM IR (module named after
/// the file stem) and run optimization passes when `opt_level > 0`.
fn build_codegen<'ctx>(
    context: &'ctx LLVMContext,
    path: &Path,
    preproc: &PreprocArgs,
    program: &Program,
    opts: &BackendOpts,
) -> Result<CodeGenerator<'ctx>> {
    let module_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("module");

    let mut codegen =
        CodeGenerator::new_with_reloc(context, module_name, &preproc.target_spec(), opts.reloc)
            .with_context(|| format!("failed to set up code generation for '{}'", path.display()))?;
    codegen
        .generate(program)
        .with_context(|| format!("failed to generate code for '{}'", path.display()))?;
    codegen
        .optimize(opts.opt_level, opts.verify_each)
        .with_context(|| format!("failed to optimize code for '{}'", path.display()))?;

    Ok(codegen)
}

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

        match &func.body {
            FunctionBody::Extern => println!("    [extern]"),
            FunctionBody::Asm(asm) => {
                println!("    [asm]");
                for line in &asm.lines {
                    println!("      {line}");
                }
            }
            FunctionBody::Naked(naked) => {
                println!("    [naked]");
                for line in &naked.lines {
                    println!("      {line}");
                }
            }
            FunctionBody::Aspect(stmts) => {
                println!("    body: {} statements", stmts.len());
                if !stmts.is_empty() {
                    println!("    statements:");
                    for (i, stmt) in stmts.iter().enumerate() {
                        println!("      [{i}]: {stmt:#?}");
                    }
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
    opts: &BackendOpts,
) -> Result<()> {
    let program = build_program(path, preproc)?;
    let context = LLVMContext::create();
    let codegen = build_codegen(&context, path, preproc, &program, opts)?;

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
    // The JIT resolves symbols in-process, so the relocation model is moot;
    // keep LLVM's default.
    let opts = BackendOpts {
        opt_level,
        reloc: RelocMode::Default,
        verify_each: false,
    };
    let codegen = build_codegen(&context, path, preproc, &program, &opts)?;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(ToString::to_string).collect()
    }

    /// A `lookup` that answers a single variable and nothing else.
    fn one(name: &'static str, value: &'static str) -> impl Fn(&str) -> Option<String> {
        move |var: &str| (var == name).then(|| value.to_string())
    }

    #[test]
    fn splices_mode_flags_in_after_the_subcommand() {
        let out = inject_env_flags(
            argv(&["aspc", "compile", "foo.ap"]),
            one("ASPC_COMPILE_FLAGS", "-I lib"),
        );
        assert_eq!(out, argv(&["aspc", "compile", "-I", "lib", "foo.ap"]));
    }

    #[test]
    fn injected_flags_precede_user_flags_so_the_command_line_wins() {
        // Both set `-O`; the user's explicit `-O3` comes after the injected
        // `-O1`, and clap keeps the last occurrence — so the CLI overrides env.
        let out = inject_env_flags(
            argv(&["aspc", "compile", "-O3", "foo.ap"]),
            one("ASPC_COMPILE_FLAGS", "-O1"),
        );
        assert_eq!(out, argv(&["aspc", "compile", "-O1", "-O3", "foo.ap"]));
    }

    #[test]
    fn honours_shell_quoting_in_the_value() {
        let out = inject_env_flags(
            argv(&["aspc", "compile", "foo.ap"]),
            one("ASPC_COMPILE_FLAGS", "-I \"dir with spaces\""),
        );
        assert_eq!(
            out,
            argv(&["aspc", "compile", "-I", "dir with spaces", "foo.ap"])
        );
    }

    #[test]
    fn keys_the_variable_off_the_actual_subcommand() {
        // `ASPC_COMPILE_FLAGS` must not leak into an `interpret` run.
        let out = inject_env_flags(
            argv(&["aspc", "interpret", "foo.ap"]),
            one("ASPC_COMPILE_FLAGS", "-I lib"),
        );
        assert_eq!(out, argv(&["aspc", "interpret", "foo.ap"]));

        let out = inject_env_flags(
            argv(&["aspc", "interpret", "foo.ap"]),
            one("ASPC_INTERPRET_FLAGS", "-O2"),
        );
        assert_eq!(out, argv(&["aspc", "interpret", "-O2", "foo.ap"]));
    }

    #[test]
    fn does_nothing_without_a_subcommand() {
        // A top-level `--help` names no mode, so no variable can apply — even
        // one that happens to be set.
        let out = inject_env_flags(
            argv(&["aspc", "--help"]),
            |_: &str| Some("-I lib".to_string()),
        );
        assert_eq!(out, argv(&["aspc", "--help"]));
    }

    #[test]
    fn an_unset_or_blank_value_changes_nothing() {
        let unchanged = argv(&["aspc", "compile", "foo.ap"]);
        assert_eq!(
            inject_env_flags(unchanged.clone(), |_: &str| None),
            unchanged
        );
        assert_eq!(
            inject_env_flags(unchanged.clone(), one("ASPC_COMPILE_FLAGS", "   ")),
            unchanged
        );
    }

    #[test]
    fn a_malformed_value_is_ignored_not_fatal() {
        // Unbalanced quote: shlex refuses to split it, so the args are left as
        // they were rather than aborting the build.
        let unchanged = argv(&["aspc", "compile", "foo.ap"]);
        assert_eq!(
            inject_env_flags(unchanged.clone(), one("ASPC_COMPILE_FLAGS", "-I \"oops")),
            unchanged
        );
    }

    #[test]
    fn a_file_named_like_a_subcommand_does_not_confuse_detection() {
        // The subcommand is the *first* token that names one; a later argument
        // that happens to be `parse` is an operand, not a second mode.
        let out = inject_env_flags(
            argv(&["aspc", "compile", "parse"]),
            one("ASPC_COMPILE_FLAGS", "-O2"),
        );
        assert_eq!(out, argv(&["aspc", "compile", "-O2", "parse"]));
    }
}
