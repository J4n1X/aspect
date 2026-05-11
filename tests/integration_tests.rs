use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::NamedTempFile;

use tjlb_rust::lexer::tokenize;
use tjlb_rust::parser::Parser;
use tjlb_rust::codegen::CodeGenerator;
use inkwell::context::Context;

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
    // Read source file
    let source = fs::read_to_string(source_path)
        .map_err(|e| format!("Failed to read source file: {e}"))?;

    // Tokenize
    let tokens = tokenize(source)
        .map_err(|e| format!("Tokenization failed: {e}"))?;

    // Parse
    let mut parser = Parser::new(tokens).with_source_file(source_path.to_string());
    let parse_result = parser.parse_program();
    let program = parse_result.map_err(|errors| {
        errors.iter().map(|e| parser.format_error(e)).collect::<Vec<_>>().join("\n")
    })?;

    // Generate LLVM IR
    let context = Context::create();
    let module_name = Path::new(source_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("module");

    let mut codegen = CodeGenerator::new(&context, module_name);
    codegen.generate(&program)
        .map_err(|e| format!("Code generation failed: {e}"))?;

    // Write IR to temporary file
    let ir_file = NamedTempFile::new()
        .map_err(|e| format!("Failed to create temp file: {e}"))?;

    codegen.write_ir_to_file(ir_file.path())
        .map_err(|e| format!("Failed to write IR: {e}"))?;

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

#[test]
fn test_return_42() {
    let result = compile_and_run("tests/programs/return_42.tjlb")
        .expect("Failed to compile and run return_42.tjlb");
    assert_eq!(result, 42, "Expected exit code 42, got {result}");
}

#[test]
fn test_arithmetic() {
    let result = compile_and_run("tests/programs/arithmetic.tjlb")
        .expect("Failed to compile and run arithmetic.tjlb");
    assert_eq!(result, 27, "Expected exit code 27, got {result}");
}

#[test]
fn test_pointer_arithmetic() {
    let result = compile_and_run("tests/programs/pointer_arithmetic.tjlb")
        .expect("Failed to compile and run pointer_arithmetic.tjlb");
    assert_eq!(result, 123, "Expected exit code 123, got {result}");
}

#[test]
fn test_fibonacci() {
    let result = compile_and_run("tests/programs/fibonacci.tjlb")
        .expect("Failed to compile and run fibonacci.tjlb");
    assert_eq!(result, 13, "Expected exit code 13 (fib(7)), got {result}");
}

#[test]
fn test_loops() {
    let result = compile_and_run("tests/programs/loops.tjlb")
        .expect("Failed to compile and run loops.tjlb");
    assert_eq!(result, 60, "Expected exit code 60, got {result}");
}

#[test]
fn test_conditionals() {
    let result = compile_and_run("tests/programs/conditionals.tjlb")
        .expect("Failed to compile and run conditionals.tjlb");
    assert_eq!(result, 50, "Expected exit code 50, got {result}");
}

#[test]
fn test_global_vars() {
    let result = compile_and_run("tests/programs/global_vars.tjlb")
        .expect("Failed to compile and run global_vars.tjlb");
    assert_eq!(result, 103, "Expected exit code 103, got {result}");
}

#[test]
fn test_pointers() {
    let result = compile_and_run("tests/programs/pointers.tjlb")
        .expect("Failed to compile and run pointers.tjlb");
    assert_eq!(result, 42, "Expected exit code 42, got {result}");
}

#[test]
fn test_bitwise() {
    let result = compile_and_run("tests/programs/bitwise.tjlb")
        .expect("Failed to compile and run bitwise.tjlb");
    assert_eq!(result, 28, "Expected exit code 28, got {result}");
}

#[test]
fn test_array_access() {
    let arg = String::from("array_access_test");
    let arg_len = i32::try_from(arg.len()).unwrap();
    let result = compile_and_run_with_args("tests/programs/array_access.tjlb", &[arg])
        .expect("Failed to compile and run brackets.tjlb");

    assert_eq!(result, arg_len, "Expected exit code {arg_len}, got {result}");
}

#[test]
fn test_break_continue() {
    let result = compile_and_run("tests/programs/break_continue.tjlb")
        .expect("Failed to compile and run break_continue.tjlb");
    assert_eq!(result, 22, "Expected exit code 22, got {result}");
}

#[test]
fn test_logical_ops() {
    let result = compile_and_run("tests/programs/logical_ops.tjlb")
        .expect("Failed to compile and run logical_ops.tjlb");
    assert_eq!(result, 121, "Expected exit code 121, got {result}");
}

#[test]
fn test_bitwise_not() {
    let result = compile_and_run("tests/programs/bitwise_not.tjlb")
        .expect("Failed to compile and run bitwise_not.tjlb");
    assert_eq!(result, 42, "Expected exit code 42, got {result}");
}

#[test]
fn test_variable_shadowing() {
    let result = compile_and_run("tests/programs/variable_shadowing.tjlb")
        .expect("Failed to compile and run variable_shadowing.tjlb");
    assert_eq!(result, 10, "Expected exit code 10, got {result}");
}
