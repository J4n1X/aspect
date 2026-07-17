    use super::*;
    use crate::lexer::{tokenize, Position};
    use crate::parser::{ExprKind, LiteralValue, Parser, Program, StatementKind, TypeBase};

    /// Lex, parse, and type-check `src`, returning the (mutated) AST and the result.
    fn check(src: &str) -> (Program, Result<(), Vec<TypeCheckError>>) {
        let tokens = tokenize(src.to_string()).expect("tokenization should succeed");
        let mut parser = Parser::new(tokens);
        let mut program = parser.parse_program().expect("parsing should succeed");
        let mut checker = TypeChecker::new();
        let result = checker.check_program(&mut program);
        (program, result)
    }

    /// Lex, parse, and type-check `src` against an explicit `--target`,
    /// mirroring what the driver does for `aspc --target <triple>`.
    ///
    /// The integration harness cannot reach these rules — it always compiles
    /// for the host, and its `# compile_args:` supports `-D`/`-I` only — so
    /// the target-arch rules are covered here instead.
    ///
    /// Note that a non-x86 target is *not* merely hypothetical for a binary
    /// built with only the x86 backend: `i686-*` is an x86 triple that LLVM
    /// accepts and happily emits a 32-bit module for, while having no `rax`.
    /// Nothing downstream would catch that, which is precisely why the check
    /// belongs to the type checker.
    fn check_for_target(src: &str, triple: &str) -> Result<(), Vec<TypeCheckError>> {
        let tokens = tokenize(src.to_string()).expect("tokenization should succeed");
        let mut parser = Parser::new(tokens);
        let mut program = parser.parse_program().expect("parsing should succeed");
        let mut checker = TypeChecker::new().with_target(TargetSpec::parse(triple));
        checker.check_program(&mut program)
    }

    const SYSCALL_ASM_FN: &str = r#"
asm fn add2(i64 a: rax, i64 b: rbx) -> i64: rax
{
    "add rax, rbx"
}
"#;

    #[test]
    fn asm_fn_is_rejected_for_a_non_x86_target() {
        let errors = check_for_target(SYSCALL_ASM_FN, "aarch64-unknown-linux-gnu")
            .expect_err("an asm fn must not compile for aarch64");
        assert!(
            matches!(
                errors.as_slice(),
                [TypeCheckError::AsmUnsupportedTarget { name, triple, .. }]
                    if name == "add2" && triple == "aarch64-unknown-linux-gnu"
            ),
            "expected a single AsmUnsupportedTarget error, got {errors:?}"
        );
    }

    #[test]
    fn asm_fn_is_accepted_for_an_x86_64_target() {
        assert!(check_for_target(SYSCALL_ASM_FN, "x86_64-unknown-linux-gnu").is_ok());
    }

    #[test]
    fn asm_fn_is_rejected_for_a_32_bit_x86_target() {
        // i686 is the case that fails *open* if the target never reaches the
        // checker: LLVM has the backend, accepts the triple, and emits a
        // 32-bit module in which `{rax}` does not exist — surfacing only as a
        // raw, positionless backend error. `arch_define()` returns None for
        // i686, so the checker must reject it like any other non-x86-64 arch.
        let errors = check_for_target(SYSCALL_ASM_FN, "i686-unknown-linux-gnu")
            .expect_err("an asm fn naming rax must not compile for 32-bit x86");
        assert!(
            matches!(
                errors.as_slice(),
                [TypeCheckError::AsmUnsupportedTarget { triple, .. }]
                    if triple == "i686-unknown-linux-gnu"
            ),
            "expected a single AsmUnsupportedTarget error, got {errors:?}"
        );
    }

    #[test]
    fn an_operand_wider_than_its_register_is_rejected() {
        // LLVM would silently widen `al` to the full `rax`, making the written
        // spelling a no-op.
        let (_, result) = check(
            r#"
asm fn lo(i64 v: al) -> i64: rax
{
    "mov rax, 0"
}
"#,
        );
        let errors = result.expect_err("an i64 must not fit in al");
        assert!(
            matches!(
                errors.as_slice(),
                [TypeCheckError::AsmRegisterTooNarrow { register, type_bits: 64, reg_bits: 8, .. }]
                    if register == "al"
            ),
            "expected a single AsmRegisterTooNarrow error, got {errors:?}"
        );
    }

    #[test]
    fn a_pointer_operand_wider_than_its_register_is_rejected() {
        // A pointer is 64-bit whatever it points at: `u8*` reports
        // `size_bits == 8` for its *pointee*, which must not be mistaken for
        // the operand's width.
        let (_, result) = check(
            r#"
asm fn p(u8* buf: sil) -> i64: rax
{
    "mov rax, 0"
}
"#,
        );
        let errors = result.expect_err("a pointer must not fit in sil");
        assert!(
            matches!(
                errors.as_slice(),
                [TypeCheckError::AsmRegisterTooNarrow { type_bits: 64, reg_bits: 8, .. }]
            ),
            "expected a single AsmRegisterTooNarrow error, got {errors:?}"
        );
    }

    #[test]
    fn an_operand_narrower_than_its_register_is_accepted() {
        // The orthogonality rule working as intended: the declared type drives
        // the conversion and LLVM selects `%eax` from the operand's type. This
        // must stay legal — it is the direction the width rule does not check.
        let (_, result) = check(
            r#"
asm fn narrow(i32 x: rax, u8* buf: rsi) -> i32: rax
{
    "nop"
}
"#,
        );
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn asm_fn_is_rejected_for_an_unrecognised_target() {
        // A triple whose arch we cannot classify at all must fail closed
        // rather than fall through to the x86 register table.
        let errors = check_for_target(SYSCALL_ASM_FN, "riscv64-unknown-linux-gnu")
            .expect_err("an asm fn must not compile for an unmodelled arch");
        assert!(matches!(
            errors.as_slice(),
            [TypeCheckError::AsmUnsupportedTarget { .. }]
        ));
    }

    #[test]
    fn asm_fn_rejects_an_unpinnable_operand_type() {
        let errors = check_for_target(
            r#"
asm fn f(bool a: rax) -> i64: rdx
{
    "nop"
}
"#,
            "x86_64-unknown-linux-gnu",
        )
        .expect_err("a bool cannot be pinned to any register");
        assert!(
            errors
                .iter()
                .any(|e| matches!(e, TypeCheckError::AsmUnpinnableType { .. })),
            "expected an AsmUnpinnableType error, got {errors:?}"
        );
    }

    #[test]
    fn asm_fn_rejects_an_operand_pinned_to_the_wrong_register_bank() {
        // A float is pinnable — but only to SSE. `rax` cannot hold one, and
        // LLVM diagnoses nothing if asked to try.
        for (src, what) in [
            ("asm fn f(f64 a: rax) -> f64: xmm0\n{\n    \"nop\"\n}\n", "f64 in a GPR"),
            ("asm fn f(i64 a: xmm0) -> i64: rax\n{\n    \"nop\"\n}\n", "i64 in an SSE reg"),
        ] {
            let errors = check_for_target(src, "x86_64-unknown-linux-gnu")
                .expect_err(&format!("{what} must be rejected"));
            assert!(
                errors
                    .iter()
                    .any(|e| matches!(e, TypeCheckError::AsmRegisterClassMismatch { .. })),
                "expected an AsmRegisterClassMismatch for {what}, got {errors:?}"
            );
        }
    }

    #[test]
    fn asm_fn_accepts_a_float_pinned_to_an_sse_register() {
        check_for_target(
            r#"
asm fn sqrt_asm(f64 x: xmm0) -> f64: xmm0
{
    "sqrtsd xmm0, xmm0"
}
"#,
            "x86_64-unknown-linux-gnu",
        )
        .expect("an f64 pinned to xmm0 is the whole point of SSE support");
    }

    #[test]
    fn asm_fn_accepts_an_output_sharing_a_register_with_an_input() {
        // The in-out syscall form: the duplicate rule applies only among
        // parameters, so `-> i64: rax` alongside `i64 nr: rax` is legal.
        assert!(
            check_for_target(
                r#"
asm fn syscall1(i64 nr: rax, i64 a1: rdi) -> i64: rax
    clobbers(rcx, r11, memory)
{
    "syscall"
}
"#,
                "x86_64-unknown-linux-gnu",
            )
            .is_ok()
        );
    }

    #[test]
    fn asm_fn_rejects_a_repeated_memory_clobber() {
        let errors = check_for_target(
            r#"
asm fn f(i64 a: rdi) -> i64: rax
    clobbers(memory, memory)
{
    "nop"
}
"#,
            "x86_64-unknown-linux-gnu",
        )
        .expect_err("a repeated memory clobber is a mistake, not a no-op");
        assert!(matches!(
            errors.as_slice(),
            [TypeCheckError::AsmDuplicateClobber { register, .. }] if register == "memory"
        ));
    }

    #[test]
    fn asm_fn_rejects_a_register_clobbered_under_two_spellings() {
        // Aliasing again: `rcx` and `ecx` are one register, so clobbering
        // both is a duplicate even though the spellings differ.
        let errors = check_for_target(
            r#"
asm fn f(i64 a: rdi) -> i64: rax
    clobbers(rcx, ecx)
{
    "nop"
}
"#,
            "x86_64-unknown-linux-gnu",
        )
        .expect_err("one register clobbered twice must be rejected");
        assert!(matches!(
            errors.as_slice(),
            [TypeCheckError::AsmDuplicateClobber { register, .. }] if register == "ecx"
        ));
    }

    #[test]
    fn asm_fn_accepts_a_single_memory_clobber_alongside_registers() {
        assert!(
            check_for_target(
                r#"
asm fn f(i64 a: rdi) -> i64: rax
    clobbers(rcx, r11, memory)
{
    "nop"
}
"#,
                "x86_64-unknown-linux-gnu",
            )
            .is_ok()
        );
    }

    /// Find a function by name.
    fn func<'a>(program: &'a Program, name: &str) -> &'a Function {
        program
            .functions
            .iter()
            .find(|f| f.proto.name == name)
            .unwrap_or_else(|| panic!("function `{name}` not found"))
    }

    /// Statements of an Aspect-bodied function.
    fn body<'a>(program: &'a Program, name: &str) -> &'a [Statement] {
        match &func(program, name).body {
            FunctionBody::Aspect(stmts) => stmts,
            _ => panic!("function `{name}` has no Aspect body"),
        }
    }

    /// Initializer expression of the `idx`-th `VarDecl` in function `fname`.
    fn nth_var_init<'a>(program: &'a Program, fname: &str, idx: usize) -> &'a Expression {
        let mut count = 0;
        for stmt in body(program, fname) {
            if let StatementKind::VarDecl {
                initializer: Some(init),
                ..
            } = &stmt.kind
            {
                if count == idx {
                    return init;
                }
                count += 1;
            }
        }
        panic!("var decl #{idx} not found in `{fname}`");
    }

    fn assert_ty(actual: LangType, base: TypeBase, bits: u32, ptr: u32) {
        assert_eq!(actual.base, base, "base type");
        assert_eq!(actual.size_bits, bits, "size_bits");
        assert_eq!(actual.pointer_depth, ptr, "pointer_depth");
    }

    fn has_type_mismatch(errs: &[TypeCheckError], at: Position) -> bool {
        errs.iter().any(|e| {
            matches!(e, TypeCheckError::TypeMismatch { position, .. } if *position == at)
        })
    }

    // 1. Literal fits target on assignment — stamped at the target type.
    #[test]
    fn literal_fits_target() {
        let (program, res) = check("fn main() -> i32 {\n    u8 x = 200\n    return 0\n}\n");
        assert!(res.is_ok(), "expected ok, got {res:?}");
        assert_ty(nth_var_init(&program, "main", 0).expr_type, TypeBase::UInt, 8, 0);
    }

    // 2. Literal overflows target — error at the literal's position.
    #[test]
    fn literal_overflows_target() {
        let (program, res) = check("fn main() -> i32 {\n    u8 x = 300\n    return 0\n}\n");
        let lit_pos = nth_var_init(&program, "main", 0).pos;
        let errs = res.expect_err("expected overflow error");
        assert!(has_type_mismatch(&errs, lit_pos), "error should sit on the literal: {errs:?}");
    }

    // 3. Binary propagates target — both literals and the `+` stamped u8.
    #[test]
    fn binary_propagates_target() {
        let (program, res) = check("fn main() -> i32 {\n    u8 x = 1 + 2\n    return 0\n}\n");
        assert!(res.is_ok(), "expected ok, got {res:?}");
        let init = nth_var_init(&program, "main", 0);
        assert_ty(init.expr_type, TypeBase::UInt, 8, 0);
        let ExprKind::Binary { left, right, .. } = &init.kind else {
            panic!("expected binary");
        };
        assert_ty(left.expr_type, TypeBase::UInt, 8, 0);
        assert_ty(right.expr_type, TypeBase::UInt, 8, 0);
    }

    // 4. Mixed literal and variable — the literal is stamped, result is u8.
    #[test]
    fn binary_mixed_literal_and_variable() {
        let src = "fn main() -> i32 {\n    u8 y = 0\n    u8 x = y + 1\n    return 0\n}\n";
        let (program, res) = check(src);
        assert!(res.is_ok(), "expected ok, got {res:?}");
        let init = nth_var_init(&program, "main", 1);
        assert_ty(init.expr_type, TypeBase::UInt, 8, 0);
        let ExprKind::Binary { right, .. } = &init.kind else {
            panic!("expected binary");
        };
        assert_ty(right.expr_type, TypeBase::UInt, 8, 0);
    }

    // 5. Comparison yields `bool` and coerces into an integer target; the
    //    target is never propagated into the operands.
    #[test]
    fn comparison_yields_bool() {
        let src = "fn main() -> i32 {\n    i32 a = 1\n    i32 b = 2\n    i32 c = a < b\n    return 0\n}\n";
        let (program, res) = check(src);
        assert!(res.is_ok(), "expected ok, got {res:?}");
        // The comparison node itself is `bool`; it coerces to the `i32` target.
        assert_ty(nth_var_init(&program, "main", 2).expr_type, TypeBase::Bool, 8, 0);
    }

    // 6. Function-call argument fit — error at the literal argument.
    #[test]
    fn call_argument_overflow() {
        let src = "fn f(u8 b) -> i32 {\n    return 0\n}\nfn main() -> i32 {\n    return f(300)\n}\n";
        let (_program, res) = check(src);
        let errs = res.expect_err("expected argument overflow error");
        assert!(
            errs.iter().any(|e| matches!(e, TypeCheckError::TypeMismatch { expected, .. }
                if expected.base == TypeBase::UInt && expected.size_bits == 8)),
            "expected u8 type mismatch on the argument: {errs:?}"
        );
    }

    // 7. Return propagates the function's return type into the literal.
    #[test]
    fn return_literal_fits() {
        let (_p, res) = check("fn f() -> u16 {\n    return 65535\n}\n");
        assert!(res.is_ok(), "expected ok, got {res:?}");
    }

    #[test]
    fn return_literal_overflows() {
        let (_p, res) = check("fn f() -> u16 {\n    return 65536\n}\n");
        assert!(res.is_err(), "expected overflow error");
    }

    // 8. Dereference takes the synth path; coercibility holds.
    #[test]
    fn dereference_synth_path() {
        let src = "fn f(u8* p) -> u8 {\n    u8 x = *p\n    return x\n}\n";
        let (_p, res) = check(src);
        assert!(res.is_ok(), "expected ok, got {res:?}");
    }

    // 9. Reference checks its inner against the pointee type.
    #[test]
    fn reference_propagates_pointee() {
        let src = "fn main() -> i32 {\n    u8 v = 5\n    u8* p = &v\n    return 0\n}\n";
        let (_p, res) = check(src);
        assert!(res.is_ok(), "expected ok, got {res:?}");
    }

    // 10. Cast forces its type; the inner literal is left at its synth default.
    #[test]
    fn cast_does_not_propagate() {
        let (program, res) = check("fn main() -> i32 {\n    u32 x = 300 as u32\n    return 0\n}\n");
        assert!(res.is_ok(), "expected ok, got {res:?}");
        let init = nth_var_init(&program, "main", 0);
        let ExprKind::Cast { expr: inner, .. } = &init.kind else {
            panic!("expected cast");
        };
        // The literal keeps its synthesised default (i32), not the cast target.
        assert!(matches!(inner.kind, ExprKind::Literal(LiteralValue::Integer(300))));
        assert_eq!(inner.expr_type.base, TypeBase::SInt);
    }

    // 11. List initialiser propagates the element type into every element.
    #[test]
    fn list_init_propagates_element_type() {
        let (program, res) = check("fn main() -> i32 {\n    u8[3] arr = {1, 2, 3}\n    return 0\n}\n");
        assert!(res.is_ok(), "expected ok, got {res:?}");
        let ExprKind::ListInitializer(elems) = &nth_var_init(&program, "main", 0).kind else {
            panic!("expected list initializer");
        };
        for elem in elems {
            assert_ty(elem.expr_type, TypeBase::UInt, 8, 0);
        }
    }

    // 12. List initialiser element overflow — error at the offending element.
    #[test]
    fn list_init_element_overflow() {
        let (program, res) = check("fn main() -> i32 {\n    u8[3] arr = {1, 2, 300}\n    return 0\n}\n");
        let ExprKind::ListInitializer(elems) = &nth_var_init(&program, "main", 0).kind else {
            panic!("expected list initializer");
        };
        let bad_pos = elems[2].pos;
        let errs = res.expect_err("expected element overflow error");
        assert!(has_type_mismatch(&errs, bad_pos), "error should sit on the `300` element: {errs:?}");
    }

    // 13. Field access stamps the declared field type onto the AST.
    #[test]
    fn struct_field_access_stamps_field_type() {
        let src = "type P { public i32 x public u8 y }\n\
                   fn main() -> i32 {\n    P p = P { x = 1, y = 2 }\n    \
                   u8 v = p.y\n    return 0\n}\n";
        let (program, res) = check(src);
        assert!(res.is_ok(), "expected ok, got {res:?}");
        // var init #1 is `p.y` — its field type is u8.
        assert_ty(nth_var_init(&program, "main", 1).expr_type, TypeBase::UInt, 8, 0);
    }

    // 14. Accessing an undeclared field is an error.
    #[test]
    fn struct_unknown_field_errors() {
        let src = "type P { public i32 x }\n\
                   fn main() -> i32 {\n    P p = P { x = 1 }\n    return p.z\n}\n";
        let (_program, res) = check(src);
        let errs = res.expect_err("expected unknown-field error");
        assert!(
            errs.iter()
                .any(|e| matches!(e, TypeCheckError::UnknownField { .. })),
            "got {errs:?}"
        );
    }

    // 15. A struct literal must name every field.
    #[test]
    fn struct_missing_field_errors() {
        let src = "type P { public i32 x public i32 y }\n\
                   fn main() -> i32 {\n    P p = P { x = 1 }\n    return p.x\n}\n";
        let (_program, res) = check(src);
        let errs = res.expect_err("expected missing-field error");
        assert!(
            errs.iter()
                .any(|e| matches!(e, TypeCheckError::MissingStructFields { .. })),
            "got {errs:?}"
        );
    }

    // 18. Value-block in a checked position adopts the target type; its
    //     `return` binds to the block, NOT the enclosing function (1000
    //     fits the block's i32 but not the function's u8 return).
    #[test]
    fn value_block_return_binds_to_block() {
        let src = "fn f() -> u8 {\n    i32 x = { return 1000 }\n    return 0\n}\n";
        let (program, res) = check(src);
        assert!(res.is_ok(), "expected ok, got {res:?}");
        assert_ty(nth_var_init(&program, "f", 0).expr_type, TypeBase::SInt, 32, 0);
    }

    // 19. Nested value-blocks: each `return` binds to its innermost block.
    #[test]
    fn value_block_nested() {
        let src = "fn main() -> i32 {\n    i32 x = {\n    i32 y = { return 21 }\n    return y * 2\n}\n    return x\n}\n";
        let (_p, res) = check(src);
        assert!(res.is_ok(), "expected ok, got {res:?}");
    }

    // 20. Synthesis position (condition): the first `return` fixes the type.
    #[test]
    fn value_block_synth_position() {
        let src = "fn main() -> i32 {\n    if { return true } {\n    return 1\n}\n    return 0\n}\n";
        let (_p, res) = check(src);
        assert!(res.is_ok(), "expected ok, got {res:?}");
    }

    // 21. A path that falls off the end without returning is rejected.
    #[test]
    fn value_block_missing_return_errors() {
        let src = "fn main(u32 argc, u8** argv) -> i32 {\n    i32 x = {\n    if argc > 1 { return 1 }\n}\n    return x\n}\n";
        let (_p, res) = check(src);
        let errs = res.expect_err("expected all-paths error");
        assert!(
            errs.iter()
                .any(|e| matches!(e, TypeCheckError::ValueBlockMissingReturn(_))),
            "got {errs:?}"
        );
    }

    // 22. Loops never satisfy the all-paths rule (conservative: `break`
    //     could skip the return).
    #[test]
    fn value_block_loop_return_rejected() {
        let src = "fn main() -> i32 {\n    i32 x = {\n    while true { return 1 }\n}\n    return x\n}\n";
        let (_p, res) = check(src);
        let errs = res.expect_err("expected all-paths error");
        assert!(
            errs.iter()
                .any(|e| matches!(e, TypeCheckError::ValueBlockMissingReturn(_))),
            "got {errs:?}"
        );
    }

    // 23. A bare `return` inside a value-block is rejected. (`return;` —
    //     a bare `return` directly before `}` is already a parse error.)
    #[test]
    fn value_block_bare_return_errors() {
        let src = "fn main() -> i32 {\n    i32 x = { return; }\n    return x\n}\n";
        let (_p, res) = check(src);
        let errs = res.expect_err("expected void-return error");
        assert!(
            errs.iter()
                .any(|e| matches!(e, TypeCheckError::ValueBlockVoidReturn(_))),
            "got {errs:?}"
        );
    }

    // 24. Brace disambiguation regression: `{1, 2, 3}` stays a list
    //     initializer (test 11 covers the positive case; this pins the
    //     single-element form, which is a 1-element list, not a block).
    #[test]
    fn single_element_brace_stays_list() {
        let (program, res) = check("fn main() -> i32 {\n    u8[1] arr = {5}\n    return 0\n}\n");
        assert!(res.is_ok(), "expected ok, got {res:?}");
        assert!(matches!(
            nth_var_init(&program, "main", 0).kind,
            ExprKind::ListInitializer(_)
        ));
    }

    // 16. A private method (no `public`) is not callable from outside the type.
    #[test]
    fn private_method_external_call_errors() {
        let src = "type C {\n    i32 n\n    \
                   public fn make(i32 v) -> C { return C { n = v } }\n    \
                   fn secret(this) -> i32 { return this.n }\n}\n\
                   fn main() -> i32 {\n    C c = C.make(1)\n    return c.secret()\n}\n";
        let (_p, res) = check(src);
        let errs = res.expect_err("expected inaccessible-method error");
        assert!(
            errs.iter().any(|e| matches!(e,
                TypeCheckError::InaccessibleMethod { method, type_name, .. }
                if method == "secret" && type_name == "C")),
            "got {errs:?}"
        );
    }

    // 17. A private method IS callable from within the type's own methods,
    //     and a public method is callable from outside. Mirrors the private-
    //     field accessibility rule.
    #[test]
    fn private_method_internal_call_ok() {
        let src = "type C {\n    i32 n\n    \
                   public fn make(i32 v) -> C { return C { n = v } }\n    \
                   public fn doubled(this) -> i32 { return this.secret() + this.secret() }\n    \
                   fn secret(this) -> i32 { return this.n }\n}\n\
                   fn main() -> i32 {\n    C c = C.make(1)\n    return c.doubled()\n}\n";
        let (_p, res) = check(src);
        assert!(res.is_ok(), "expected ok, got {res:?}");
    }
