use std::fs;
use std::path::Path;

use inkwell::context::Context;
use tjlb_macros::generate_tests;
use tjlb_rust::codegen::CodeGenerator;
use tjlb_rust::lexer::tokenize;
use tjlb_rust::parser::{Parser, Program};
use tjlb_rust::typechecker::TypeChecker;

/// Read, tokenize, parse, and type-check the source file. Returns the typed
/// AST ready for codegen, or a stage-tagged error string.
fn parse_and_typecheck(source_path: &str) -> Result<Program, String> {
    let source =
        fs::read_to_string(source_path).map_err(|e| format!("Failed to read source file: {e}"))?;

    let tokens = tokenize(source).map_err(|e| format!("Tokenization failed: {e}"))?;

    let mut parser = Parser::new(tokens).with_source_file(source_path.to_string());
    let program = parser.parse_program().map_err(|errors| {
        errors
            .iter()
            .map(|e| parser.format_error(e))
            .collect::<Vec<_>>()
            .join("\n")
    })?;

    let mut typechecker = TypeChecker::new().with_source_file(source_path.to_string());
    typechecker.check_program(&program).map_err(|errors| {
        errors
            .iter()
            .map(|e| typechecker.format_error(e))
            .collect::<Vec<_>>()
            .join("\n")
    })?;

    Ok(program)
}

fn module_name_for(source_path: &str) -> &str {
    Path::new(source_path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("module")
}

/// Compile through codegen without executing — used by failure tests to assert
/// that compilation reports the expected error.
fn compile_only(source_path: &str) -> Result<(), String> {
    let program = parse_and_typecheck(source_path)?;
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, module_name_for(source_path));
    codegen
        .generate(&program)
        .map_err(|e| format!("Code generation failed: {e}"))?;
    Ok(())
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

/// Compile and JIT-execute a TJLB program in-process, returning `main`'s
/// `i32` return value. `args` is the program's argv tail; the source path is
/// prepended as the conventional `argv[0]`.
fn compile_and_run_with_args(source_path: &str, args: &[String]) -> Result<i32, String> {
    let program = parse_and_typecheck(source_path)?;
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, module_name_for(source_path));
    codegen
        .generate(&program)
        .map_err(|e| format!("Code generation failed: {e}"))?;

    // Run the optimizer over it to catch any codegen issues that would cause optimization to fail
    codegen.optimize(2, true);
    
    let mut argv: Vec<&str> = Vec::with_capacity(args.len() + 1);
    argv.push(source_path);
    argv.extend(args.iter().map(String::as_str));

    codegen
        .jit_execute_main(&argv, 0)
        .map_err(|e| format!("JIT execution failed: {e}"))
}

fn compile_and_run(source_path: &str) -> Result<i32, String> {
    compile_and_run_with_args(source_path, &[])
}

generate_tests!();
