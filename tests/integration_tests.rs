use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::NamedTempFile;

use inkwell::context::Context;
use tjlb_macros::generate_tests;
use tjlb_rust::codegen::CodeGenerator;
use tjlb_rust::lexer::tokenize;
use tjlb_rust::parser::Parser;
use tjlb_rust::typechecker::TypeChecker;

fn compile_to_ir_tempfile(source_path: &str) -> Result<NamedTempFile, String> {
    // Read source file
    let source =
        fs::read_to_string(source_path).map_err(|e| format!("Failed to read source file: {e}"))?;

    // Tokenize
    let tokens = tokenize(source).map_err(|e| format!("Tokenization failed: {e}"))?;

    // Parse
    let mut parser = Parser::new(tokens).with_source_file(source_path.to_string());
    let parse_result = parser.parse_program();
    let program = parse_result.map_err(|errors| {
        errors
            .iter()
            .map(|e| parser.format_error(e))
            .collect::<Vec<_>>()
            .join("\n")
    })?;

    // Typecheck
    let mut typechecker = TypeChecker::new().with_source_file(source_path.to_string());
    typechecker.check_program(&program).map_err(|errors| {
        errors
            .iter()
            .map(|e| typechecker.format_error(e))
            .collect::<Vec<_>>()
            .join("\n")
    })?;

    // Generate LLVM IR
    let context = Context::create();
    let module_name = Path::new(source_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("module");

    let mut codegen = CodeGenerator::new(&context, module_name);
    codegen
        .generate(&program)
        .map_err(|e| format!("Code generation failed: {e}"))?;

    // Write IR to temporary file
    let ir_file = NamedTempFile::new().map_err(|e| format!("Failed to create temp file: {e}"))?;

    codegen
        .write_ir_to_file(ir_file.path())
        .map_err(|e| format!("Failed to write IR: {e}"))?;

    Ok(ir_file)
}

fn compile_only(source_path: &str) -> Result<(), String> {
    compile_to_ir_tempfile(source_path).map(|_| ())
}

fn assert_compile_error_contains(source_path: &str, expected_fragments: &[&str]) {
    let err = compile_only(source_path).expect_err(&format!(
        "Expected compilation to fail for {source_path}, but it succeeded"
    ));

    let err_lower = err.to_lowercase();
    for fragment in expected_fragments {
        let fragment_lower = fragment.to_lowercase();
        assert!(
            err_lower.contains(&fragment_lower),
            "Expected error for {source_path} to contain '{fragment}', got: {err}"
        );
    }
}

/// Helper function to compile a TJLB program and run it with lli-19
/// Here's what it does in detail:
/// 1. Reads the source file
/// 2. Tokenizes the source code
/// 3. Parses the tokens into an AST
/// 4. Generates LLVM IR from the AST
/// 5. Writes the LLVM IR to a temporary file
/// 6. Executes the IR with lli-19
/// 7. Captures and returns the exit code of the program
fn compile_and_run_with_args(source_path: &str, args: &[String]) -> Result<i32, String> {
    let ir_file = compile_to_ir_tempfile(source_path)?;

    // Run with lli-19
    let output = Command::new("lli-19")
        .arg(ir_file.path())
        .args(args)
        .output()
        .map_err(|e| format!("Failed to execute lli-19: {e}"))?;

    // Get exit code (note: we use non-zero exit codes as return values, so don't check for success)
    let exit_code = output.status.code().ok_or_else(|| {
        format!(
            "lli-19 terminated by signal:\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })?;

    // If there was stderr output, it might indicate a problem
    if !output.stderr.is_empty() {
        eprintln!("lli-19 stderr: {}", String::from_utf8_lossy(&output.stderr));
    }

    Ok(exit_code)
}

fn compile_and_run(source_path: &str) -> Result<i32, String> {
    compile_and_run_with_args(source_path, &[])
}

generate_tests!();
