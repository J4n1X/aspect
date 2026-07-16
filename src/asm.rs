//! Inline-asm register model: the per-target register table `asm fn`
//! validates against.
//!
//! Pure data and string classification — like [`crate::target::TargetSpec`],
//! it never touches LLVM's target registry, so it is usable from the type
//! checker (which runs long before any `TargetMachine` exists, and must
//! reject `rax` under `--target aarch64-*` even though this binary has no
//! AArch64 backend at all).
//!
//! The central concept here is the **register family**. `rax`, `eax`, `ax`
//! and `al` are four spellings of one physical register; LLVM neither
//! rejects nor tolerates two operands naming the same physical register —
//! it silently drops one of them. Every collision check therefore compares
//! [`RegInfo::family`], never the spelling the user wrote.

/// Which bank a register lives in. A value can only be pinned to its own
/// bank's registers: LLVM has no way to put an `f64` in `rax` or an `i64` in
/// `xmm0`, and asking it to is a frontend error rather than something it
/// diagnoses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegClass {
    /// Integers and pointers.
    Gpr,
    /// Floats.
    Sse,
}

impl RegClass {
    /// How this bank names itself in a diagnostic, article included.
    #[must_use]
    pub fn describe(self) -> &'static str {
        match self {
            Self::Gpr => "a general-purpose register",
            Self::Sse => "an SSE register",
        }
    }
}

/// What is known about one register spelling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RegInfo {
    /// Canonical name of the physical register this spelling aliases (`rax`
    /// for every one of `rax`/`eax`/`ax`/`al`).
    pub family: &'static str,
    /// Width of this spelling in bits. Informational only — the declared
    /// Aspect type, never the register, drives conversions.
    pub width_bits: u32,
    pub class: RegClass,
}

/// x86-64 general-purpose registers as `(spelling, family, width_bits)`.
///
/// APX `r16`-`r31` are excluded (not baseline x86-64). The legacy high-byte
/// registers `ah`/`bh`/`ch`/`dh` are deliberately absent: they cannot be
/// encoded alongside any REX-prefixed register, so pinning one would fail
/// inside the encoder with a far worse diagnostic than "unknown register".
static X86_64_GPRS: &[(&str, &str, u32)] = &[
    ("rax", "rax", 64),
    ("eax", "rax", 32),
    ("ax", "rax", 16),
    ("al", "rax", 8),
    ("rbx", "rbx", 64),
    ("ebx", "rbx", 32),
    ("bx", "rbx", 16),
    ("bl", "rbx", 8),
    ("rcx", "rcx", 64),
    ("ecx", "rcx", 32),
    ("cx", "rcx", 16),
    ("cl", "rcx", 8),
    ("rdx", "rdx", 64),
    ("edx", "rdx", 32),
    ("dx", "rdx", 16),
    ("dl", "rdx", 8),
    ("rsi", "rsi", 64),
    ("esi", "rsi", 32),
    ("si", "rsi", 16),
    ("sil", "rsi", 8),
    ("rdi", "rdi", 64),
    ("edi", "rdi", 32),
    ("di", "rdi", 16),
    ("dil", "rdi", 8),
    ("rbp", "rbp", 64),
    ("ebp", "rbp", 32),
    ("bp", "rbp", 16),
    ("bpl", "rbp", 8),
    ("rsp", "rsp", 64),
    ("esp", "rsp", 32),
    ("sp", "rsp", 16),
    ("spl", "rsp", 8),
    ("r8", "r8", 64),
    ("r8d", "r8", 32),
    ("r8w", "r8", 16),
    ("r8b", "r8", 8),
    ("r9", "r9", 64),
    ("r9d", "r9", 32),
    ("r9w", "r9", 16),
    ("r9b", "r9", 8),
    ("r10", "r10", 64),
    ("r10d", "r10", 32),
    ("r10w", "r10", 16),
    ("r10b", "r10", 8),
    ("r11", "r11", 64),
    ("r11d", "r11", 32),
    ("r11w", "r11", 16),
    ("r11b", "r11", 8),
    ("r12", "r12", 64),
    ("r12d", "r12", 32),
    ("r12w", "r12", 16),
    ("r12b", "r12", 8),
    ("r13", "r13", 64),
    ("r13d", "r13", 32),
    ("r13w", "r13", 16),
    ("r13b", "r13", 8),
    ("r14", "r14", 64),
    ("r14d", "r14", 32),
    ("r14w", "r14", 16),
    ("r14b", "r14", 8),
    ("r15", "r15", 64),
    ("r15d", "r15", 32),
    ("r15w", "r15", 16),
    ("r15b", "r15", 8),
];

/// x86-64 SSE registers as `(spelling, family, width_bits)`.
///
/// `xmm0`-`xmm15` are baseline x86-64 (SSE2 is mandatory), which is what makes
/// them safe to pin under the `generic` CPU the target machine is built with.
/// `ymm`/`zmm` are deliberately absent: they need AVX, which `generic` does not
/// enable. Each register is its own family — unlike the GPRs there are no
/// narrower spellings to alias.
static X86_64_SSE: &[(&str, &str, u32)] = &[
    ("xmm0", "xmm0", 128),
    ("xmm1", "xmm1", 128),
    ("xmm2", "xmm2", 128),
    ("xmm3", "xmm3", 128),
    ("xmm4", "xmm4", 128),
    ("xmm5", "xmm5", 128),
    ("xmm6", "xmm6", 128),
    ("xmm7", "xmm7", 128),
    ("xmm8", "xmm8", 128),
    ("xmm9", "xmm9", 128),
    ("xmm10", "xmm10", 128),
    ("xmm11", "xmm11", 128),
    ("xmm12", "xmm12", 128),
    ("xmm13", "xmm13", 128),
    ("xmm14", "xmm14", 128),
    ("xmm15", "xmm15", 128),
];

/// The pseudo-register naming "this asm touches memory". Legal only in
/// `clobbers(...)`, never as an operand pin — as a pin it is not in the
/// register table and so reports as an unknown register.
pub const MEMORY_CLOBBER: &str = "memory";

/// Resolve a register spelling for `arch_define` (a
/// [`crate::target::TargetSpec::arch_define`] value, e.g. `"ARCH_X86_64"`).
///
/// Returns `None` for an unknown name, or for any architecture whose
/// register table we do not model — which is what makes `rax` under
/// `--target aarch64-*` a clean compile error rather than a silent accept.
#[must_use]
pub fn lookup_register(arch_define: &str, name: &str) -> Option<RegInfo> {
    if arch_define != "ARCH_X86_64" {
        return None;
    }
    find_in(X86_64_GPRS, name, RegClass::Gpr).or_else(|| find_in(X86_64_SSE, name, RegClass::Sse))
}

fn find_in(
    table: &'static [(&'static str, &'static str, u32)],
    name: &str,
    class: RegClass,
) -> Option<RegInfo> {
    table
        .iter()
        .find(|(spelling, _, _)| *spelling == name)
        .map(|(_, family, width_bits)| RegInfo {
            family,
            width_bits: *width_bits,
            class,
        })
}

/// The width in bits of a pointer on `arch_define`.
///
/// Not `LangType::size_bits`, which describes the *pointee* (`u8*` reports 8)
/// and so cannot answer this.
#[must_use]
pub fn pointer_width_bits(arch_define: &str) -> u32 {
    debug_assert!(
        matches!(arch_define, "ARCH_X86_64" | "ARCH_AARCH64"),
        "pointer width asked for unmodelled arch '{arch_define}'"
    );
    64
}

/// True for the stack- and frame-pointer families, which may never be pinned
/// or clobbered.
///
/// `rsp` is the live hardware stack pointer the calling convention depends on
/// continuously; `rbp` may address spill slots under the default frame
/// lowering — which is exactly what an unoptimised `asm fn` uses before
/// `alwaysinline` folds it away. Checked per *family*, so `esp` is rejected
/// for the same reason `rsp` is.
#[must_use]
pub fn is_reserved_family(family: &str) -> bool {
    matches!(family, "rsp" | "rbp")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_x86_64_spellings_to_their_family() {
        assert_eq!(lookup_register("ARCH_X86_64", "rax").unwrap().family, "rax");
        // The aliasing case that matters: a 32-bit spelling names the same
        // physical register as its 64-bit family, so collision checks must
        // see them as one register.
        assert_eq!(lookup_register("ARCH_X86_64", "eax").unwrap().family, "rax");
        assert_eq!(lookup_register("ARCH_X86_64", "ax").unwrap().family, "rax");
        assert_eq!(lookup_register("ARCH_X86_64", "al").unwrap().family, "rax");
        assert_eq!(lookup_register("ARCH_X86_64", "r11d").unwrap().family, "r11");
    }

    #[test]
    fn records_the_width_of_each_spelling() {
        assert_eq!(lookup_register("ARCH_X86_64", "rax").unwrap().width_bits, 64);
        assert_eq!(lookup_register("ARCH_X86_64", "eax").unwrap().width_bits, 32);
        assert_eq!(lookup_register("ARCH_X86_64", "ax").unwrap().width_bits, 16);
        assert_eq!(lookup_register("ARCH_X86_64", "al").unwrap().width_bits, 8);
    }

    #[test]
    fn rejects_unknown_register_names() {
        assert!(lookup_register("ARCH_X86_64", "rex").is_none());
        assert!(lookup_register("ARCH_X86_64", "").is_none());
        // `memory` is a clobber pseudo-register, never a real one.
        assert!(lookup_register("ARCH_X86_64", MEMORY_CLOBBER).is_none());
        // The always-injected implicit clobbers are not user-writable names.
        assert!(lookup_register("ARCH_X86_64", "flags").is_none());
        assert!(lookup_register("ARCH_X86_64", "dirflag").is_none());
        assert!(lookup_register("ARCH_X86_64", "fpsr").is_none());
    }

    #[test]
    fn rejects_high_byte_registers() {
        // Unencodable alongside REX-prefixed registers; excluded on purpose.
        for name in ["ah", "bh", "ch", "dh"] {
            assert!(lookup_register("ARCH_X86_64", name).is_none());
        }
    }

    #[test]
    fn knows_no_registers_for_unmodelled_architectures() {
        // The hard requirement: `rax` under an aarch64 target is an error,
        // never a silent accept.
        assert!(lookup_register("ARCH_AARCH64", "rax").is_none());
        assert!(lookup_register("ARCH_AARCH64", "x0").is_none());
        assert!(lookup_register("ARCH_RISCV64", "rax").is_none());
    }

    #[test]
    fn treats_every_stack_and_frame_pointer_spelling_as_reserved() {
        for name in ["rsp", "esp", "sp", "spl", "rbp", "ebp", "bp", "bpl"] {
            let info = lookup_register("ARCH_X86_64", name)
                .unwrap_or_else(|| panic!("{name} should be a known register"));
            assert!(
                is_reserved_family(info.family),
                "{name} (family {}) must be reserved",
                info.family
            );
        }
    }

    #[test]
    fn treats_ordinary_registers_as_unreserved() {
        for name in ["rax", "rbx", "rcx", "rdx", "rsi", "rdi", "r8", "r15"] {
            let info = lookup_register("ARCH_X86_64", name).unwrap();
            assert!(!is_reserved_family(info.family));
        }
    }

    #[test]
    fn table_families_are_self_consistent() {
        // Every family name must itself be a 64-bit spelling in the table:
        // the family is what diagnostics and collision checks key on, so a
        // typo'd family would silently split one physical register in two.
        for (spelling, family, _) in X86_64_GPRS {
            let info = lookup_register("ARCH_X86_64", family)
                .unwrap_or_else(|| panic!("family {family} of {spelling} is not itself a register"));
            assert_eq!(info.family, *family);
            assert_eq!(info.width_bits, 64, "family {family} should be the 64-bit spelling");
        }
    }
}
