use super::TypeChecker;
use crate::lexer::LangType;
use crate::parser::AsmSpec;
use crate::typechecker::errors::TypeCheckError;

impl TypeChecker {
    /// Validate an `asm fn`'s register contract against the compilation
    /// target.
    ///
    /// Every collision check compares the canonical register *family*, never
    /// the spelling: `rax` and `eax` are one physical register, and LLVM
    /// diagnoses nothing if two operands name it — it silently drops one.
    /// Comparing spellings would let that through.
    ///
    /// The duplicate rule deliberately applies **only among parameters**. An
    /// output sharing a register with an input (`-> i64: rax` alongside
    /// `i64 nr: rax`) is the ordinary in-out syscall form and must be
    /// accepted.
    pub(crate) fn check_asm_function(
        &mut self,
        proto: &crate::parser::FunctionProto,
        asm: &AsmSpec,
    ) {
        // An arch whose register model we don't have cannot be validated at
        // all, so there is no point resolving x86 names against it. Both x86
        // flavours are modelled; the register table then decides which names
        // are legal (e.g. `rax` exists on x86-64 but not i386).
        let Some(arch) = self
            .target
            .arch_define()
            .filter(|a| matches!(*a, "ARCH_X86_64" | "ARCH_I386"))
        else {
            self.errors.push(TypeCheckError::AsmUnsupportedTarget {
                name: proto.name.clone(),
                triple: self.target.triple().to_string(),
                position: asm.pos,
            });
            return;
        };

        // Operand types: only values that can actually live in a register.
        for (param_type, _) in &proto.params {
            self.check_pinnable_type(*param_type, proto.pos);
        }
        if !proto.return_type.is_void_value() {
            self.check_pinnable_type(proto.return_type, proto.pos);
        }

        // Parameters: resolve each name, then check it against the families
        // already taken by an earlier parameter.
        let mut param_families: Vec<(&'static str, String)> = Vec::new();
        for (reg, (param_type, param_name)) in asm.param_regs.iter().zip(&proto.params) {
            let Some(info) = self.resolve_asm_reg(arch, reg) else {
                continue;
            };
            self.check_register_class(*param_type, &info, reg);
            self.check_register_fits(arch, *param_type, &info, reg);
            if let Some((_, owner)) = param_families.iter().find(|(f, _)| *f == info.family) {
                self.errors.push(TypeCheckError::AsmDuplicateRegister {
                    register: reg.name.clone(),
                    param: owner.clone(),
                    position: reg.pos,
                });
                continue;
            }
            param_families.push((info.family, param_name.clone()));
        }

        // Return register: resolvable and unreserved, but explicitly *not*
        // checked against the parameters — see the in-out note above.
        let return_family = asm.return_reg.as_ref().and_then(|reg| {
            let info = self.resolve_asm_reg(arch, reg)?;
            if !proto.return_type.is_void_value() {
                self.check_register_class(proto.return_type, &info, reg);
                self.check_register_fits(arch, proto.return_type, &info, reg);
            }
            Some(info.family)
        });

        // Clobbers: `memory` is a pseudo-register that names no hardware, so
        // it skips register resolution and every family-based check — but a
        // repeat of it is the same user mistake the duplicate rule exists to
        // catch, so it is reported through the same diagnostic.
        let mut clobber_families: Vec<&'static str> = Vec::new();
        let mut saw_memory = false;
        for clobber in &asm.clobbers {
            if clobber.name == crate::asm::MEMORY_CLOBBER {
                if saw_memory {
                    self.errors.push(TypeCheckError::AsmDuplicateClobber {
                        register: clobber.name.clone(),
                        position: clobber.pos,
                    });
                }
                saw_memory = true;
                continue;
            }
            let Some(info) = self.resolve_asm_reg(arch, clobber) else {
                continue;
            };
            if param_families.iter().any(|(f, _)| *f == info.family)
                || return_family == Some(info.family)
            {
                self.errors.push(TypeCheckError::AsmClobberIsOperand {
                    register: clobber.name.clone(),
                    position: clobber.pos,
                });
                continue;
            }
            if clobber_families.contains(&info.family) {
                self.errors.push(TypeCheckError::AsmDuplicateClobber {
                    register: clobber.name.clone(),
                    position: clobber.pos,
                });
                continue;
            }
            clobber_families.push(info.family);
        }
    }

    /// Resolve one register name against `arch`'s table, reporting an unknown
    /// or reserved name. `None` means an error was reported and the caller
    /// should skip its family-based checks for this register.
    fn resolve_asm_reg(
        &mut self,
        arch: &str,
        reg: &crate::parser::AsmReg,
    ) -> Option<crate::asm::RegInfo> {
        let Some(info) = crate::asm::lookup_register(arch, &reg.name) else {
            self.errors.push(TypeCheckError::AsmUnknownRegister {
                register: reg.name.clone(),
                arch: arch.to_string(),
                position: reg.pos,
            });
            return None;
        };
        // Reserved takes precedence over every collision rule: naming rsp at
        // all is the error, regardless of what else names it.
        if crate::asm::is_reserved_family(info.family) {
            self.errors.push(TypeCheckError::AsmReservedRegister {
                register: reg.name.clone(),
                position: reg.pos,
            });
            return None;
        }
        Some(info)
    }

    /// Reject an operand pinned to a register spelling too narrow to hold it.
    ///
    /// This is the one place [`crate::asm::RegInfo::width_bits`] is consulted,
    /// and it deliberately checks in one direction only. A *narrower* type in
    /// a wider spelling (`i32 x: rax`) is the orthogonality rule working:
    /// LLVM sizes the physical register from the operand's LLVM type and
    /// selects `%eax`. A *wider* type in a narrower spelling (`i64 v: al`) is
    /// the same mechanism silently discarding what the user wrote — LLVM hands
    /// back the full `rax`, so `al` means `rax` and the author's belief that
    /// only the low byte is live is wrong. LLVM diagnoses nothing either way.
    ///
    /// This does not infer a type from a register: the declared type still
    /// drives every conversion. It only rejects a pairing that cannot mean
    /// what it says.
    fn check_register_fits(
        &mut self,
        arch: &str,
        ty: LangType,
        info: &crate::asm::RegInfo,
        reg: &crate::parser::AsmReg,
    ) {
        // A pointer occupies the target's full pointer width regardless of its
        // pointee (`u8*` is 64-bit on x86-64, though its `size_bits` is 8).
        // `check_pinnable_type` has already rejected anything that is neither
        // pointer-like nor an 8/16/32/64-bit integer.
        let type_bits = if ty.is_pointer_like() {
            crate::asm::pointer_width_bits(arch)
        } else {
            ty.size_bits
        };

        if type_bits > info.width_bits {
            self.errors.push(TypeCheckError::AsmRegisterTooNarrow {
                found: self.type_name(&ty),
                type_bits,
                register: reg.name.clone(),
                reg_bits: info.width_bits,
                position: reg.pos,
            });
        }
    }

    /// Reject operand types that cannot be pinned to any register: integers of
    /// 8/16/32/64 bits, floats of 32/64, and pointer-like values qualify.
    /// Other widths are a real `X86ISelLowering` error, and a struct-by-value
    /// operand would take the `byval` path, which is meaningless under
    /// register pinning. (`u0` parameters are already rejected by
    /// `check_proto`.) Which *bank* the type belongs in is
    /// [`Self::check_register_class`]'s job.
    fn check_pinnable_type(&mut self, ty: LangType, pos: crate::lexer::Position) {
        let pinnable = ty.is_pointer_like()
            || (ty.is_plain_int() && matches!(ty.size_bits, 8 | 16 | 32 | 64))
            || (ty.is_plain_float() && matches!(ty.size_bits, 32 | 64));
        if !pinnable {
            self.errors.push(TypeCheckError::AsmUnpinnableType {
                found: self.type_name(&ty),
                position: pos,
            });
        }
    }

    /// Reject an operand pinned to the wrong register bank. Floats live in
    /// SSE registers, integers and pointers in general-purpose ones; LLVM
    /// cannot lower the crossed pairings and does not diagnose them.
    fn check_register_class(
        &mut self,
        ty: LangType,
        info: &crate::asm::RegInfo,
        reg: &crate::parser::AsmReg,
    ) {
        let expected = if ty.is_plain_float() {
            crate::asm::RegClass::Sse
        } else {
            crate::asm::RegClass::Gpr
        };
        if info.class != expected {
            self.errors
                .push(TypeCheckError::AsmRegisterClassMismatch {
                    found: self.type_name(&ty),
                    register: reg.name.clone(),
                    expected: expected.describe(),
                    actual: info.class.describe(),
                    position: reg.pos,
                });
        }
    }
}
