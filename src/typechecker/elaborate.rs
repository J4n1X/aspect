//! Round-based elaboration: re-checks the whole program to a fixpoint so
//! transform handlers can rewrite the AST at stuck type judgments. A program
//! that declares no coercion transforms settles in a single pass; otherwise the
//! transform engine is JIT-built once and each stuck demand site consults it.

use super::checker::TypeChecker;
use super::errors::TypeCheckError;
use super::types::types_coercible;
use crate::lexer::LangType;
use crate::parser::{Program, TransformDecl, TransformKey};
use crate::target::TargetSpec;

/// Round cap before a non-settling transform is reported as an error.
pub const DEFAULT_MAX_ROUNDS: usize = 16;

/// A stuck type judgment a transform handler can be consulted to repair, keyed
/// by kind and subject.
#[derive(Debug, Clone, PartialEq)]
pub enum Obligation {
    /// `from` was found where `to` was expected and built-in coercion failed.
    Coerce { from: LangType, to: LangType },
}

/// A resolved transform handler: the demand-site type pair it claims, the JIT'd
/// trampoline's address, and its reach (`is_public`, else scoped to `module`).
/// `addr` is only valid while the [`with_transform_engine`] call that produced
/// it is on the stack — see that function.
///
/// [`with_transform_engine`]: crate::meta::jit::with_transform_engine
#[derive(Debug, Clone)]
pub struct HandlerAddr {
    pub from: LangType,
    pub to: LangType,
    pub addr: usize,
    pub module: String,
    pub is_public: bool,
}

/// Transform handlers keyed by demand-site type pair. Empty for a program with
/// no coercion transforms, in which case elaboration is a single pass.
#[derive(Debug, Clone, Default)]
pub struct HandlerRegistry {
    entries: Vec<HandlerAddr>,
}

impl HandlerRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn from_entries(entries: Vec<HandlerAddr>) -> Self {
        Self { entries }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The handler claiming `from -> to` visible from `module`, if any. A public
    /// handler is visible everywhere; a private one only within its own module.
    #[must_use]
    pub fn lookup(&self, from: &LangType, to: &LangType, module: &str) -> Option<&HandlerAddr> {
        self.entries.iter().find(|e| {
            ty_matches(&e.from, from)
                && ty_matches(&e.to, to)
                && (e.is_public || e.module == module)
        })
    }
}

/// Demand-site type matching for a handler key: same shape, ignoring `is_const`
/// so a `const T` site still fires a `T -> …` transform (const *removal* is
/// blocked at key-registration time, not here).
fn ty_matches(a: &LangType, b: &LangType) -> bool {
    a.base == b.base
        && a.size_bits == b.size_bits
        && a.pointer_depth == b.pointer_depth
        && a.array_size == b.array_size
}

/// Whether two transform declarations can both fire at some site: true if either
/// is public (whole-program reach) or they share a declaring module.
fn reach_overlaps(program: &Program, a: &TransformDecl, b: &TransformDecl) -> bool {
    use crate::symbol::module::Visibility;
    let module_of = |d: &TransformDecl| {
        program
            .file_modules
            .get(d.pos.file_id as usize)
            .map_or("", String::as_str)
            .to_string()
    };
    a.vis == Visibility::Public || b.vis == Visibility::Public || module_of(a) == module_of(b)
}

/// Result of [`elaborate_program`]: the final round's checker (kept so the
/// caller can format diagnostics and read warnings), its result, and the round
/// count.
pub struct Elaboration {
    pub checker: TypeChecker,
    pub result: Result<(), Vec<TypeCheckError>>,
    pub rounds: usize,
}

/// Type-check `program` to a fixpoint. If it declares coercion transforms, the
/// transform engine is JIT-built once (before any round), and each round's
/// checker consults it at stuck demand sites; the loop settles when a round
/// rewrites nothing. Only the final round's diagnostics are reported.
#[must_use]
pub fn elaborate_program(
    program: &mut Program,
    target: TargetSpec,
    max_rounds: usize,
) -> Elaboration {
    let coerce_decls: Vec<TransformDecl> = program
        .transforms
        .iter()
        .filter(|d| matches!(d.key, TransformKey::Coerce { .. }))
        .cloned()
        .collect();

    // No coercion transforms: the fast path — one registry-less round loop.
    if coerce_decls.is_empty() {
        return run_rounds(program, &target, max_rounds, HandlerRegistry::new());
    }

    // Reject dead / const-laundering keys and invalid handlers before building.
    if let Err(errs) = validate_transforms(program, &coerce_decls) {
        return failed(program, target, errs);
    }

    // Build the engine and run the rounds inside its lifetime (the JIT'd handler
    // code is live only while `with_transform_engine` is on the stack). The
    // `&mut Program` is threaded through the closure to keep a single borrow.
    match crate::meta::jit::with_transform_engine(program, &coerce_decls, &target, |program, registry| {
        run_rounds(program, &target, max_rounds, registry)
    }) {
        Ok(elab) => elab,
        Err(message) => failed(
            program,
            target,
            vec![TypeCheckError::TransformEngineError { message }],
        ),
    }
}

/// Re-check with a fresh [`TypeChecker`] each round until a round rewrites
/// nothing (the fixpoint) or `max_rounds` is exceeded.
fn run_rounds(
    program: &mut Program,
    target: &TargetSpec,
    max_rounds: usize,
    registry: HandlerRegistry,
) -> Elaboration {
    let mut round = 0;
    loop {
        round += 1;
        let mut checker = TypeChecker::new()
            .with_target(target.clone())
            .with_handlers(registry.clone());
        let result = checker.check_program(program);
        // A round that rewrote nothing is the fixpoint; its result is final.
        if checker.rewrites() == 0 {
            return Elaboration {
                checker,
                result,
                rounds: round,
            };
        }
        if round >= max_rounds {
            let err = TypeCheckError::RoundLimitExceeded {
                message: format!(
                    "elaboration did not settle within {max_rounds} rounds — a transform keeps rewriting (raise --max-rounds if this is legitimate)"
                ),
            };
            return Elaboration {
                checker,
                result: Err(vec![err]),
                rounds: round,
            };
        }
        // Rewrote something and under the bound — re-check; a later rewrite may
        // clear this round's errors, so they are discarded.
    }
}

/// Validate every coercion transform declaration before the engine is built: a
/// key that already coerces implicitly is dead (could never fire), a key that
/// only removes `const` would make const-removal implicit, and the named handler
/// must be a `transform fn` with signature `(Expr) -> Expr`.
fn validate_transforms(program: &Program, decls: &[TransformDecl]) -> Result<(), Vec<TypeCheckError>> {
    let mut errs = Vec::new();
    for (i, decl) in decls.iter().enumerate() {
        let TransformKey::Coerce { from, to } = &decl.key else {
            continue;
        };
        // Two handlers claiming the same key with overlapping reach is ambiguous
        // — reject the later one rather than silently letting the first win.
        if let Some(prev) = decls[..i].iter().find(|p| {
            let TransformKey::Coerce { from: pf, to: pt } = &p.key else {
                return false;
            };
            ty_matches(pf, from)
                && ty_matches(pt, to)
                && reach_overlaps(program, p, decl)
        }) {
            errs.push(TypeCheckError::InvalidTransformKey {
                message: format!(
                    "transform key '{from} -> {to}' already has a handler ('{}') in reach — one key, one handler",
                    prev.handler_fn
                ),
                position: decl.pos,
            });
            continue;
        }
        if types_coercible(from, to) {
            errs.push(TypeCheckError::InvalidTransformKey {
                message: format!(
                    "transform key '{from} -> {to}' is dead: '{from}' already coerces to '{to}' implicitly, so the handler could never fire"
                ),
                position: decl.pos,
            });
            continue;
        }
        if from.base == to.base
            && from.size_bits == to.size_bits
            && from.pointer_depth == to.pointer_depth
            && from.array_size == to.array_size
            && from.is_const
            && !to.is_const
        {
            errs.push(TypeCheckError::InvalidTransformKey {
                message: format!(
                    "transform key '{from} -> {to}' removes const — const removal must stay explicit (an `as` cast), never an implicit transform"
                ),
                position: decl.pos,
            });
            continue;
        }
        match program.functions.iter().find(|f| f.proto.name == decl.handler_fn) {
            None => errs.push(TypeCheckError::InvalidTransformKey {
                message: format!("transform handler '{}' is not defined", decl.handler_fn),
                position: decl.pos,
            }),
            Some(f) if !crate::meta::jit::is_valid_transform(f, program) => {
                errs.push(TypeCheckError::InvalidTransformKey {
                    message: format!(
                        "transform handler '{}' must be a `transform fn` with signature `(Expr) -> Expr`",
                        decl.handler_fn
                    ),
                    position: decl.pos,
                });
            }
            Some(_) => {}
        }
    }
    if errs.is_empty() {
        Ok(())
    } else {
        Err(errs)
    }
}

/// An [`Elaboration`] carrying a pre-round failure (a bad transform key or an
/// engine-build error), with a checker seeded to format the diagnostics.
fn failed(program: &Program, target: TargetSpec, errs: Vec<TypeCheckError>) -> Elaboration {
    let checker = TypeChecker::new()
        .with_target(target)
        .with_source_files(program.source_files.clone());
    Elaboration {
        checker,
        result: Err(errs),
        rounds: 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexer::TypeBase;

    fn struct_ty(is_const: bool) -> LangType {
        LangType::new(TypeBase::Struct(0), 0, 0, is_const)
    }

    fn entry(module: &str, is_public: bool) -> HandlerAddr {
        HandlerAddr {
            from: struct_ty(false),
            to: LangType::U8_PTR,
            addr: 1,
            module: module.to_string(),
            is_public,
        }
    }

    #[test]
    fn lookup_matches_ignoring_const_within_module() {
        let reg = HandlerRegistry::from_entries(vec![entry("m", false)]);
        // Exact and const-only-differing demand sites both match.
        assert!(reg.lookup(&struct_ty(false), &LangType::U8_PTR, "m").is_some());
        assert!(reg.lookup(&struct_ty(true), &LangType::U8_PTR, "m").is_some());
        // A different target type does not.
        assert!(reg.lookup(&struct_ty(false), &LangType::I32, "m").is_none());
    }

    #[test]
    fn private_handler_is_module_scoped() {
        let reg = HandlerRegistry::from_entries(vec![entry("m", false)]);
        assert!(reg.lookup(&struct_ty(false), &LangType::U8_PTR, "m").is_some());
        assert!(reg.lookup(&struct_ty(false), &LangType::U8_PTR, "other").is_none());
    }

    #[test]
    fn public_handler_reaches_any_module() {
        let reg = HandlerRegistry::from_entries(vec![entry("m", true)]);
        assert!(reg
            .lookup(&struct_ty(false), &LangType::U8_PTR, "elsewhere")
            .is_some());
    }
}
