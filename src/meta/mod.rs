//! Metaprogramming — **rules** (hook #3 of the three-hook metasystem).
//!
//! Phase 2a: post-typecheck governance judgments implemented as **Rust
//! builtins** — no JIT, no `std/meta` handle ABI (those are Phase 2b). A
//! `rule <anchor> <fn>` declaration ([`crate::parser::RuleDecl`]) names a
//! builtin in [`builtins`]; [`run_rules`] validates each declaration, runs the
//! builtin over the whole typed program via a [`query::QueryIndex`], and
//! collects [`Judgment`]s. Rules **modify nothing** — the phase only reads and
//! reports (see `doc/plans/Three-Hook-Metasystem.md` §7, §15 Phase 2a).
//!
//! Governance is whole-program: a rule sees every anchor site regardless of
//! module, and importing a module imports its rules. Anchor resolution is a
//! flat symbol lookup and does **not** honor the `public type` gate — a
//! deliberate "governance sees all" choice consistent with §8.

pub mod builtins;
pub mod jit;
pub mod query;

use std::path::PathBuf;

use crate::lexer::{Position, TypeBase};
use crate::parser::{MetaKind, Program, RuleAnchor};
use query::QueryIndex;

/// Severity of a rule [`Judgment`]. `Error` fails the build; `Report` is a
/// non-fatal note (checker-only/audit rules) — it flows to stderr / the test
/// harness's warning channel and the build continues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Report,
}

/// A single rule verdict. The `rule` name is stamped by [`run_rules`]; a
/// builtin only supplies severity, position, and message (see [`RawJudgment`]).
#[derive(Debug, Clone, PartialEq)]
pub struct Judgment {
    pub severity: Severity,
    pub pos: Position,
    pub rule: String,
    pub message: String,
}

/// A judgment as a builtin emits it, before [`run_rules`] attaches the rule
/// name (per the design doc, the `rule` field belongs to the driver, not the
/// metaprogram).
#[derive(Debug, Clone, PartialEq)]
pub struct RawJudgment {
    pub severity: Severity,
    pub pos: Position,
    pub message: String,
}

impl RawJudgment {
    #[must_use]
    pub fn error(pos: Position, message: String) -> Self {
        Self {
            severity: Severity::Error,
            pos,
            message,
        }
    }

    #[must_use]
    pub fn report(pos: Position, message: String) -> Self {
        Self {
            severity: Severity::Report,
            pos,
            message,
        }
    }
}

/// A rule anchor resolved against the program: a struct id, or the source
/// positions of every site carrying the attribute.
pub enum ResolvedAnchor {
    Type(u32),
    Attribute(Vec<Position>),
}

/// A builtin rule: whole-program view + resolved anchor + the declaration
/// position (for anchor-level diagnostics) → judgments. The anchor is data the
/// builtin consults rather than a fixed argument shape, so the same builtins
/// port to the Phase 2b JIT'd `fn(Program) -> Judgments` form unchanged.
pub type RuleFn = fn(&QueryIndex<'_>, &ResolvedAnchor, Position) -> Vec<RawJudgment>;

/// Run every declared `rule` over the typed `program`, collecting judgments.
/// Modifies nothing. Validation failures (unknown type anchor, unknown builtin)
/// surface as `Error` judgments so they fail the build and the test corpus can
/// assert on them. Identical `rule` declarations are de-duplicated so a
/// repeated rule does not double every judgment.
#[must_use]
pub fn run_rules(program: &Program) -> Vec<Judgment> {
    let query = QueryIndex::build(program);
    let mut out = Vec::new();
    let mut seen: Vec<(&RuleAnchor, &str)> = Vec::new();

    for decl in &program.rules {
        let key = (&decl.anchor, decl.checker_fn.as_str());
        if seen.contains(&key) {
            continue;
        }
        seen.push(key);

        let anchor = match &decl.anchor {
            RuleAnchor::Type(name) => match resolve_type_anchor(program, name) {
                Some(id) => ResolvedAnchor::Type(id),
                None => {
                    out.push(Judgment {
                        severity: Severity::Error,
                        pos: decl.pos,
                        rule: decl.checker_fn.clone(),
                        message: format!("rule anchor names an unknown type '{name}'"),
                    });
                    continue;
                }
            },
            // A typo'd attribute anchor cannot be diagnosed in 2a — attributes
            // are undeclared strings — so an unknown one resolves to zero
            // carriers and the rule runs vacuously (the Phase 2b hygiene rule
            // closes this).
            RuleAnchor::Attribute(name) => ResolvedAnchor::Attribute(query.attr_carriers(name)),
        };

        // Resolution order: a compiler builtin first, then a user-authored
        // `rule fn`, then error.
        if let Some(rule_fn) = builtins::lookup(&decl.checker_fn) {
            for raw in rule_fn(&query, &anchor, decl.pos) {
                out.push(stamp(&decl.checker_fn, raw));
            }
            continue;
        }

        if let Some(func) = program
            .functions
            .iter()
            .find(|f| f.proto.name == decl.checker_fn)
        {
            if func.proto.meta_kind != Some(MetaKind::Rule) {
                out.push(Judgment {
                    severity: Severity::Error,
                    pos: decl.pos,
                    rule: decl.checker_fn.clone(),
                    message: format!(
                        "'{}' is an ordinary function; a rule checker must be a `rule fn`",
                        decl.checker_fn
                    ),
                });
                continue;
            }
            if !jit::is_valid_checker(func, program) {
                out.push(Judgment {
                    severity: Severity::Error,
                    pos: decl.pos,
                    rule: decl.checker_fn.clone(),
                    message: "a rule fn used as a checker must have signature \
                              `(Program, Type) -> Judgments`"
                        .to_string(),
                });
                continue;
            }
            let ResolvedAnchor::Type(id) = &anchor else {
                out.push(Judgment {
                    severity: Severity::Error,
                    pos: decl.pos,
                    rule: decl.checker_fn.clone(),
                    message: "attribute-anchored rule functions are not yet supported"
                        .to_string(),
                });
                continue;
            };
            match jit::run_rule_fn(program, &decl.checker_fn, *id, &query) {
                Ok(raws) => {
                    for raw in raws {
                        out.push(stamp(&decl.checker_fn, raw));
                    }
                }
                Err(e) => out.push(Judgment {
                    severity: Severity::Error,
                    pos: decl.pos,
                    rule: decl.checker_fn.clone(),
                    message: format!("rule fn '{}' failed: {e}", decl.checker_fn),
                }),
            }
            continue;
        }

        out.push(Judgment {
            severity: Severity::Error,
            pos: decl.pos,
            rule: decl.checker_fn.clone(),
            message: unknown_builtin_message(&decl.checker_fn),
        });
    }
    out
}

/// Attach the rule name to a builtin/meta-fn's raw judgment.
fn stamp(rule: &str, raw: RawJudgment) -> Judgment {
    Judgment {
        severity: raw.severity,
        pos: raw.pos,
        rule: rule.to_string(),
        message: raw.message,
    }
}

/// Resolve a type anchor to a struct id, following a one-hop `alias` to a
/// value struct type (`alias Cfg Config` then `rule Cfg singleton`). Whole
/// program, flat lookup — no `public type` gate (governance sees all).
fn resolve_type_anchor(program: &Program, name: &str) -> Option<u32> {
    if let Some(id) = program.symbols.struct_id(name) {
        return Some(id);
    }
    let ty = program.symbols.resolve_alias(name)?;
    if ty.pointer_depth == 0 {
        if let TypeBase::Struct(id) = ty.base {
            return Some(id);
        }
    }
    None
}

fn unknown_builtin_message(name: &str) -> String {
    match builtins::suggest(name) {
        Some(s) => format!("unknown rule '{name}' — did you mean '{s}'?"),
        None => format!("unknown rule '{name}'"),
    }
}

/// Render a judgment as `file:line:col: rule <name>: <msg>`, mirroring
/// [`crate::typechecker::TypeChecker::format_error`]; the file is resolved via
/// `pos.file_id` so a judgment inside an imported file names that file.
#[must_use]
pub fn format_judgment(judgment: &Judgment, source_files: &[PathBuf]) -> String {
    match source_files.get(judgment.pos.file_id as usize) {
        Some(path) => format!(
            "{}:{}:{}: rule {}: {}",
            path.display(),
            judgment.pos.line,
            judgment.pos.column,
            judgment.rule,
            judgment.message
        ),
        None => format!("rule {}: {}", judgment.rule, judgment.message),
    }
}
