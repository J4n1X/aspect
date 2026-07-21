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

use crate::lexer::{LangType, Position, TypeBase};
use crate::parser::ast::{ExprKind, Expression, FunctionBody, Statement, StatementKind};
use crate::parser::{MetaKind, Program, RuleAnchor};
use query::QueryIndex;

/// The meta-only gate: `std/meta` types and meta functions (`rule fn`) may be
/// used only inside a meta function's body — never ordinary code. Injection
/// makes std/meta *present*, so this is the check that keeps it *gated* (§10.3).
/// Returns one `Error` judgment per violation; run before rules so a misuse is
/// a clean diagnostic rather than a cryptic undefined-`meta_*` codegen failure.
#[must_use]
pub fn check_meta_gate(program: &Program) -> Vec<Judgment> {
    let module_of = |file_id: u32| {
        program
            .file_modules
            .get(file_id as usize)
            .map_or("", String::as_str)
    };
    let meta_structs: std::collections::HashSet<u32> = program
        .symbols
        .structs()
        .filter(|s| module_of(s.file_id) == "std/meta")
        .map(|s| s.id)
        .collect();
    let meta_fns: std::collections::HashSet<&str> = program
        .functions
        .iter()
        .filter(|f| f.proto.meta_kind.is_some() || module_of(f.proto.pos.file_id) == "std/meta")
        .map(|f| f.proto.name.as_str())
        .collect();
    let is_meta_ty = |t: &LangType| matches!(t.base, TypeBase::Struct(id) if meta_structs.contains(&id));

    let mut out = Vec::new();
    for func in &program.functions {
        // Meta functions and the injected std/meta library are the legitimate
        // users of the meta surface.
        if func.proto.meta_kind.is_some() || module_of(func.proto.pos.file_id) == "std/meta" {
            continue;
        }
        let name = &func.proto.name;
        if func.proto.params.iter().any(|(t, _)| is_meta_ty(t)) || is_meta_ty(&func.proto.return_type)
        {
            out.push(Judgment {
                severity: Severity::Error,
                pos: func.proto.pos,
                rule: "meta-scope".to_string(),
                message: format!(
                    "ordinary function '{name}' uses a std/meta type in its signature; \
                     std/meta types are usable only inside a `rule fn`"
                ),
            });
        }
        if let FunctionBody::Aspect(body) = &func.body {
            let mut calls = Vec::new();
            for stmt in body {
                collect_meta_calls(stmt, &meta_fns, &mut calls);
            }
            for (callee, pos) in calls {
                out.push(Judgment {
                    severity: Severity::Error,
                    pos,
                    rule: "meta-scope".to_string(),
                    message: format!(
                        "ordinary function '{name}' calls the meta function '{callee}'; \
                         meta functions run only inside the rule engine"
                    ),
                });
            }
        }
    }
    out
}

type Calls = Vec<(String, Position)>;

fn collect_meta_calls(stmt: &Statement, meta_fns: &std::collections::HashSet<&str>, out: &mut Calls) {
    let expr = |e, out: &mut Calls| walk_expr_calls(e, meta_fns, out);
    match &stmt.kind {
        StatementKind::Expression(e) | StatementKind::Return(Some(e)) => expr(e, out),
        StatementKind::Return(None) | StatementKind::Break | StatementKind::Continue => {}
        StatementKind::Block(b) => b.iter().for_each(|s| collect_meta_calls(s, meta_fns, out)),
        StatementKind::If { condition, then_block, else_block } => {
            expr(condition, out);
            then_block.iter().for_each(|s| collect_meta_calls(s, meta_fns, out));
            if let Some(eb) = else_block {
                eb.iter().for_each(|s| collect_meta_calls(s, meta_fns, out));
            }
        }
        StatementKind::While { condition, body } => {
            expr(condition, out);
            body.iter().for_each(|s| collect_meta_calls(s, meta_fns, out));
        }
        StatementKind::For { init, condition, increment, body } => {
            if let Some(s) = init {
                collect_meta_calls(s, meta_fns, out);
            }
            if let Some(c) = condition {
                expr(c, out);
            }
            if let Some(s) = increment {
                collect_meta_calls(s, meta_fns, out);
            }
            body.iter().for_each(|s| collect_meta_calls(s, meta_fns, out));
        }
        StatementKind::VarDecl { initializer, .. } => {
            if let Some(e) = initializer {
                expr(e, out);
            }
        }
        StatementKind::VarAssign { value, .. } => expr(value, out),
        StatementKind::DerefAssign { target, value } | StatementKind::FieldAssign { target, value } => {
            expr(target, out);
            expr(value, out);
        }
    }
}

fn walk_expr_calls(e: &Expression, meta_fns: &std::collections::HashSet<&str>, out: &mut Calls) {
    let go = |e, out: &mut Calls| walk_expr_calls(e, meta_fns, out);
    match &e.kind {
        ExprKind::FunctionCall { name, args } => {
            if meta_fns.contains(name.as_str()) {
                out.push((name.clone(), e.pos));
            }
            args.iter().for_each(|a| go(a, out));
        }
        ExprKind::Binary { left, right, .. } | ExprKind::Comparison { left, right, .. } => {
            go(left, out);
            go(right, out);
        }
        ExprKind::Reference(x)
        | ExprKind::Dereference(x)
        | ExprKind::UnaryNot(x)
        | ExprKind::BitwiseNot(x)
        | ExprKind::Cast { expr: x, .. }
        | ExprKind::FieldAccess { base: x, .. } => go(x, out),
        ExprKind::IndirectCall { callee, args } => {
            go(callee, out);
            args.iter().for_each(|a| go(a, out));
        }
        ExprKind::MethodCall { base, args, .. } => {
            go(base, out);
            args.iter().for_each(|a| go(a, out));
        }
        ExprKind::StructLiteral { fields, .. } => fields.iter().for_each(|(_, fe)| go(fe, out)),
        ExprKind::Alloc { count, .. } => go(count, out),
        ExprKind::ListInitializer(items) => items.iter().for_each(|x| go(x, out)),
        ExprKind::ValueBlock(stmts) => stmts.iter().for_each(|s| collect_meta_calls(s, meta_fns, out)),
        ExprKind::Literal(_)
        | ExprKind::Variable(_)
        | ExprKind::EnumValue { .. }
        | ExprKind::FunctionRef(_)
        | ExprKind::SizeOf(_)
        | ExprKind::Null => {}
    }
}

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
    // The meta-only gate runs first: on a misuse (std/meta in ordinary code),
    // report it and do not run rules over a program that shouldn't compile.
    let gate = check_meta_gate(program);
    if gate.iter().any(|j| j.severity == Severity::Error) {
        return gate;
    }

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
