//! The builtin rule registry (Phase 2a). A name here is what `rule <anchor>
//! <fn>` resolves against; a later phase will let it name a user-authored
//! Aspect function instead (same surface syntax, different resolution).

use crate::lexer::Position;

use super::{query::QueryIndex, RawJudgment, ResolvedAnchor, RuleFn};

/// Registry of builtin rules, name → implementation. A slice (not a map) so
/// [`suggest`] can offer a did-you-mean over the same source of truth.
const BUILTINS: &[(&str, RuleFn)] = &[("singleton", singleton), ("audit", audit)];

/// Look up a builtin rule by the name written in a `rule` declaration.
#[must_use]
pub fn lookup(name: &str) -> Option<RuleFn> {
    BUILTINS
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, f)| *f)
}

/// The nearest builtin name to `name` within a small edit distance, for a
/// did-you-mean on an unknown rule (mirrors the preprocessor's directive
/// suggestions).
#[must_use]
pub fn suggest(name: &str) -> Option<String> {
    BUILTINS
        .iter()
        .map(|(candidate, _)| (levenshtein(name, candidate), *candidate))
        .filter(|(distance, _)| *distance <= 2)
        .min_by_key(|(distance, _)| *distance)
        .map(|(_, candidate)| candidate.to_string())
}

/// `singleton`: a type anchored by this rule may be **constructed** at most
/// once in the whole program; each construction past the first is an error.
/// "Construction" is a struct literal or an `alloc` of the type (see
/// [`QueryIndex::instantiations_of`] for the counted set and its v1 blind
/// spots).
fn singleton(query: &QueryIndex<'_>, anchor: &ResolvedAnchor, decl_pos: Position) -> Vec<RawJudgment> {
    let ResolvedAnchor::Type(id) = anchor else {
        return vec![RawJudgment::error(
            decl_pos,
            "the `singleton` rule needs a type anchor, e.g. `rule Config singleton`".to_string(),
        )];
    };
    let sites = query.instantiations_of(*id);
    if sites.len() <= 1 {
        return Vec::new();
    }
    let name = query.struct_name(*id);
    sites
        .iter()
        .skip(1)
        .map(|pos| {
            RawJudgment::error(
                *pos,
                format!(
                    "type '{name}' is declared a singleton but is constructed here more than once"
                ),
            )
        })
        .collect()
}

/// `audit`: a checker-only rule (emits `Report`s, never errors). Lists every
/// site of its anchor — the carriers of an `@attribute`, or the construction
/// sites of a type. Proves attribute-anchor resolution and the report channel;
/// a zero-carrier anchor produces nothing and compiles clean.
fn audit(query: &QueryIndex<'_>, anchor: &ResolvedAnchor, _decl_pos: Position) -> Vec<RawJudgment> {
    match anchor {
        ResolvedAnchor::Attribute(carriers) => carriers
            .iter()
            .map(|pos| RawJudgment::report(*pos, "audited: site carries the anchored attribute".to_string()))
            .collect(),
        ResolvedAnchor::Type(id) => {
            let name = query.struct_name(*id);
            query
                .instantiations_of(*id)
                .iter()
                .map(|pos| RawJudgment::report(*pos, format!("audited: '{name}' constructed here")))
                .collect()
        }
    }
}

/// Classic dynamic-programming Levenshtein distance (single-row). Local to the
/// meta module for now; the preprocessor has a private twin, and hoisting both
/// into a shared util is tracked as a Phase 2b cleanup.
fn levenshtein(a: &str, b: &str) -> usize {
    let b_chars: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b_chars.len()).collect();
    let mut curr = vec![0usize; b_chars.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, &cb) in b_chars.iter().enumerate() {
            let cost = usize::from(ca != cb);
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b_chars.len()]
}
