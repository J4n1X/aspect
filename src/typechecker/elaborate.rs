//! Round-based elaboration: re-checks the whole program to a fixpoint so
//! transform handlers can rewrite the AST at stuck type judgments. No handlers
//! are registered yet, so the loop settles in a single pass.

use super::checker::TypeChecker;
use super::errors::TypeCheckError;
use crate::lexer::LangType;
use crate::parser::Program;
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

/// Transform handlers keyed by [`Obligation`]. Currently always empty.
#[derive(Debug, Clone, Default)]
pub struct HandlerRegistry {
    keys: Vec<Obligation>,
}

impl HandlerRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }
}

/// Result of [`elaborate_program`]: the final round's checker (kept so the
/// caller can format diagnostics and read warnings), its result, and the round
/// count.
pub struct Elaboration {
    pub checker: TypeChecker,
    pub result: Result<(), Vec<TypeCheckError>>,
    pub rounds: usize,
}

/// Type-check `program` to a fixpoint: re-check with a fresh [`TypeChecker`] each
/// round until a round rewrites nothing or `max_rounds` is exceeded. Only the
/// final round's diagnostics are reported.
#[must_use]
pub fn elaborate_program(
    program: &mut Program,
    target: TargetSpec,
    max_rounds: usize,
) -> Elaboration {
    let mut round = 0;
    loop {
        round += 1;
        let mut checker = TypeChecker::new().with_target(target.clone());
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
