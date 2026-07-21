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
    requires_arch: Option<String>,
    /// `# expected_warning: "frag"` — a fragment that must appear in the
    /// checker's warnings. Only meaningful on a runtime test.
    expected_warning: Option<String>,
}

/// Maps an `ARCH_*` name to the `cfg(target_arch = ..)` value for the same
/// machine. `None` for an unrecognised name — treated as "no gate", not a
/// silently disabled test.
fn cfg_target_arch_for(arch_define: &str) -> Option<&'static str> {
    match arch_define {
        "ARCH_X86_64" => Some("x86_64"),
        "ARCH_AARCH64" => Some("aarch64"),
        _ => None,
    }
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
        remaining = &remaining[1..];
        let Some(end) = remaining.find('"') else {
            break;
        };
        result.push(remaining[..end].to_string());
        remaining = &remaining[end + 1..];
    }
    result
}

/// Scans the first 10 lines of `path` for `# expected:` and friends; `None`
/// (file skipped) when no `# expected:` is found.
///
/// `# requires_arch: ARCH_X86_64` compiles the test only on that host arch. A
/// *running* program can gate itself with `$ifdef` and match in the `$else`,
/// but a program asserting a *compile error* can't — gating it away leaves a
/// clean-compiling program, so the test must be gated from outside.
fn parse_annotation(path: &Path) -> Option<Annotation> {
    let source = fs::read_to_string(path).ok()?;
    let mut expected: Option<Expected> = None;
    let mut run_args: Vec<String> = Vec::new();
    let mut compile_args: Vec<String> = Vec::new();
    let mut requires_arch: Option<String> = None;
    let mut expected_warning: Option<String> = None;

    for line in source.lines().take(10) {
        let trimmed = line.trim();
        // `# expected_warning:` must be tested before `# expected:` — the
        // latter is a prefix of the former, so the order matters.
        if let Some(rest) = trimmed.strip_prefix("# expected_warning:") {
            expected_warning = parse_string_list(rest.trim()).into_iter().next();
        } else if let Some(rest) = trimmed.strip_prefix("# expected:") {
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
        } else if let Some(rest) = trimmed.strip_prefix("# requires_arch:") {
            requires_arch = Some(rest.trim().to_string());
        }
    }

    expected.map(|e| Annotation {
        expected: e,
        run_args,
        compile_args,
        requires_arch,
        expected_warning,
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

/// A valid Rust identifier from a scan-root-relative path, e.g.
/// `failures/literal_overflow.ap` with prefix `"test"` →
/// `test_failures_literal_overflow`.
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

    // Only the integration-test corpus. Demos are showcase programs, not
    // regression tests, so they are deliberately NOT scanned.
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
                continue;
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

            // An unrecognised arch name leaves the test ungated — a typo should
            // not silently make a test disappear.
            let arch_gate: TokenStream2 = ann
                .requires_arch
                .as_deref()
                .and_then(cfg_target_arch_for)
                .map_or_else(TokenStream2::new, |arch| {
                    quote! { #[cfg(target_arch = #arch)] }
                });

            // An optional `# expected_warning:` assertion, spliced into runtime
            // tests after the exit-code check (a warning never fails the build,
            // so it rides on a passing runtime test).
            let warning_check: TokenStream2 = match ann.expected_warning.as_deref() {
                Some(frag) => quote! {
                    assert_warning_contains(
                        #path_str,
                        #frag,
                        &[#(String::from(#compile_args)),*],
                    );
                },
                None => TokenStream2::new(),
            };

            let test_fn: TokenStream2 = match ann.expected {
            Expected::ExitCode(code) if ann.run_args.is_empty() && compile_args.is_empty() => quote! {
                #arch_gate
                #[test]
                fn #test_ident() {
                    let result = compile_and_run(#path_str)
                        .expect(concat!("Failed to compile and run ", #path_str));
                    assert_eq!(result, #code, "Expected exit code {}, got {}", #code, result);
                    #warning_check
                }
            },
            Expected::ExitCode(code) => {
                let args: Vec<&str> = ann.run_args.iter().map(String::as_str).collect();
                quote! {
                    #arch_gate
                    #[test]
                    fn #test_ident() {
                        let result = compile_and_run_with_args(
                            #path_str,
                            &[#(String::from(#args)),*],
                            &[#(String::from(#compile_args)),*],
                        ).expect(concat!("Failed to compile and run ", #path_str));
                        assert_eq!(result, #code, "Expected exit code {}, got {}", #code, result);
                        #warning_check
                    }
                }
            }
            Expected::ErrorFragments(frags) => quote! {
                #arch_gate
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
