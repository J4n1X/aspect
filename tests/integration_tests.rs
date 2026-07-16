use std::path::Path;

use inkwell::context::Context;
use aspect_macros::generate_tests;
use aspect::codegen::CodeGenerator;
use aspect::preprocessor::{PreprocessedSource, Preprocessor};
use aspect::parser::{Parser, Program};
use aspect::typechecker::TypeChecker;

/// Preprocess a source file with extra compiler flags from a
/// `# compile_args:` annotation. Supported flags: `-D NAME[=VALUE]` (seeds
/// the define table before the entry file) and `-I DIR` (module search
/// root). Flags and their values are separate annotation strings, mirroring
/// the CLI: `# compile_args: "-D", "DEBUG=1"`.
fn preprocess_with_args(
    source_path: &str,
    compile_args: &[String],
) -> Result<PreprocessedSource, String> {
    let mut pp = Preprocessor::new();
    let mut args = compile_args.iter();
    while let Some(flag) = args.next() {
        let value = args
            .next()
            .ok_or_else(|| format!("compile_args flag '{flag}' is missing its value"))?;
        match flag.as_str() {
            "-D" => pp
                .add_cli_define(value)
                .map_err(|e| format!("Tokenization failed: {}", pp.format_error(&e)))?,
            "-I" => pp.add_include_dir(value),
            other => return Err(format!("unsupported compile_args flag '{other}'")),
        }
    }
    pp.preprocess(Path::new(source_path))
        .map_err(|e| format!("Tokenization failed: {}", pp.format_error(&e)))
}

/// Read, tokenize, parse, and type-check the source file. Returns the typed
/// AST ready for codegen, or a stage-tagged error string.
fn parse_and_typecheck(source_path: &str, compile_args: &[String]) -> Result<Program, String> {
    let pp = preprocess_with_args(source_path, compile_args)?;

    let mut parser = Parser::new(pp.tokens)
        .with_source_files(pp.files)
        .with_module_info(pp.modules, pp.imports);
    let mut program = parser.parse_program().map_err(|errors| {
        errors
            .iter()
            .map(|e| parser.format_error(e))
            .collect::<Vec<_>>()
            .join("\n")
    })?;

    let mut typechecker = TypeChecker::new();
    typechecker.check_program(&mut program).map_err(|errors| {
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
fn compile_only(source_path: &str, compile_args: &[String]) -> Result<(), String> {
    let program = parse_and_typecheck(source_path, compile_args)?;
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, module_name_for(source_path));
    codegen
        .generate(&program)
        .map_err(|e| format!("Code generation failed: {e}"))?;
    Ok(())
}

fn assert_compile_error_contains(
    source_path: &str,
    expected_fragments: &[&str],
    compile_args: &[String],
) {
    let err = compile_only(source_path, compile_args).expect_err(&format!(
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

/// Compile and JIT-execute a Aspect program in-process, returning `main`'s
/// `i32` return value. `args` is the program's argv tail; the source path is
/// prepended as the conventional `argv[0]`. `compile_args` holds
/// `# compile_args:` flags for the preprocessor (`-D`/`-I`).
fn compile_and_run_with_args(
    source_path: &str,
    args: &[String],
    compile_args: &[String],
) -> Result<i32, String> {
    let program = parse_and_typecheck(source_path, compile_args)?;
    let context = Context::create();
    let mut codegen = CodeGenerator::new(&context, module_name_for(source_path));
    codegen
        .generate(&program)
        .map_err(|e| format!("Code generation failed: {e}"))?;

    // Run the optimizer over it to catch any codegen issues that would cause optimization to fail
    codegen.optimize(2, true).map_err(|e| format!("Optimization failed: {e}"))?;
    
    let mut argv: Vec<&str> = Vec::with_capacity(args.len() + 1);
    argv.push(source_path);
    argv.extend(args.iter().map(String::as_str));

    codegen
        .jit_execute_main(&argv, 0)
        .map_err(|e| format!("JIT execution failed: {e}"))
}

fn compile_and_run(source_path: &str) -> Result<i32, String> {
    compile_and_run_with_args(source_path, &[], &[])
}

generate_tests!();
