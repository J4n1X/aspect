//! `asm fn` lowering.
//!
//! An `asm fn` becomes a real LLVM function — internal, `alwaysinline` —
//! whose body is a single inline-asm call plus a return. Because it is a
//! genuine function, call sites stay ordinary calls and the existing call
//! path handles them unchanged; `asm fn` is a declaration form, not an
//! expression form.
//!
//! ## The always-injected clobbers
//!
//! Every constraint string ends with `~{dirflag},~{fpsr},~{flags}`, as clang
//! appends to every x86 asm block. This is not belt-and-braces: almost any
//! instruction can touch EFLAGS, and *`sideeffect` does not protect against
//! it*. Without `~{flags}` a caller holding a live comparison across the asm
//! miscompiles — LLVM keeps its `cmp` above the block and branches on flags
//! the asm destroyed. Omitting them buys nothing and costs a silent wrong
//! answer, so it is compiler-decided, like `sideeffect`.

use inkwell::attributes::{Attribute, AttributeLoc};
use inkwell::values::BasicMetadataValueEnum;
use inkwell::InlineAsmDialect;

use crate::codegen::{CodeGenerator, CodegenError};
use crate::parser::{AsmSpec, Function, NakedSpec};

/// Build the LLVM constraint string for `spec`. LangRef requires exactly this
/// order — output, inputs, clobbers — and forbids intermingling.
pub(crate) fn constraint_string(spec: &AsmSpec) -> String {
    let mut parts: Vec<String> = Vec::new();

    if let Some(reg) = &spec.return_reg {
        parts.push(format!("={{{}}}", reg.name));
    }
    // An input naming the same register as the output (the in-out `rax`
    // syscall case) stays an ordinary untied `{rax}`: the numeric-tie form
    // (`0`) constrains the register *allocator*, and nothing is left to
    // allocate once both ends are hard-pinned by name.
    for reg in &spec.param_regs {
        parts.push(format!("{{{}}}", reg.name));
    }
    for clobber in &spec.clobbers {
        parts.push(format!("~{{{}}}", clobber.name));
    }
    // Always-on x86 clobbers — see the module docs.
    for implicit in ["dirflag", "fpsr", "flags"] {
        parts.push(format!("~{{{implicit}}}"));
    }

    parts.join(",")
}

impl<'ctx> CodeGenerator<'ctx> {
    /// The function was already declared from its `proto` in pass 1; this only
    /// fills in the body (a single inline-asm call plus a return).
    pub(crate) fn generate_asm_function(
        &mut self,
        func: &Function,
        spec: &AsmSpec,
    ) -> Result<(), CodegenError> {
        let function = *self.functions.get(&func.proto.name).ok_or_else(|| {
            CodegenError::UndefinedFunction(func.proto.name.clone(), func.proto.pos)
        })?;

        // Linkage was decided at declaration; forcing it here would silently
        // demote a `public asm fn`.
        let kind_id = Attribute::get_named_enum_kind_id("alwaysinline");
        function.add_attribute(
            AttributeLoc::Function,
            self.context.create_enum_attribute(kind_id, 0),
        );

        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        // `create_inline_asm` and `build_indirect_call` must get the *identical*
        // FunctionType, and inkwell doesn't check — a mismatch only surfaces at
        // `module.verify()`, far from its cause. So reuse the declared type.
        let fn_type = function.get_type();

        let asm_ptr = self.context.create_inline_asm(
            fn_type,
            spec.lines.join("\n"),
            constraint_string(spec),
            true,                          // sideeffect: always
            false,                         // alignstack
            Some(InlineAsmDialect::Intel), // `None` would silently mean AT&T
            false,                         // can_throw
        );

        // Operands map 1:1 to the LLVM params: the type checker rejects
        // struct operands, so there is no sret/byval offset to account for.
        let args: Vec<BasicMetadataValueEnum<'ctx>> =
            function.get_param_iter().map(Into::into).collect();
        let call = self
            .builder
            .build_indirect_call(fn_type, asm_ptr, &args, "asm.ret")?;

        match call.try_as_basic_value().basic() {
            Some(value) => self.builder.build_return(Some(&value))?,
            None => self.builder.build_return(None)?, // `-> u0` asm fn
        };

        // A callee named only in asm text (`call foo`) has no IR reference, so
        // `globaldce` would strip it. Retain any word resolving to a function;
        // over-approximating is safe — a false hit was already reachable.
        for line in &spec.lines {
            for word in line.split_whitespace() {
                if let Some(func) = self.module.get_function(word) {
                    self.asm_retained.push(func);
                }
            }
        }

        Ok(())
    }

    /// A `naked` (no prologue/epilogue) function whose body is one side-effecting
    /// inline-asm block plus `unreachable`. No register contract or operands:
    /// arguments stay in their ABI-incoming registers and the asm body owns the
    /// calling convention. `noinline` because a naked body can't be spliced into
    /// a caller; `unreachable` because control leaves only through the asm's own
    /// `ret`/`jmp`/`syscall`.
    pub(crate) fn generate_naked_function(
        &mut self,
        func: &Function,
        spec: &NakedSpec,
    ) -> Result<(), CodegenError> {
        let function = *self.functions.get(&func.proto.name).ok_or_else(|| {
            CodegenError::UndefinedFunction(func.proto.name.clone(), func.proto.pos)
        })?;

        // Linkage was decided at declaration; forcing it here would silently
        // demote a `public naked fn`.
        for attr in ["naked", "noinline"] {
            let kind_id = Attribute::get_named_enum_kind_id(attr);
            function.add_attribute(
                AttributeLoc::Function,
                self.context.create_enum_attribute(kind_id, 0),
            );
        }

        let entry = self.context.append_basic_block(function, "entry");
        self.builder.position_at_end(entry);

        // A void, no-operand inline-asm block — the asm references ABI
        // registers directly, so there is nothing to pin or clobber.
        let asm_ty = self.context.void_type().fn_type(&[], false);
        let asm_ptr = self.context.create_inline_asm(
            asm_ty,
            spec.lines.join("\n"),
            String::new(),                 // no operands, no clobbers
            true,                          // sideeffect: always
            false,                         // alignstack
            Some(InlineAsmDialect::Intel), // `None` would silently mean AT&T
            false,                         // can_throw
        );
        self.builder.build_indirect_call(asm_ty, asm_ptr, &[], "")?;
        self.builder.build_unreachable()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::Position;
    use crate::parser::AsmReg;

    fn reg(name: &str) -> AsmReg {
        AsmReg {
            name: name.to_string(),
            pos: Position::new(1, 1),
        }
    }

    fn spec(params: &[&str], ret: Option<&str>, clobbers: &[&str]) -> AsmSpec {
        AsmSpec {
            param_regs: params.iter().map(|n| reg(n)).collect(),
            return_reg: ret.map(reg),
            clobbers: clobbers.iter().map(|n| reg(n)).collect(),
            lines: vec!["nop".to_string()],
            pos: Position::new(1, 1),
        }
    }

    #[test]
    fn builds_the_syscall_constraint_string() {
        // The in-out case: `rax` is both the output and the first input.
        let s = spec(
            &["rax", "rdi", "rsi", "rdx"],
            Some("rax"),
            &["rcx", "r11", "memory"],
        );
        assert_eq!(
            constraint_string(&s),
            "={rax},{rax},{rdi},{rsi},{rdx},~{rcx},~{r11},~{memory},~{dirflag},~{fpsr},~{flags}"
        );
    }

    #[test]
    fn builds_a_plain_two_input_constraint_string() {
        let s = spec(&["rax", "rbx"], Some("rax"), &[]);
        assert_eq!(
            constraint_string(&s),
            "={rax},{rax},{rbx},~{dirflag},~{fpsr},~{flags}"
        );
    }

    #[test]
    fn omits_the_output_constraint_for_a_void_asm_fn() {
        let s = spec(&["rdi", "rsi"], None, &["memory"]);
        assert_eq!(
            constraint_string(&s),
            "{rdi},{rsi},~{memory},~{dirflag},~{fpsr},~{flags}"
        );
    }

    #[test]
    fn builds_a_constraint_string_for_a_zero_operand_asm_fn() {
        let s = spec(&[], None, &[]);
        assert_eq!(constraint_string(&s), "~{dirflag},~{fpsr},~{flags}");
    }

    #[test]
    fn builds_a_constraint_string_for_an_output_only_asm_fn() {
        let s = spec(&[], Some("rax"), &[]);
        assert_eq!(constraint_string(&s), "={rax},~{dirflag},~{fpsr},~{flags}");
    }

    #[test]
    fn emits_register_spellings_verbatim() {
        // Types and registers are orthogonal: a sub-register spelling reaches
        // the constraint string exactly as written, and LLVM matches it to
        // the operand's LLVM type.
        let s = spec(&["eax", "r8b"], Some("ax"), &[]);
        assert_eq!(
            constraint_string(&s),
            "={ax},{eax},{r8b},~{dirflag},~{fpsr},~{flags}"
        );
    }

    /// The lowering's *emitted-IR* invariants, as opposed to the constraint
    /// string alone.
    ///
    /// These exist because every property the design fixes by decision —
    /// `sideeffect`, `alwaysinline`, internal linkage, the Intel dialect,
    /// newline-joined lines — lives in `generate_asm_function`'s call to
    /// `create_inline_asm`, where nothing but the emitted module can observe
    /// it. Without these, flipping `sideeffect` to `false` (letting LLVM
    /// delete or duplicate asm blocks whose results are unused) passed the
    /// entire suite.
    ///
    /// x86-gated: an `asm fn` naming `rax` is rejected by the type checker on
    /// any other host, so on a non-x86 machine these would test nothing. This
    /// is the Rust-side counterpart of the corpus's `$ifdef ARCH_X86_64`.
    #[cfg(target_arch = "x86_64")]
    mod ir {
        use crate::codegen::CodeGenerator;
        use crate::parser::Parser;
        use crate::target::TargetSpec;
        use crate::typechecker::TypeChecker;
        use inkwell::context::Context;

        /// Compile `source` all the way to an LLVM IR string.
        fn ir_for(source: &str, context: &Context) -> String {
            let tokens = crate::lexer::tokenize(source.to_string()).expect("lex");
            let mut parser = Parser::new(tokens);
            let mut program = parser.parse_program().expect("parse");
            let mut tc = TypeChecker::new();
            tc.check_program(&mut program).expect("typecheck");
            let mut codegen =
                CodeGenerator::new(context, "asm_test", &TargetSpec::host()).expect("codegen setup");
            codegen.generate(&program).expect("generate");
            codegen.print_ir_to_string()
        }

        const SYSCALL_SRC: &str = r#"
asm fn two_line(i64 nr: rax, i64 a1: rdi) -> i64: rax
    clobbers(rcx, r11, memory)
{
    "mov r10, rcx"
    "syscall"
}

fn main(u32 argc, u8 **argv) -> i32 {
    return two_line(1, 2) as i32
}
"#;

        #[test]
        fn asm_call_is_marked_sideeffect() {
            // Design decision: sideeffect is always on. Without it LLVM may
            // drop an asm block whose result is unused.
            let context = Context::create();
            assert!(ir_for(SYSCALL_SRC, &context).contains("asm sideeffect"));
        }

        #[test]
        fn asm_call_uses_the_intel_dialect() {
            let context = Context::create();
            assert!(ir_for(SYSCALL_SRC, &context).contains("inteldialect"));
        }

        #[test]
        fn asm_fn_is_internal_and_alwaysinline() {
            // `alwaysinline` is what folds the wrapper away at -O1+; internal
            // linkage is what lets it be dropped once every call is inlined.
            let context = Context::create();
            let ir = ir_for(SYSCALL_SRC, &context);
            assert!(ir.contains("define internal i64 @two_line"), "{ir}");
            assert!(ir.contains("alwaysinline"), "{ir}");
        }

        #[test]
        fn asm_lines_are_joined_with_newlines() {
            // Adjacent string literals are one line of asm each, joined into
            // the single string LLVM wants — `\0A` once escaped into the IR.
            let context = Context::create();
            assert!(ir_for(SYSCALL_SRC, &context).contains(r"mov r10, rcx\0Asyscall"));
        }

        #[test]
        fn call_sites_stay_ordinary_calls() {
            // The whole point of lowering to a real function: no new
            // expression form, so the call site is an ordinary call.
            let context = Context::create();
            assert!(ir_for(SYSCALL_SRC, &context).contains("call i64 @two_line"));
        }
    }
}
