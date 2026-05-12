use anyhow::{Context, Result};
use clap::{Parser as ClapParser, Subcommand};
use inkwell::context::Context as LLVMContext;
use tjlb_rust::typechecker::TypeChecker;
use std::fmt::Write;
use std::fs;
use std::path::PathBuf;
use tjlb_rust::codegen::CodeGenerator;
use tjlb_rust::lexer::tokenize;
use tjlb_rust::parser::Parser;

#[derive(ClapParser)]
#[command(name = "tjlb-parser")]
#[command(about = "Parser for the TJLB programming language", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Tokenize the input file and print tokens
    Lex {
        /// Input file path
        #[arg(value_name = "FILE")]
        file: PathBuf,
    },
    /// Parse the input file and print the AST
    Parse {
        /// Input file path
        #[arg(value_name = "FILE")]
        file: PathBuf,
    },
    /// Compile the input file to LLVM IR
    Compile {
        /// Input file path
        #[arg(value_name = "FILE")]
        file: PathBuf,

        /// Output file path (defaults to stdout)
        #[arg(short, long, value_name = "OUTPUT")]
        output: Option<PathBuf>,

        /// Print IR to stdout even when writing to file
        #[arg(short, long)]
        print: bool,

        /// Optimization level (0-3)
        #[arg(short = 'O', long = "optimize", value_name = "LEVEL", default_value = "0")]
        opt_level: u8,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Lex { file } => lex_file(&file)?,
        Commands::Parse { file } => parse_file(&file)?,
        Commands::Compile {
            file,
            output,
            print,
            opt_level,
        } => compile_file(&file, output.as_deref(), print, opt_level)?,
    }

    Ok(())
}

fn lex_file(path: &PathBuf) -> Result<()> {
    let input = fs::read_to_string(path)
        .with_context(|| format!("failed to read file '{}'", path.display()))?;

    let tokens = tokenize(input)
        .with_context(|| format!("failed to tokenize '{}'", path.display()))?;

    println!("Tokens:");
    println!("-------");
    for token in &tokens {
        println!(
            "{}:{}:{} {:?} {}",
            path.display(),
            token.pos.line,
            token.pos.column,
            token.kind,
            token.lexeme
        );
    }

    println!("\nTotal tokens: {}", tokens.len());

    Ok(())
}

fn parse_file(path: &PathBuf) -> Result<()> {
    let input = fs::read_to_string(path)
        .with_context(|| format!("failed to read file '{}'", path.display()))?;

    // Tokenize
    let tokens = tokenize(input)
        .with_context(|| format!("failed to tokenize '{}'", path.display()))?;

    // Parse
    let mut parser = Parser::new(tokens).with_source_file(path.display().to_string());
    let parse_result = parser.parse_program();
    let program = parse_result.map_err(|errors| {
        let msgs: Vec<String> = errors.iter().map(|e| parser.format_error(e)).collect();
        anyhow::anyhow!("{}", msgs.join("\n"))
    })?;

    // Print AST
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

fn compile_file(path: &PathBuf, output: Option<&std::path::Path>, print: bool, opt_level: u8) -> Result<()> {
    let input = fs::read_to_string(path)
        .with_context(|| format!("failed to read file '{}'", path.display()))?;

    // Tokenize
    let tokens = tokenize(input)
        .with_context(|| format!("failed to tokenize '{}'", path.display()))?;

    // Parse
    let mut parser = Parser::new(tokens).with_source_file(path.display().to_string());
    let parse_result = parser.parse_program();
    let program = parse_result.map_err(|errors| {
        let msgs: Vec<String> = errors.iter().map(|e| parser.format_error(e)).collect();
        anyhow::anyhow!("{}", msgs.join("\n"))
    })?;

    let mut typechecker = TypeChecker::new().with_source_file(path.display().to_string());
    typechecker.check_program(&program)
        .map_err(|errors| {
            let mut err_msg = String::new();
            for error in &errors {
                let _ = writeln!(err_msg, "{}", typechecker.format_error(error));
            }
            anyhow::anyhow!("Type checking failed for '{}':\n{}", path.display(), err_msg.trim_end())
        })?;

    // Generate LLVM IR
    let context = LLVMContext::create();
    let module_name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("module");

    let mut codegen = CodeGenerator::new(&context, module_name);
    codegen.generate(&program)
        .with_context(|| format!("failed to generate code for '{}'", path.display()))?;

    // Run optimization passes
    if opt_level > 0 {
        codegen.optimize(opt_level)
            .with_context(|| format!("failed to optimize code for '{}'", path.display()))?;
    }

    // Output
    let ir = codegen.print_ir_to_string();

    if let Some(output_path) = output {
        codegen.write_ir_to_file(output_path)
            .with_context(|| format!("failed to write IR to '{}'", output_path.display()))?;
        if print {
            println!("{ir}");
        } else {
            println!("LLVM IR written to: {}", output_path.display());
        }
    } else {
        println!("{ir}");
    }

    Ok(())
}
