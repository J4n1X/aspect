//! `$module <path>` / `$import <path>` — module identity and loading.
//!
//! v1 modules are **load units, not namespaces**: an import splices the
//! module's files into the token stream, and
//! all symbols land in the flat global table. What this stage adds is
//! *identity*: which module each file belongs to, and which modules each
//! module directly imports. The later visibility phase enforces
//! non-transitive imports on top of that data (see
//! `doc/plans/Preprocessor-Infrastructure.md` § Modules).
//!
//! ## Path grammar
//!
//! `segment('/'segment)*` — segments are bare identifier tokens, no quotes,
//! nothing after the path. `$module std/math` and `$import std/math` share
//! the grammar ([`parse_module_path`]).
//!
//! ## Resolution: convention + verification
//!
//! `$import <path>` searches each `-I` root **in flag order** for exactly
//! two shapes ([`resolve_module_files`]):
//!
//! - **file form** — `<root>/<path>.ap`, a single-file module;
//! - **directory form** — every `.ap` file *directly* inside
//!   `<root>/<path>/` (non-recursive; subdirectories are submodules and are
//!   imported explicitly), loaded in sorted filename order.
//!
//! The first root offering either form wins; a root offering *both* is an
//! error; nothing in any root is an error listing every candidate tried.
//! There is deliberately **no tree scanning** — the import path is the
//! location contract.
//!
//! `$module` is then the *verified identity*: every file loaded for an
//! import must declare exactly the imported path, or the import is a hard
//! error naming the file, its declaration (or the lack of one), and the
//! import that pulled it in.
//!
//! ## Import-once
//!
//! Importing the same module twice — directly or diamond-shaped — loads it
//! once; the module registry is marked *before* the module's files are
//! processed, so import cycles (A imports B imports A) terminate. Later
//! imports of an already-loaded module still record the dependency edge for
//! the visibility phase. Canonical-path file dedup
//! ([`Preprocessor::process_file`]) remains as a second guard underneath.

use std::fs;
use std::path::PathBuf;

use crate::lexer::{Position, Token, TokenKind};

use super::{PreprocessError, Preprocessor};

/// Handle `$module <path>`: record the current file's module identity.
///
/// At most one `$module` per file, and it must appear before any
/// non-directive token of the file (blank lines, comments, and other
/// directives don't count as content).
pub(crate) fn handle_module(
    pp: &mut Preprocessor,
    rest: &[Token],
    pos: Position,
) -> Result<(), PreprocessError> {
    let module = parse_module_path("module", rest, pos)?;
    let ctx = pp
        .file_stack
        .last_mut()
        .expect("a file context is always active while tokens are processed");
    if let Some(previous) = ctx.module_pos {
        return Err(PreprocessError::DuplicateModuleDirective { pos, previous });
    }
    if ctx.saw_content {
        return Err(PreprocessError::ModuleAfterContent(pos));
    }
    ctx.module_pos = Some(pos);
    let file_id = ctx.file_id;
    pp.file_modules[file_id as usize] = Some(module);
    Ok(())
}

/// Handle `$import <path>`: record the dependency edge, and — the first
/// time this module path is imported anywhere in the compilation — resolve
/// it against the `-I` roots, load every file of the module, and verify
/// each file's `$module` declaration against the import path.
pub(crate) fn handle_import(
    pp: &mut Preprocessor,
    rest: &[Token],
    pos: Position,
) -> Result<(), PreprocessError> {
    let module = parse_module_path("import", rest, pos)?;

    // The direct edge belongs to the *importing file*; it accrues to that
    // file's module when the file finishes processing (the file's own
    // `$module` may legally appear after this import).
    let ctx = pp
        .file_stack
        .last_mut()
        .expect("a file context is always active while tokens are processed");
    if !ctx.imports.contains(&module) {
        ctx.imports.push(module.clone());
    }

    // Import-once by module identity. Marked BEFORE the module's files are
    // processed so that cycles (A imports B imports A) terminate: the inner
    // import finds the module already registered and only records its edge.
    if !pp.imported.insert(module.clone()) {
        return Ok(());
    }

    let files = resolve_module_files(&pp.include_dirs, &module, pos)?;
    for file in files {
        let file_id = pp.process_file(&file)?;
        let declared = pp.file_modules[file_id as usize].clone();
        if declared.as_deref() != Some(module.as_str()) {
            return Err(PreprocessError::ModuleDeclarationMismatch {
                module,
                file: pp.files[file_id as usize].clone(),
                declared,
                pos,
            });
        }
    }
    Ok(())
}

/// Parse a module path (`segment('/'segment)*`) from the directive line's
/// remaining tokens. Returns the canonical `a/b/c` string form.
pub(crate) fn parse_module_path(
    directive: &'static str,
    rest: &[Token],
    pos: Position,
) -> Result<String, PreprocessError> {
    let malformed = |detail: String, pos: Position| PreprocessError::MalformedModulePath {
        directive,
        detail,
        pos,
    };

    let mut path = String::new();
    let mut expect_segment = true;
    for token in rest {
        match &token.kind {
            TokenKind::Identifier(name) if expect_segment => {
                path.push_str(name);
                expect_segment = false;
            }
            TokenKind::Slash if !expect_segment => {
                path.push('/');
                expect_segment = true;
            }
            TokenKind::StringLiteral(_) => {
                return Err(malformed(
                    "quoted paths are not allowed".to_string(),
                    token.pos,
                ));
            }
            kind if expect_segment => {
                return Err(malformed(
                    format!("expected an identifier segment, found `{kind}`"),
                    token.pos,
                ));
            }
            kind => {
                return Err(malformed(
                    format!("unexpected `{kind}` after the path"),
                    token.pos,
                ));
            }
        }
    }
    if path.is_empty() {
        return Err(malformed("missing module path".to_string(), pos));
    }
    if expect_segment {
        // The line ended on a `/` — rest is never empty here.
        let last = rest.last().expect("a trailing `/` implies tokens exist");
        return Err(malformed("trailing `/`".to_string(), last.pos));
    }
    Ok(path)
}

/// Resolve an import path to the on-disk files of the module, per `-I` root
/// in flag order (see the module docs for the file/directory forms). The
/// returned list is non-empty and deterministic (directory-form files are
/// sorted by name).
pub(crate) fn resolve_module_files(
    roots: &[PathBuf],
    module: &str,
    pos: Position,
) -> Result<Vec<PathBuf>, PreprocessError> {
    let relative: PathBuf = module.split('/').collect();
    let mut candidates = Vec::new();
    for root in roots {
        let dir_form = root.join(&relative);
        let file_form = dir_form.with_extension("ap");
        let file_exists = file_form.is_file();
        let dir_files = list_module_dir(&dir_form)?;
        match (file_exists, dir_files.is_empty()) {
            (true, false) => {
                return Err(PreprocessError::AmbiguousModuleForms {
                    module: module.to_string(),
                    file: file_form,
                    dir: dir_form,
                    pos,
                });
            }
            (true, true) => return Ok(vec![file_form]),
            (false, false) => return Ok(dir_files),
            (false, true) => {
                candidates.push(file_form);
                candidates.push(dir_form);
            }
        }
    }
    Err(PreprocessError::ModuleNotFound {
        module: module.to_string(),
        candidates,
        pos,
    })
}

/// The `.ap` files directly inside `dir`, sorted by name. Empty when
/// `dir` doesn't exist or is not a directory — a directory without any
/// `.ap` files directly inside it does *not* constitute the directory
/// form (resolution falls through to the next root).
fn list_module_dir(dir: &PathBuf) -> Result<Vec<PathBuf>, PreprocessError> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(dir).map_err(|e| PreprocessError::Unreadable {
        path: dir.clone(),
        reason: e.to_string(),
    })?;
    let mut files = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|e| PreprocessError::Unreadable {
                path: dir.clone(),
                reason: e.to_string(),
            })?
            .path();
        if path.is_file() && path.extension().is_some_and(|ext| ext == "ap") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::super::{preprocess_str, preprocess_str_full, PreprocessedSource, Preprocessor};
    use super::*;

    /// The checked-in fixture tree (primary `-I` root) used by these tests
    /// and mirrored by the `tests/programs/module_*.ap` integration tests.
    fn fixture_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("modules")
    }

    /// A second fixture root, for search-order tests.
    fn alt_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("modules_alt")
    }

    /// Preprocess an in-memory entry file against the given search roots.
    fn run(source: &str, roots: &[PathBuf]) -> Result<PreprocessedSource, PreprocessError> {
        let mut pp = Preprocessor::new();
        for root in roots {
            pp.add_include_dir(root.clone());
        }
        preprocess_str_full(pp, source)
    }

    /// The imports recorded for `module`, panicking when the module has no
    /// entry at all.
    fn imports_of<'a>(src: &'a PreprocessedSource, module: &str) -> &'a [String] {
        src.imports
            .get(module)
            .unwrap_or_else(|| panic!("module `{module}` has no imports entry"))
    }

    // ── Path grammar ────────────────────────────────────────────────────

    #[test]
    fn empty_module_path_is_an_error() {
        for directive in ["module", "import"] {
            let err = preprocess_str(&format!("${directive}\n")).unwrap_err();
            assert!(
                matches!(
                    &err,
                    PreprocessError::MalformedModulePath { directive: d, detail, .. }
                        if *d == directive && detail.contains("missing")
                ),
                "`${directive}` without a path must be malformed, got {err:?}"
            );
        }
    }

    #[test]
    fn trailing_tokens_after_the_path_are_an_error() {
        let err = preprocess_str("$module std/math extra\n").unwrap_err();
        assert!(matches!(
            &err,
            PreprocessError::MalformedModulePath { detail, .. } if detail.contains("unexpected")
        ));
    }

    #[test]
    fn non_identifier_segment_is_an_error() {
        for source in ["$module std/42\n", "$module std/if\n", "$module u8\n"] {
            let err = preprocess_str(source).unwrap_err();
            assert!(
                matches!(
                    &err,
                    PreprocessError::MalformedModulePath { detail, .. }
                        if detail.contains("expected an identifier segment")
                ),
                "{source:?} must be malformed, got {err:?}"
            );
        }
    }

    #[test]
    fn quoted_module_path_is_rejected() {
        let err = preprocess_str("$import \"std/math\"\n").unwrap_err();
        assert!(matches!(
            &err,
            PreprocessError::MalformedModulePath { directive: "import", detail, .. }
                if detail.contains("quoted")
        ));
    }

    #[test]
    fn trailing_and_doubled_slashes_are_errors() {
        assert!(matches!(
            preprocess_str("$module std/\n").unwrap_err(),
            PreprocessError::MalformedModulePath { detail, .. } if detail.contains("trailing `/`")
        ));
        assert!(matches!(
            preprocess_str("$module std//math\n").unwrap_err(),
            PreprocessError::MalformedModulePath { detail, .. }
                if detail.contains("expected an identifier segment")
        ));
    }

    // ── `$module` placement ─────────────────────────────────────────────

    #[test]
    fn second_module_directive_is_an_error() {
        let err = preprocess_str("$module app\n$module other\n").unwrap_err();
        let PreprocessError::DuplicateModuleDirective { pos, previous } = err else {
            panic!("expected DuplicateModuleDirective, got {err:?}");
        };
        assert_eq!(previous.line, 1);
        assert_eq!(pos.line, 2);
    }

    #[test]
    fn module_after_content_is_an_error() {
        let err = preprocess_str("i32 x = 0\n$module app\n").unwrap_err();
        assert!(matches!(err, PreprocessError::ModuleAfterContent(_)));
    }

    #[test]
    fn module_after_blank_lines_and_directives_is_fine() {
        // Newlines and other directives don't count as content.
        let src = run("\n\n$define FLAG\n$module app\n", &[]).unwrap();
        assert_eq!(src.modules, vec![(0, "app".to_string())]);
    }

    // ── Resolution ──────────────────────────────────────────────────────

    #[test]
    fn file_form_resolution() {
        let src = run("$import mathlib\n", &[fixture_root()]).unwrap();
        assert_eq!(src.files.len(), 2, "entry + one module file");
        assert!(src.files[1].ends_with("modules/mathlib.ap"));
        assert_eq!(
            src.modules,
            vec![(0, String::new()), (1, "mathlib".to_string())]
        );
        // The module's tokens really are in the stream.
        assert!(src
            .tokens
            .iter()
            .any(|t| matches!(&t.kind, TokenKind::Identifier(n) if n == "ml_add")));
    }

    #[test]
    fn directory_form_loads_every_file_sorted() {
        let src = run("$import strutil\n", &[fixture_root()]).unwrap();
        assert_eq!(src.files.len(), 3, "entry + two module files");
        assert!(src.files[1].ends_with("modules/strutil/first.ap"));
        assert!(src.files[2].ends_with("modules/strutil/second.ap"));
        assert_eq!(
            src.modules,
            vec![
                (0, String::new()),
                (1, "strutil".to_string()),
                (2, "strutil".to_string()),
            ]
        );
    }

    #[test]
    fn first_root_offering_a_form_wins() {
        // `shadow` exists in both roots; flag order decides.
        let src = run("$import shadow\n", &[fixture_root(), alt_root()]).unwrap();
        assert!(src.files[1].ends_with("modules/shadow.ap"));

        let src = run("$import shadow\n", &[alt_root(), fixture_root()]).unwrap();
        assert!(src.files[1].ends_with("modules_alt/shadow.ap"));
    }

    #[test]
    fn later_roots_are_searched_when_earlier_ones_miss() {
        // `onlyalt` exists only in the second root.
        let src = run("$import onlyalt\n", &[fixture_root(), alt_root()]).unwrap();
        assert!(src.files[1].ends_with("modules_alt/onlyalt.ap"));
    }

    #[test]
    fn both_forms_in_one_root_is_an_error() {
        let err = run("$import dual\n", &[fixture_root()]).unwrap_err();
        let PreprocessError::AmbiguousModuleForms { module, file, dir, .. } = &err else {
            panic!("expected AmbiguousModuleForms, got {err:?}");
        };
        assert_eq!(module, "dual");
        assert!(file.ends_with("modules/dual.ap"));
        assert!(dir.ends_with("modules/dual"));
    }

    #[test]
    fn module_not_found_lists_every_candidate() {
        let err = run("$import no/such/module\n", &[fixture_root(), alt_root()]).unwrap_err();
        let PreprocessError::ModuleNotFound { module, candidates, .. } = &err else {
            panic!("expected ModuleNotFound, got {err:?}");
        };
        assert_eq!(module, "no/such/module");
        // File form + directory form, per root.
        assert_eq!(candidates.len(), 4);
        let message = err.to_string();
        assert!(message.contains("no/such/module.ap"));
        assert!(message.contains("modules_alt"));
    }

    #[test]
    fn import_without_search_roots_says_so() {
        let err = run("$import anything\n", &[]).unwrap_err();
        assert!(matches!(
            &err,
            PreprocessError::ModuleNotFound { candidates, .. } if candidates.is_empty()
        ));
        assert!(err.to_string().contains("no module search roots"));
    }

    // ── `$module` verification ──────────────────────────────────────────

    #[test]
    fn module_declaration_mismatch_is_an_error() {
        let err = run("$import badmod\n", &[fixture_root()]).unwrap_err();
        let PreprocessError::ModuleDeclarationMismatch {
            module,
            file,
            declared,
            ..
        } = &err
        else {
            panic!("expected ModuleDeclarationMismatch, got {err:?}");
        };
        assert_eq!(module, "badmod");
        assert!(file.ends_with("modules/badmod.ap"));
        assert_eq!(declared.as_deref(), Some("actually_other"));
        let message = err.to_string();
        assert!(message.contains("badmod.ap"));
        assert!(message.contains("declares module `actually_other`"));
    }

    #[test]
    fn missing_module_declaration_is_an_error() {
        let err = run("$import nodecl\n", &[fixture_root()]).unwrap_err();
        assert!(matches!(
            &err,
            PreprocessError::ModuleDeclarationMismatch { declared: None, .. }
        ));
        assert!(err.to_string().contains("declares no `$module`"));
    }

    // ── Import-once / cycles / the imports map ──────────────────────────

    #[test]
    fn diamond_import_loads_the_base_module_once() {
        let src = run("$import left\n$import right\n", &[fixture_root()]).unwrap();
        let base_loads = src
            .files
            .iter()
            .filter(|f| f.ends_with("modules/base.ap"))
            .count();
        assert_eq!(base_loads, 1, "diamond must load `base` exactly once");
        // Direct edges only: the anonymous root imports left+right, each of
        // which imports base; base imports nothing but still has an entry.
        assert_eq!(imports_of(&src, ""), ["left", "right"]);
        assert_eq!(imports_of(&src, "left"), ["base"]);
        assert_eq!(imports_of(&src, "right"), ["base"]);
        assert!(imports_of(&src, "base").is_empty());
    }

    #[test]
    fn import_cycles_terminate() {
        let src = run("$import cyc_a\n", &[fixture_root()]).unwrap();
        assert_eq!(src.files.len(), 3, "entry + one file per cycle member");
        assert_eq!(imports_of(&src, "cyc_a"), ["cyc_b"]);
        assert_eq!(imports_of(&src, "cyc_b"), ["cyc_a"]);
    }

    #[test]
    fn repeated_import_is_loaded_once_and_deduped_in_the_edge_list() {
        let src = run("$import mathlib\n$import mathlib\n", &[fixture_root()]).unwrap();
        assert_eq!(src.files.len(), 2);
        assert_eq!(imports_of(&src, ""), ["mathlib"]);
    }

    #[test]
    fn entry_file_module_declaration_owns_its_imports() {
        let src = run("$module app\n$import mathlib\n", &[fixture_root()]).unwrap();
        assert_eq!(src.modules[0], (0, "app".to_string()));
        assert_eq!(imports_of(&src, "app"), ["mathlib"]);
        assert!(
            !src.imports.contains_key(""),
            "no anonymous entry when every file declares a module"
        );
    }

    #[test]
    fn import_before_module_declaration_still_accrues_to_the_module() {
        // `$module` may legally follow other directives, including imports;
        // the edge belongs to the finally-declared module.
        let src = run("$import mathlib\n$module app\n", &[fixture_root()]).unwrap();
        assert_eq!(imports_of(&src, "app"), ["mathlib"]);
    }

    #[test]
    fn search_roots_are_reported() {
        let src = run("$import mathlib\n", &[fixture_root()]).unwrap();
        assert_eq!(src.search_roots, vec![fixture_root()]);
    }

    // ── Define scoping ──────────────────────────────────────────────────

    /// True iff any emitted token is the identifier `name` (i.e. it was left
    /// unexpanded), false when every occurrence was substituted away.
    fn has_identifier(src: &PreprocessedSource, name: &str) -> bool {
        src.tokens
            .iter()
            .any(|t| matches!(&t.kind, TokenKind::Identifier(n) if n == name))
    }

    #[test]
    fn a_directly_imported_modules_define_is_visible() {
        // `macdefiner` defines MD_MACRO; a file that imports it sees it, so
        // the trailing `MD_MACRO` is substituted (to 40) rather than surviving
        // as a bare identifier.
        let src = run("$import macdefiner\nMD_MACRO\n", &[fixture_root()]).unwrap();
        assert!(
            src.tokens
                .iter()
                .any(|t| matches!(t.kind, TokenKind::Integer(40))),
            "MD_MACRO must expand to 40 for a direct importer"
        );
        assert!(
            !has_identifier(&src, "MD_MACRO"),
            "no unexpanded MD_MACRO should remain"
        );
    }

    #[test]
    fn same_name_defines_in_unrelated_modules_do_not_collide() {
        // `macdefiner` and `othermod` both `$define MD_MACRO`. Module-scoped
        // defines make this legal — importing both is not a redefinition.
        let src = run("$import macdefiner\n$import othermod\n", &[fixture_root()]).unwrap();
        assert_eq!(imports_of(&src, ""), ["macdefiner", "othermod"]);
    }

    #[test]
    fn a_define_is_not_visible_through_a_transitive_import() {
        // The entry imports only `mdbridge`; `mdbridge` imports `macdefiner`
        // (and so expands MD_MACRO to 40 inside its own code), but the entry
        // does not directly import `macdefiner`, so its `MD_MACRO` is left
        // unexpanded — non-transitive, mirroring symbol visibility.
        let src = run("$import mdbridge\nMD_MACRO\n", &[fixture_root()]).unwrap();
        assert!(
            has_identifier(&src, "MD_MACRO"),
            "MD_MACRO must NOT be visible to a file that only transitively imports its module"
        );
    }
}
