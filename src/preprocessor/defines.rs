//! `$define NAME` / `$define NAME <tokens>` / `$undefine NAME` — the define
//! table and identifier substitution.
//!
//! Defines are **object-like only**: a name maps to a replacement token
//! sequence (empty for flag defines like `$define DEBUG`). There are no
//! function-like macros — parameterised code generation is the metasystem
//! expansion hook's job, not the preprocessor's.
//!
//! ## Substitution
//!
//! Wherever a defined name appears as an [`TokenKind::Identifier`] token in
//! the output stream, the define's tokens are spliced in, each stamped with
//! the *use-site* position so downstream errors point at the usage.
//! Substitution is recursive, but a name may expand at most once per
//! expansion chain (self-reference guard, like C) — `$define X X + 1` emits
//! `X + 1` with the inner `X` left as a plain identifier. Token-level
//! substitution is word-boundary-safe by construction, and string literals
//! are single tokens, so they are never touched.
//!
//! ## Redefinition
//!
//! Redefinition is an error, uniformly: compiler-provided platform defines
//! and `-D` CLI defines count as prior defines exactly like a `$define`
//! directive. Files that want overridable defaults write the `$ifndef`
//! guard instead (Phase 2).
//!
//! ## Module scoping
//!
//! A `$define` is **module-scoped**, mirroring the non-transitive symbol
//! rule the parser enforces: its macro is visible only from its **defining
//! module** and any file that **directly `$import`s** that module. Two
//! *unrelated* modules may therefore `$define` the same name without
//! colliding — redefinition is an error only within one module (or against a
//! global define). Compiler-provided and `-D` CLI defines have **no** module
//! (they are global) and stay visible everywhere. Lookup and expansion go
//! through a [`ScopedDefines`] view carrying the querying file's module and
//! direct imports; the raw [`DefineTable::get`] / [`DefineTable::is_defined`]
//! accessors are module-unaware and only meaningful for global defines.

use std::collections::HashMap;

use crate::lexer::{tokenize, Position, Token, TokenKind};
use crate::target::TargetSpec;

use super::errors::PreprocessError;

/// Where a define came from. Drives redefinition diagnostics and makes the
/// uniform redefinition rule concrete: every origin counts as a prior define.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefineOrigin {
    /// Seeded by the compiler itself (platform / version defines).
    Builtin,
    /// Injected with `-D NAME[=VALUE]` on the command line.
    Cli,
    /// Declared by a `$define` directive at this position.
    Directive(Position),
}

impl DefineOrigin {
    /// Human-readable description of the *previous* definition site, spliced
    /// into redefinition error messages.
    fn describe(self) -> String {
        match self {
            DefineOrigin::Builtin => "previously provided by the compiler".to_string(),
            DefineOrigin::Cli => "previously defined on the command line via -D".to_string(),
            DefineOrigin::Directive(pos) => format!("previously defined at {pos}"),
        }
    }
}

/// A single define: its replacement tokens (empty for flag defines) and
/// where it was made.
#[derive(Debug, Clone)]
pub struct Define {
    pub tokens: Vec<Token>,
    pub origin: DefineOrigin,
}

/// The define table. Owned by the driver; conditionals and substitution
/// consult it through a module-scoped [`ScopedDefines`] view.
///
/// A name maps to a **list** of defines rather than a single one: because
/// `$define`s are module-scoped, the same name may be defined independently
/// in several unrelated modules, and each such define is a distinct entry
/// (each tagged, via its [`DefineOrigin::Directive`] position's `file_id`,
/// with the file — hence module — that declared it). Globals occupy a
/// single entry; the redefinition rule keeps every list free of colliding
/// entries.
#[derive(Debug, Default)]
pub struct DefineTable {
    map: HashMap<String, Vec<Define>>,
}

impl DefineTable {
    /// An empty table with no platform defines — mostly useful in tests.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// A table pre-seeded with the compiler-provided defines: `OS_LINUX` /
    /// `OS_WINDOWS` / `OS_MACOS` and `ARCH_X86_64` / `ARCH_AARCH64` as flag
    /// defines from `target` (the *compilation* target — see
    /// [`crate::target::TargetSpec`], never the compiler binary's own build
    /// host), plus `ASPECT_VERSION_MAJOR` / `ASPECT_VERSION_MINOR` as integer
    /// tokens from the crate version.
    #[must_use]
    pub fn with_platform_defines(target: &TargetSpec) -> Self {
        let mut table = Self::default();

        if let Some(name) = target.os_define() {
            table.insert_builtin_flag(name);
        }

        if let Some(name) = target.arch_define() {
            table.insert_builtin_flag(name);
        }

        table.insert_builtin_int(
            "ASPECT_VERSION_MAJOR",
            version_component(env!("CARGO_PKG_VERSION_MAJOR")),
        );
        table.insert_builtin_int(
            "ASPECT_VERSION_MINOR",
            version_component(env!("CARGO_PKG_VERSION_MINOR")),
        );
        table
    }

    fn insert_builtin_flag(&mut self, name: &str) {
        self.map.insert(
            name.to_string(),
            vec![Define {
                tokens: Vec::new(),
                origin: DefineOrigin::Builtin,
            }],
        );
    }

    fn insert_builtin_int(&mut self, name: &str, value: i64) {
        let token = Token::new(
            TokenKind::Integer(value),
            Position::new(0, 0),
            value.to_string(),
        );
        self.map.insert(
            name.to_string(),
            vec![Define {
                tokens: vec![token],
                origin: DefineOrigin::Builtin,
            }],
        );
    }

    /// True iff *any* define named `name` exists — **module-unaware**: it
    /// ignores visibility and so is only meaningful for global defines (a
    /// module-scoped `$ifdef`/`defined(NAME)` goes through [`ScopedDefines`]).
    #[must_use]
    pub fn is_defined(&self, name: &str) -> bool {
        self.map.contains_key(name)
    }

    /// The first define named `name`, if any — **module-unaware** (see
    /// [`DefineTable::is_defined`]); use [`ScopedDefines::get`] for scoped
    /// lookup.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Define> {
        self.map.get(name).and_then(|defs| defs.first())
    }

    /// A module-unaware view of this table: only global/`-D` defines and
    /// defines whose home module is the anonymous root `""` are visible.
    /// Test-only — real evaluation always runs with a module-scoped view.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn global_view(&self) -> ScopedDefines<'_> {
        ScopedDefines::new(self, "", &[], &[])
    }

    /// Register `name` → `tokens`, tagging the entry (via `origin`) with the
    /// module that declared it. Redefinition is an error only when the new
    /// define collides with a **visible-everywhere** global or with another
    /// define **in the same module**; the same name in two unrelated modules
    /// is not a collision. `file_modules` resolves each entry's home module
    /// from the `file_id` in its origin position.
    ///
    /// # Errors
    /// [`PreprocessError::Redefinition`] when a `$define` directive collides;
    /// [`PreprocessError::CliRedefinition`] when the collision happens while
    /// seeding `-D` defines.
    pub fn define(
        &mut self,
        name: String,
        tokens: Vec<Token>,
        origin: DefineOrigin,
        file_modules: &[Option<String>],
    ) -> Result<(), PreprocessError> {
        let new_home = home_module(origin, file_modules);
        if let Some(existing) = self.map.get(&name) {
            for def in existing {
                // A global on either side always collides (a module may
                // neither shadow nor duplicate a compiler/CLI define); two
                // module-scoped macros collide only within one module.
                let collides = match (new_home, home_module(def.origin, file_modules)) {
                    (Some(a), Some(b)) => a == b,
                    _ => true,
                };
                if collides {
                    let previous = def.origin.describe();
                    return Err(match origin {
                        DefineOrigin::Directive(pos) => PreprocessError::Redefinition {
                            name,
                            previous,
                            pos,
                        },
                        DefineOrigin::Builtin | DefineOrigin::Cli => {
                            PreprocessError::CliRedefinition { name, previous }
                        }
                    });
                }
            }
        }
        self.map.entry(name).or_default().push(Define { tokens, origin });
        Ok(())
    }

    /// Remove the current file's own module's `$define` of `name`; a no-op
    /// when it isn't defined there. Only the module of `this_file` is
    /// touched — an imported module's define and every global are left in
    /// place (a `$undefine` in one file must not unbind another module's or
    /// another file's macro). `file_modules` resolves home modules.
    pub fn undefine(&mut self, name: &str, this_file: u32, file_modules: &[Option<String>]) {
        let this_module = module_of(file_modules, this_file);
        let Some(defs) = self.map.get_mut(name) else {
            return;
        };
        defs.retain(|def| match def.origin {
            DefineOrigin::Directive(pos) => module_of(file_modules, pos.file_id) != this_module,
            DefineOrigin::Builtin | DefineOrigin::Cli => true,
        });
        if defs.is_empty() {
            self.map.remove(name);
        }
    }

    /// Parse a `-D NAME` / `-D NAME=VALUE` spec and register it. The value
    /// (everything after the first `=`) is lexed into a token sequence with
    /// the same scanner source files go through.
    ///
    /// # Errors
    /// [`PreprocessError::InvalidCliDefine`] for a malformed name or an
    /// unlexable value; redefinition errors as per [`DefineTable::define`].
    pub fn add_cli_define(&mut self, spec: &str) -> Result<(), PreprocessError> {
        let (name, value) = match spec.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (spec, None),
        };
        if !is_identifier(name) {
            return Err(PreprocessError::InvalidCliDefine {
                spec: spec.to_string(),
                reason: "the name must be an identifier (and not a keyword or type)".to_string(),
            });
        }
        let tokens = match value {
            Some(value) => lex_define_value(spec, value)?,
            None => Vec::new(),
        };
        // CLI defines are global (no home module), so `file_modules` is
        // irrelevant to their collision check.
        self.define(name.to_string(), tokens, DefineOrigin::Cli, &[])
    }
}

/// A single file's scoped view of the define table for lookup, `$if`/`$ifdef`
/// evaluation, and identifier substitution. It bundles the shared table with
/// the querying file's own module and direct imports (plus the file→module
/// map used to resolve each define's home module), and applies the
/// module-visibility rule described on this module: globals are visible
/// everywhere, a `$define` macro only from its home module or a direct
/// importer of that module.
pub(crate) struct ScopedDefines<'a> {
    table: &'a DefineTable,
    /// The querying file's module (`""` for the anonymous root).
    module: &'a str,
    /// Modules the querying file has directly `$import`ed so far.
    imports: &'a [String],
    /// file_id → declared module, resolving a define's home from the file_id
    /// embedded in its [`DefineOrigin::Directive`] position.
    file_modules: &'a [Option<String>],
}

impl<'a> ScopedDefines<'a> {
    pub(crate) fn new(
        table: &'a DefineTable,
        module: &'a str,
        imports: &'a [String],
        file_modules: &'a [Option<String>],
    ) -> Self {
        Self {
            table,
            module,
            imports,
            file_modules,
        }
    }

    /// Whether `define` is visible from this file: globals always are; a
    /// module-scoped macro only from its home module or a direct importer.
    fn sees(&self, define: &Define) -> bool {
        match define.origin {
            DefineOrigin::Builtin | DefineOrigin::Cli => true,
            DefineOrigin::Directive(pos) => {
                let home = module_of(self.file_modules, pos.file_id);
                home == self.module || self.imports.iter().any(|m| m.as_str() == home)
            }
        }
    }

    /// True iff a *visible* define named `name` exists.
    pub(crate) fn is_defined(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// The visible define named `name`, if any. (A name is either a single
    /// global or a set of module-scoped entries with distinct home modules,
    /// so at most one entry is visible from any given file.)
    pub(crate) fn get(&self, name: &str) -> Option<&'a Define> {
        self.table.map.get(name)?.iter().find(|def| self.sees(def))
    }

    /// Append `token` to `out`, splicing in its define expansion (recursively)
    /// when it is an identifier bound to a *visible* define. Spliced tokens
    /// are re-stamped with the use-site position. Flag defines expand to
    /// nothing.
    pub(crate) fn expand_into(&self, out: &mut Vec<Token>, token: &Token) {
        let mut active = Vec::new();
        self.expand_token(out, token, &mut active);
    }

    /// Recursive step for [`ScopedDefines::expand_into`]. `active` is the
    /// expansion chain: a name already on it does not expand again
    /// (self-reference guard), so mutually-recursive defines terminate with
    /// the inner name left verbatim.
    fn expand_token(&self, out: &mut Vec<Token>, token: &Token, active: &mut Vec<String>) {
        if let TokenKind::Identifier(name) = &token.kind
            && !active.iter().any(|n| n == name)
            && let Some(define) = self.get(name)
        {
            active.push(name.clone());
            for replacement in &define.tokens {
                let mut stamped = replacement.clone();
                stamped.pos = token.pos;
                self.expand_token(out, &stamped, active);
            }
            active.pop();
            return;
        }
        out.push(token.clone());
    }
}

/// The module a file belongs to — `""` for the anonymous root, a file that
/// declared no `$module`, or an unknown file id.
fn module_of(file_modules: &[Option<String>], file_id: u32) -> &str {
    file_modules
        .get(file_id as usize)
        .and_then(Option::as_deref)
        .unwrap_or("")
}

/// A define's home module: `None` for globals (compiler/CLI), otherwise the
/// module of the file that declared it (resolved from its origin's file id).
fn home_module(origin: DefineOrigin, file_modules: &[Option<String>]) -> Option<&str> {
    match origin {
        DefineOrigin::Builtin | DefineOrigin::Cli => None,
        DefineOrigin::Directive(pos) => Some(module_of(file_modules, pos.file_id)),
    }
}

/// Parse one Cargo version component (`env!` hands them over as strings).
fn version_component(s: &str) -> i64 {
    s.parse()
        .expect("Cargo version components are always integers")
}

/// True iff `name` lexes to exactly one identifier token — this rejects
/// keywords (`true`), built-in types (`u8`), and anything non-identifier,
/// none of which could ever be substituted for.
fn is_identifier(name: &str) -> bool {
    matches!(
        tokenize(name.to_string()).as_deref(),
        Ok([
            Token {
                kind: TokenKind::Identifier(_),
                ..
            },
            Token {
                kind: TokenKind::Eof,
                ..
            }
        ])
    )
}

/// Lex a `-D NAME=VALUE` value into its replacement token sequence.
fn lex_define_value(spec: &str, value: &str) -> Result<Vec<Token>, PreprocessError> {
    let mut tokens = tokenize(value.to_string()).map_err(|e| PreprocessError::InvalidCliDefine {
        spec: spec.to_string(),
        reason: e.to_string(),
    })?;
    tokens.retain(|t| !matches!(t.kind, TokenKind::Eof));
    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::super::{preprocess_str, preprocess_str_with, Preprocessor};
    use super::*;

    /// Strip the trailing Eof and any Newline tokens so assertions can focus
    /// on the interesting kinds.
    fn kinds(tokens: Vec<Token>) -> Vec<TokenKind> {
        tokens
            .into_iter()
            .map(|t| t.kind)
            .filter(|k| !matches!(k, TokenKind::Newline | TokenKind::Eof))
            .collect()
    }

    #[test]
    fn define_and_undefine_round_trip() {
        let tokens = preprocess_str("$define MAX 7\nMAX\n$undefine MAX\nMAX\n").unwrap();
        assert_eq!(
            kinds(tokens),
            vec![
                TokenKind::Integer(7),
                TokenKind::Identifier("MAX".to_string()),
            ]
        );
    }

    #[test]
    fn undefine_of_undefined_name_is_a_noop() {
        assert!(preprocess_str("$undefine NEVER_DEFINED\n").is_ok());
    }

    #[test]
    fn redefinition_is_an_error() {
        let err = preprocess_str("$define MAX 1\n$define MAX 2\n").unwrap_err();
        assert!(matches!(
            &err,
            PreprocessError::Redefinition { name, .. } if name == "MAX"
        ));
        // The message names the prior definition site (line 1).
        assert!(err.to_string().contains("previously defined at 1:"));
    }

    #[test]
    fn cli_define_counts_as_prior_define() {
        let mut pp = Preprocessor::new();
        pp.add_cli_define("MAX=1").unwrap();
        let err = preprocess_str_with(pp, "$define MAX 2\n").unwrap_err();
        assert!(matches!(&err, PreprocessError::Redefinition { .. }));
        assert!(err.to_string().contains("command line"));
    }

    #[test]
    fn colliding_cli_defines_are_an_error() {
        let mut table = DefineTable::new();
        table.add_cli_define("MAX=1").unwrap();
        let err = table.add_cli_define("MAX=2").unwrap_err();
        assert!(matches!(&err, PreprocessError::CliRedefinition { name, .. } if name == "MAX"));
    }

    #[test]
    fn substitution_is_token_exact() {
        // Word-boundary correctness for free: `MAXIMUM` and `MAX_SIZE` are
        // different identifier tokens and must not be rewritten.
        let tokens = preprocess_str("$define MAX 9\nMAX MAXIMUM MAX_SIZE\n").unwrap();
        assert_eq!(
            kinds(tokens),
            vec![
                TokenKind::Integer(9),
                TokenKind::Identifier("MAXIMUM".to_string()),
                TokenKind::Identifier("MAX_SIZE".to_string()),
            ]
        );
    }

    #[test]
    fn string_literals_are_never_substituted() {
        let tokens = preprocess_str("$define MAX 9\n\"MAX\"\n").unwrap();
        assert_eq!(
            kinds(tokens),
            vec![TokenKind::StringLiteral("MAX".to_string())]
        );
    }

    #[test]
    fn array_size_substitution() {
        // The scanner only folds `T[N]` for literal N, so `u8[MAX_SIZE]`
        // arrives unfolded (`u8` `[` `MAX_SIZE` `]`) and substitution turns
        // it into `u8` `[` `1024` `]` for the parser's type-suffix rule.
        let tokens = preprocess_str("$define MAX_SIZE 1024\nu8[MAX_SIZE] buf\n").unwrap();
        let kinds = kinds(tokens);
        assert_eq!(kinds.len(), 5);
        assert!(
            matches!(&kinds[0], TokenKind::LangType(t) if t.array_size.is_none()),
            "scanner must leave `u8[MAX_SIZE]` unfolded, got {kinds:?}"
        );
        assert_eq!(kinds[1], TokenKind::OpenBracket);
        assert_eq!(kinds[2], TokenKind::Integer(1024));
        assert_eq!(kinds[3], TokenKind::CloseBracket);
        assert_eq!(kinds[4], TokenKind::Identifier("buf".to_string()));
    }

    #[test]
    fn recursive_defines_expand_through_the_chain() {
        let tokens = preprocess_str("$define ONE 1\n$define TWO ONE + ONE\nTWO\n").unwrap();
        assert_eq!(
            kinds(tokens),
            vec![
                TokenKind::Integer(1),
                TokenKind::Plus,
                TokenKind::Integer(1),
            ]
        );
    }

    #[test]
    fn self_referential_define_expands_once_per_chain() {
        let tokens = preprocess_str("$define X X + 1\nX\n").unwrap();
        assert_eq!(
            kinds(tokens),
            vec![
                TokenKind::Identifier("X".to_string()),
                TokenKind::Plus,
                TokenKind::Integer(1),
            ]
        );
    }

    #[test]
    fn mutually_recursive_defines_terminate() {
        let tokens = preprocess_str("$define A B\n$define B A\nA\n").unwrap();
        // A → B → A, where the inner A is guarded and stays an identifier.
        assert_eq!(kinds(tokens), vec![TokenKind::Identifier("A".to_string())]);
    }

    #[test]
    fn flag_define_substitutes_to_nothing() {
        let tokens = preprocess_str("$define FLAG\nFLAG 5\n").unwrap();
        assert_eq!(kinds(tokens), vec![TokenKind::Integer(5)]);
    }

    #[test]
    fn substituted_tokens_carry_the_use_site_position() {
        let tokens = preprocess_str("$define MAX 9\n\nMAX\n").unwrap();
        let spliced = tokens
            .iter()
            .find(|t| matches!(t.kind, TokenKind::Integer(9)))
            .expect("the define must have expanded");
        assert_eq!(spliced.pos.line, 3);
        assert_eq!(spliced.pos.column, 1);
    }

    #[test]
    fn cli_define_value_is_lexed_into_tokens() {
        let mut table = DefineTable::new();
        table.add_cli_define("N=1 + 2").unwrap();
        let define = table.get("N").unwrap();
        assert_eq!(define.tokens.len(), 3);
        assert_eq!(define.origin, DefineOrigin::Cli);
    }

    #[test]
    fn cli_define_rejects_non_identifier_names() {
        let mut table = DefineTable::new();
        for spec in ["9BAD", "", "a b", "u8", "true", "NAME NAME=1"] {
            assert!(
                matches!(
                    table.add_cli_define(spec),
                    Err(PreprocessError::InvalidCliDefine { .. })
                ),
                "spec '{spec}' must be rejected"
            );
        }
    }

    #[test]
    fn platform_defines_are_seeded_from_the_host_target() {
        let table = DefineTable::with_platform_defines(&TargetSpec::host());
        if cfg!(target_os = "linux") {
            assert!(table.is_defined("OS_LINUX"));
            assert!(!table.is_defined("OS_WINDOWS"));
        }
        if cfg!(target_arch = "x86_64") {
            assert!(table.is_defined("ARCH_X86_64"));
        }
        let major = table.get("ASPECT_VERSION_MAJOR").unwrap();
        let expected = version_component(env!("CARGO_PKG_VERSION_MAJOR"));
        assert!(matches!(
            major.tokens.as_slice(),
            [Token { kind: TokenKind::Integer(v), .. }] if *v == expected
        ));
        assert!(table.is_defined("ASPECT_VERSION_MINOR"));
    }

    #[test]
    fn platform_defines_follow_an_explicit_target_triple_not_the_build_host() {
        // `x86_64-pc-windows-msvc` must seed OS_WINDOWS (and never
        // OS_LINUX) even though these unit tests themselves run on Linux —
        // the whole point of `TargetSpec` is that the compilation target is
        // independent of the compiler binary's own build/run host.
        let windows =
            DefineTable::with_platform_defines(&TargetSpec::parse("x86_64-pc-windows-msvc"));
        assert!(windows.is_defined("OS_WINDOWS"));
        assert!(!windows.is_defined("OS_LINUX"));
        assert!(!windows.is_defined("OS_MACOS"));
        assert!(windows.is_defined("ARCH_X86_64"));

        let linux =
            DefineTable::with_platform_defines(&TargetSpec::parse("x86_64-unknown-linux-gnu"));
        assert!(linux.is_defined("OS_LINUX"));
        assert!(!linux.is_defined("OS_WINDOWS"));
        assert!(linux.is_defined("ARCH_X86_64"));

        let mac = DefineTable::with_platform_defines(&TargetSpec::parse("aarch64-apple-darwin"));
        assert!(mac.is_defined("OS_MACOS"));
        assert!(mac.is_defined("ARCH_AARCH64"));
        assert!(!mac.is_defined("ARCH_X86_64"));
    }

    #[test]
    fn platform_defines_are_absent_for_an_unrecognised_triple() {
        // A triple naming neither a known OS nor a known arch simply seeds
        // no OS_*/ARCH_* define — unrecognised is not an error at this
        // layer (`TargetSpec::parse` never fails); only codegen rejects a
        // triple LLVM itself can't resolve.
        let table = DefineTable::with_platform_defines(&TargetSpec::parse("riscv64-unknown-none"));
        assert!(!table.is_defined("OS_LINUX"));
        assert!(!table.is_defined("OS_WINDOWS"));
        assert!(!table.is_defined("OS_MACOS"));
        assert!(!table.is_defined("ARCH_X86_64"));
        assert!(!table.is_defined("ARCH_AARCH64"));
        // Version defines are unconditional.
        assert!(table.is_defined("ASPECT_VERSION_MAJOR"));
    }
}
