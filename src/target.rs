//! Compilation target — the single source of truth for "what platform is this
//! build for", shared by the preprocessor (`OS_*`/`ARCH_*` defines) and codegen
//! (the LLVM target machine).
//!
//! The *build* host (`cfg!(target_os/arch)`, baked into the `aspc` binary) and
//! the *compilation target* must not be conflated. The target is what the
//! program is compiled *for*: it defaults to the runtime host (resolved through
//! LLVM, not `cfg!`, so a cross-compiled `aspc` still reports the machine it
//! runs on) and can be overridden with `--target`.

use inkwell::targets::{TargetMachine, TargetTriple};

/// Identified by its LLVM triple (`<arch>-<vendor>-<os>-<env>`). Building one
/// never touches LLVM's target registry — it is just the triple string plus
/// string-level OS/arch classification. Whether LLVM can actually *use* the
/// triple is validated in [`crate::codegen::CodeGenerator::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetSpec {
    triple: String,
}

impl TargetSpec {
    /// The triple of the machine `aspc` is running on, resolved through LLVM
    /// (not `cfg!`). The default for `--target` and every host-defaulting
    /// convenience constructor.
    #[must_use]
    pub fn host() -> Self {
        Self::from_llvm_triple(&TargetMachine::get_default_triple())
    }

    /// Never fails: a malformed triple only surfaces as an error once LLVM is
    /// asked to resolve it (a codegen concern — a `lex`/`parse` run never
    /// touches LLVM, so it must not reject a triple only codegen would).
    #[must_use]
    pub fn parse(triple: &str) -> Self {
        Self {
            triple: triple.to_string(),
        }
    }

    fn from_llvm_triple(triple: &TargetTriple) -> Self {
        Self {
            triple: triple.as_str().to_string_lossy().into_owned(),
        }
    }

    #[must_use]
    pub fn triple(&self) -> &str {
        &self.triple
    }

    /// The [`TargetTriple`] LLVM's `Target`/`TargetMachine` setup needs.
    #[must_use]
    pub fn llvm_triple(&self) -> TargetTriple {
        TargetTriple::create(&self.triple)
    }

    /// Matched by substring (case-insensitively) since LLVM triples spell each
    /// OS consistently — `linux`, `windows`, `darwin`/`macos` — regardless of
    /// the surrounding vendor/environment components.
    #[must_use]
    pub fn os_define(&self) -> Option<&'static str> {
        let lower = self.triple.to_ascii_lowercase();
        if lower.contains("linux") {
            Some("OS_LINUX")
        } else if lower.contains("windows") {
            Some("OS_WINDOWS")
        } else if lower.contains("darwin") || lower.contains("macos") {
            Some("OS_MACOS")
        } else {
            None
        }
    }

    /// Every 32-bit x86 spelling (`i386`/`i486`/`i586`/`i686`) collapses to one
    /// `ARCH_I386`: they share a 32-bit ABI and register file, differing only
    /// in the baseline CPU — a codegen concern (`-mcpu`), not an `$ifdef` one.
    #[must_use]
    pub fn arch_define(&self) -> Option<&'static str> {
        let arch = self.triple.split('-').next().unwrap_or("");
        match arch.to_ascii_lowercase().as_str() {
            "x86_64" | "amd64" => Some("ARCH_X86_64"),
            "aarch64" | "arm64" => Some("ARCH_AARCH64"),
            "i386" | "i486" | "i586" | "i686" => Some("ARCH_I386"),
            _ => None,
        }
    }
}

impl std::fmt::Display for TargetSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.triple)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_x86_64_and_aarch64_arches() {
        assert_eq!(
            TargetSpec::parse("x86_64-unknown-linux-gnu").arch_define(),
            Some("ARCH_X86_64")
        );
        assert_eq!(
            TargetSpec::parse("aarch64-unknown-linux-gnu").arch_define(),
            Some("ARCH_AARCH64")
        );
    }

    #[test]
    fn every_32_bit_x86_spelling_maps_to_arch_i386() {
        for arch in ["i386", "i486", "i586", "i686"] {
            let triple = format!("{arch}-unknown-none-elf");
            assert_eq!(
                TargetSpec::parse(&triple).arch_define(),
                Some("ARCH_I386"),
                "{triple} should seed ARCH_I386"
            );
        }
    }

    #[test]
    fn a_bare_metal_i386_triple_names_no_os_but_a_known_arch() {
        // `i386-unknown-none-elf` — the freestanding kernel target — has no OS
        // component, so no `OS_*` define is seeded, but the arch is known.
        let spec = TargetSpec::parse("i386-unknown-none-elf");
        assert_eq!(spec.os_define(), None);
        assert_eq!(spec.arch_define(), Some("ARCH_I386"));
    }
}
