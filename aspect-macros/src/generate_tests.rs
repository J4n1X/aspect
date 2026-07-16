use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::quote;
use std::fs;
use std::path::{Path, PathBuf};

// ── Annotation types ──────────────────────────────────────────────────────────

enum Expected {
    ExitCode(i32),
    ErrorFragments(Vec<String>),
}

struct Annotation {
    expected: Expected,
    run_args: Vec<String>,
    compile_args: Vec<String>,
}

// ── Annotation parsing ────────────────────────────────────────────────────────

/// Parse a comma-separated list of quoted strings: `"a", "b", "c"`
fn parse_string_list(s: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut remaining = s.trim();
    loop {
        remaining = remaining.trim_start_matches([' ', ',']);
        if remaining.is_empty() || !remaining.starts_with('"') {
            break;
        }
        remaining = &remaining[1..]; // skip opening "
        let Some(end) = remaining.find('"') else {
            break;
        };
        result.push(remaining[..end].to_string());
        remaining = &remaining[end + 1..];
    }
    result
}

/// Scan the first 10 lines of `path` for `# expected:`, `# run_args:`, and
/// `# compile_args:` annotations. Returns `None` if no `# expected:`
/// annotation is found (file is skipped).
///
/// `# compile_args:` holds extra compiler flags for the invocation, as a
/// quoted-string list mirroring the CLI word-for-word — e.g.
/// `# compile_args: "-D", "DEBUG=1", "-I", "lib"`.
fn parse_annotation(path: &Path) -> Option<Annotation> {
    let source = fs::read_to_string(path).ok()?;
    let mut expected: Option<Expected> = None;
    let mut run_args: Vec<String> = Vec::new();
    let mut compile_args: Vec<String> = Vec::new();

    for line in source.lines().take(10) {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("# expected:") {
            let rest = rest.trim();
            if rest.starts_with('"') {
                let frags = parse_string_list(rest);
                if !frags.is_empty() {
                    expected = Some(Expected::ErrorFragments(frags));
                }
            } else if let Ok(n) = rest.parse::<i32>() {
                expected = Some(Expected::ExitCode(n));
            }
        } else if let Some(rest) = trimmed.strip_prefix("# run_args:") {
            run_args = parse_string_list(rest.trim());
        } else if let Some(rest) = trimmed.strip_prefix("# compile_args:") {
            compile_args = parse_string_list(rest.trim());
        }
    }

    expected.map(|e| Annotation {
        expected: e,
        run_args,
        compile_args,
    })
}

// ── File collection ───────────────────────────────────────────────────────────

fn collect_aspect_files(dir: &Path) -> Vec<PathBuf> {
    let mut result = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return result;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            result.extend(collect_aspect_files(&path));
        } else if path.extension().is_some_and(|e| e == "ap") {
            result.push(path);
        }
    }
    result
}

// ── Test name derivation ──────────────────────────────────────────────────────

/// Derive a valid Rust identifier from a path relative to the scan root.
/// `prefix` is the leading segment of the test fn name (e.g. `"test"` for
/// `tests/programs/` files, `"test_demo"` for `demos/` files).
///
/// `failures/literal_overflow.ap` with prefix `"test"`
///   → `test_failures_literal_overflow`
/// `hello.ap` with prefix `"test_demo"`
///   → `test_demo_hello`
fn make_test_ident(relative: &Path, prefix: &str) -> proc_macro2::Ident {
    let components: Vec<_> = relative.components().collect();
    let n = components.len();
    let parts: Vec<String> = components
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let s = c.as_os_str().to_string_lossy().into_owned();
            // Strip extension from the final component only
            let s = if i == n - 1 {
                s.trim_end_matches(".ap").to_string()
            } else {
                s
            };
            s.replace('-', "_")
        })
        .collect();
    let name = format!("{}_{}", prefix, parts.join("_"));
    proc_macro2::Ident::new(&name, Span::call_site())
}

// ── Code generation ───────────────────────────────────────────────────────────

pub fn generate_tests_impl(_input: TokenStream) -> TokenStream {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");

    let mut output = TokenStream2::new();

    // One scan root: the integration-test corpus. Demos are showcase
    // programs, not regression tests — they are deliberately NOT scanned.
    // Helpers under `tests/programs/include_helpers/` lack `# expected:`
    // and are silently skipped.
    let scans: [(&str, &[&str], &str); 1] = [("test", &["tests", "programs"], "")];

    for (prefix, base_components, _) in &scans {
        let mut base_dir = PathBuf::from(&manifest_dir);
        for c in *base_components {
            base_dir = base_dir.join(c);
        }
        let mut paths = collect_aspect_files(&base_dir);
        paths.sort();

        for abs_path in paths {
            let Some(ann) = parse_annotation(&abs_path) else {
                continue; // no # expected: annotation — skip
            };

            // Path used in compile_and_run calls (relative to workspace root, forward slashes)
            let path_str = abs_path
                .strip_prefix(&manifest_dir)
                .unwrap_or(&abs_path)
                .to_string_lossy()
                .replace('\\', "/");

            // Path relative to the scan root drives the test function name
            let relative_to_base = abs_path.strip_prefix(&base_dir).unwrap_or(&abs_path);
            let test_ident = make_test_ident(relative_to_base, prefix);

            let compile_args: Vec<&str> = ann.compile_args.iter().map(String::as_str).collect();
            let test_fn: TokenStream2 = match ann.expected {
            Expected::ExitCode(code) if ann.run_args.is_empty() && compile_args.is_empty() => quote! {
                #[test]
                fn #test_ident() {
                    let result = compile_and_run(#path_str)
                        .expect(concat!("Failed to compile and run ", #path_str));
                    assert_eq!(result, #code, "Expected exit code {}, got {}", #code, result);
                }
            },
            Expected::ExitCode(code) => {
                let args: Vec<&str> = ann.run_args.iter().map(String::as_str).collect();
                quote! {
                    #[test]
                    fn #test_ident() {
                        let result = compile_and_run_with_args(
                            #path_str,
                            &[#(String::from(#args)),*],
                            &[#(String::from(#compile_args)),*],
                        ).expect(concat!("Failed to compile and run ", #path_str));
                        assert_eq!(result, #code, "Expected exit code {}, got {}", #code, result);
                    }
                }
            }
            Expected::ErrorFragments(frags) => quote! {
                #[test]
                fn #test_ident() {
                    assert_compile_error_contains(
                        #path_str,
                        &[#(#frags),*],
                        &[#(String::from(#compile_args)),*],
                    );
                }
            },
            };

            output.extend(test_fn);
        }
    }

    output.into()
}
