//! Compilation target — the single source of truth for "what platform is
//! this build for", shared by the preprocessor (`OS_*`/`ARCH_*` defines for
//! `$ifdef`) and the code generator (the LLVM target machine).
//!
//! Compilation is native-only today, so [`TargetSpec::host()`] and the
//! compiler binary's own build host always agree — but they are not the
//! same concept, and must not be conflated:
//!
//! - The *build* host is `cfg!(target_os = ..)` / `cfg!(target_arch = ..)`:
//!   compile-time constants of the `aspc` binary itself, baked in when
//!   `aspc` was built.
//! - The *compilation target* is what the program being compiled is *for*.
//!   It defaults to the *runtime* host (resolved through LLVM, not `cfg!`,
//!   so a cross-compiled `aspc` binary still reports the machine it is
//!   actually running on) and can be overridden with `--target`.
//!
//! Once the standard library picks a syscall backend with `$ifdef OS_LINUX`
//! / `$ifdef OS_WINDOWS`, that decision must follow the compilation target —
//! this module exists so there is exactly one place that decision is made.

use inkwell::targets::{TargetMachine, TargetTriple};

/// A compilation target, identified by its LLVM triple
/// (`<arch>-<vendor>-<os>-<env>`, e.g. `x86_64-unknown-linux-gnu`).
///
/// Building a `TargetSpec` never touches LLVM's target registry — it is
/// just the triple string plus string-level OS/arch classification for the
/// preprocessor. Whether LLVM can actually *use* the triple (i.e. whether
/// the matching backend was compiled into this `aspc` binary) is validated
/// where that matters: [`crate::codegen::CodeGenerator::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetSpec {
    triple: String,
}

impl TargetSpec {
    /// The triple of the machine this compiler binary is running on *right
    /// now*, resolved through LLVM (`TargetMachine::get_default_triple()`)
    /// rather than `cfg!(target_os/arch)`. This is the default for
    /// `--target` and for every host-defaulting convenience constructor
    /// (`Preprocessor::new`, `CodeGenerator::new`).
    #[must_use]
    pub fn host() -> Self {
        Self::from_llvm_triple(&TargetMachine::get_default_triple())
    }

    /// Parse an arbitrary `--target`-style triple string, e.g.
    /// `x86_64-pc-windows-msvc`. This never fails: an unrecognised or
    /// malformed triple only surfaces as an error once LLVM is actually
    /// asked to resolve it, which is a codegen-time concern (a `lex`/`parse`
    /// run never touches LLVM, so it must not reject a triple that only
    /// codegen would reject).
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

    /// The triple string, e.g. `x86_64-unknown-linux-gnu`.
    #[must_use]
    pub fn triple(&self) -> &str {
        &self.triple
    }

    /// The [`TargetTriple`] LLVM's `Target`/`TargetMachine` setup needs.
    #[must_use]
    pub fn llvm_triple(&self) -> TargetTriple {
        TargetTriple::create(&self.triple)
    }

    /// The `OS_*` preprocessor define this target seeds, or `None` if its
    /// triple doesn't name a recognised OS. Matched by substring against the
    /// whole triple (case-insensitively) since LLVM triples spell each OS
    /// consistently — `linux`, `windows`, `darwin`/`macos` — regardless of
    /// which vendor/environment components surround it.
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

    /// The `ARCH_*` preprocessor define this target seeds, or `None` if the
    /// triple's leading architecture component isn't recognised.
    #[must_use]
    pub fn arch_define(&self) -> Option<&'static str> {
        let arch = self.triple.split('-').next().unwrap_or("");
        match arch.to_ascii_lowercase().as_str() {
            "x86_64" | "amd64" => Some("ARCH_X86_64"),
            "aarch64" | "arm64" => Some("ARCH_AARCH64"),
            _ => None,
        }
    }
}

impl std::fmt::Display for TargetSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.triple)
    }
}
