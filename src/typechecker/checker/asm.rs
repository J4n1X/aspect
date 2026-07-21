use super::TypeChecker;
use crate::lexer::LangType;
use crate::parser::AsmSpec;
use crate::typechecker::errors::TypeCheckError;

impl TypeChecker {
    /// Every collision check compares the canonical register *family*, not the
    /// spelling: `rax` and `eax` are one physical register that LLVM silently
    /// drops if two operands name it. The duplicate rule applies **only among
    /// parameters** — an output sharing a register with an input (the in-out
    /// syscall form) must be accepted.
    pub(crate) fn check_asm_function(
        &mut self,
        proto: &crate::parser::FunctionProto,
        asm: &AsmSpec,
    ) {
        // Only the modelled x86 arches can be validated; the register table
        // then decides which names are legal (`rax` on x86-64 but not i386).
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

        // `memory` is a pseudo-register naming no hardware, so it skips
        // resolution and family checks — but a repeat is still reported as a
        // duplicate.
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

    /// Checks one direction only. A narrower type in a wider spelling
    /// (`i32 x: rax`) is fine — LLVM selects `%eax` from the operand's type. A
    /// wider type in a narrower spelling (`i64 v: al`) is rejected: LLVM would
    /// silently hand back the full `rax`, so the author's belief that only the
    /// low byte is live is wrong. The declared type still drives every
    /// conversion; this only rejects a pairing that can't mean what it says.
    fn check_register_fits(
        &mut self,
        arch: &str,
        ty: LangType,
        info: &crate::asm::RegInfo,
        reg: &crate::parser::AsmReg,
    ) {
        // A pointer occupies the full pointer width regardless of pointee
        // (`u8*` is 64-bit on x86-64, though its `size_bits` is 8).
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

    /// Only 8/16/32/64-bit integers, 32/64-bit floats, and pointer-like values
    /// can be pinned. Other widths are a real `X86ISelLowering` error, and a
    /// struct-by-value operand would take the meaningless `byval` path. Which
    /// *bank* the type belongs in is [`Self::check_register_class`]'s job.
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
