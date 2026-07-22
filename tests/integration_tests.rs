use std::path::Path;

use inkwell::context::Context;
use aspect_macros::generate_tests;
use aspect::codegen::CodeGenerator;
use aspect::preprocessor::{PreprocessedSource, Preprocessor};
use aspect::parser::{Parser, Program};
use aspect::target::TargetSpec;
use aspect::typechecker::TypeChecker;

/// Applies `# compile_args:` flags (`-D NAME[=VALUE]`, `-I DIR`). Flag and
/// value are separate annotation strings, mirroring the CLI.
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
/// AST plus any non-fatal type-checker warnings (formatted), or a stage-tagged
/// error string.
fn parse_and_typecheck(
    source_path: &str,
    compile_args: &[String],
) -> Result<(Program, Vec<String>), String> {
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

    // Elaborate to a fixpoint, mirroring `build_program` so the corpus exercises
    // the same path production does.
    let elaboration = aspect::typechecker::elaborate_program(
        &mut program,
        TargetSpec::host(),
        aspect::typechecker::DEFAULT_MAX_ROUNDS,
    );
    let typechecker = elaboration.checker;
    elaboration.result.map_err(|errors| {
        errors
            .iter()
            .map(|e| typechecker.format_error(e))
            .collect::<Vec<_>>()
            .join("\n")
    })?;

    // Governance rules (Phase 2a): fold Error judgments into the error string
    // (failure fixtures assert on them); Report judgments join the warnings so
    // `# expected_warning:` can assert on checker-only rules.
    let mut rule_errors: Vec<String> = Vec::new();
    let mut rule_reports: Vec<String> = Vec::new();
    for judgment in aspect::meta::run_rules(&program) {
        let line = aspect::meta::format_judgment(&judgment, &program.source_files);
        match judgment.severity {
            aspect::meta::Severity::Error => rule_errors.push(line),
            aspect::meta::Severity::Report => rule_reports.push(line),
        }
    }
    if !rule_errors.is_empty() {
        return Err(rule_errors.join("\n"));
    }

    let mut warnings: Vec<String> = typechecker
        .warnings()
        .iter()
        .map(|w| typechecker.format_warning(w))
        .collect();
    warnings.extend(rule_reports);
    Ok((program, warnings))
}

/// Assert that type-checking `source_path` emits at least one warning whose
/// text contains `fragment` (case-insensitive). Drives `# expected_warning:`.
fn assert_warning_contains(source_path: &str, fragment: &str, compile_args: &[String]) {
    let (_program, warnings) = parse_and_typecheck(source_path, compile_args)
        .expect("expected the program to type-check so its warnings could be inspected");
    let needle = fragment.to_lowercase();
    assert!(
        warnings.iter().any(|w| w.to_lowercase().contains(&needle)),
        "Expected a warning containing '{fragment}' for {source_path}, got: {warnings:?}"
    );
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
    let (program, _warnings) = parse_and_typecheck(source_path, compile_args)?;
    let context = Context::create();
    let mut codegen =
        CodeGenerator::new(&context, module_name_for(source_path), &TargetSpec::host())
            .map_err(|e| format!("Code generator setup failed: {e}"))?;
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

/// Compile and JIT-execute a Aspect program in-process at `opt_level`,
/// returning `main`'s `i32` return value. `args` is the program's argv tail;
/// the source path is prepended as the conventional `argv[0]`.
fn run_at_opt(
    source_path: &str,
    args: &[String],
    compile_args: &[String],
    opt_level: u8,
) -> Result<i32, String> {
    let (program, _warnings) = parse_and_typecheck(source_path, compile_args)?;
    let context = Context::create();
    let mut codegen =
        CodeGenerator::new(&context, module_name_for(source_path), &TargetSpec::host())
            .map_err(|e| format!("Code generator setup failed: {e}"))?;
    codegen
        .generate(&program)
        .map_err(|e| format!("Code generation failed at -O{opt_level}: {e}"))?;

    // Run the optimizer to catch codegen issues that only surface there.
    if opt_level > 0 {
        codegen
            .optimize(opt_level, true)
            .map_err(|e| format!("Optimization failed at -O{opt_level}: {e}"))?;
    }

    let mut argv: Vec<&str> = Vec::with_capacity(args.len() + 1);
    argv.push(source_path);
    argv.extend(args.iter().map(String::as_str));

    codegen
        .jit_execute_main(&argv, 0)
        .map_err(|e| format!("JIT execution failed at -O{opt_level}: {e}"))
}

/// Runs every program at **both** `-O0` and `-O2`, which must agree. The two
/// take materially different paths — an `asm fn` stays a real call at `-O0` but
/// is folded by `alwaysinline` at `-O1+` — so running only one level leaves
/// either the unoptimised lowering or the optimizer untested. A disagreement is
/// itself the finding, reported rather than resolved.
fn compile_and_run_with_args(
    source_path: &str,
    args: &[String],
    compile_args: &[String],
) -> Result<i32, String> {
    let unoptimized = run_at_opt(source_path, args, compile_args, 0)?;
    let optimized = run_at_opt(source_path, args, compile_args, 2)?;

    if unoptimized != optimized {
        return Err(format!(
            "{source_path} is optimization-level dependent: -O0 returned {unoptimized}, \
             -O2 returned {optimized}. One of the two lowerings is wrong."
        ));
    }
    Ok(optimized)
}

fn compile_and_run(source_path: &str) -> Result<i32, String> {
    compile_and_run_with_args(source_path, &[], &[])
}

/// Re-checking an already type-checked `Program` with a fresh checker must
/// produce an identical program — the elaboration driver relies on this to
/// re-check to a fixpoint. Guards against non-idempotent mutation (stale literal
/// narrowing, double lowering, symbol-table drift).
#[test]
fn typecheck_is_idempotent_on_recheck() {
    // A feature-diverse set, including the `MethodCall` lowering and a stdlib
    // import.
    let cases: &[(&str, &[&str])] = &[
        ("tests/programs/methods.ap", &[]),
        ("tests/programs/method_chain.ap", &[]),
        ("tests/programs/value_block.ap", &[]),
        ("tests/programs/enum_basic.ap", &[]),
        ("tests/programs/fnptr_vtable.ap", &[]),
        ("tests/programs/encapsulation.ap", &[]),
        ("tests/programs/attributes_inert.ap", &[]),
        ("tests/programs/stdlib_check.ap", &["-I", "lib"]),
    ];
    for (path, args) in cases {
        let args: Vec<String> = args.iter().map(|s| (*s).to_string()).collect();
        let (checked, _warnings) = parse_and_typecheck(path, &args)
            .unwrap_or_else(|e| panic!("{path}: first typecheck failed: {e}"));

        // Re-check a clone with a fresh checker; it must be unchanged.
        let mut rechecked = checked.clone();
        TypeChecker::new()
            .check_program(&mut rechecked)
            .unwrap_or_else(|errs| panic!("{path}: re-check errored: {errs:?}"));
        assert_eq!(
            checked, rechecked,
            "{path}: re-checking a checked program changed it — the checker is not idempotent"
        );
    }
}

generate_tests!();
