use super::super::{preprocess_str, preprocess_str_with, Preprocessor};
use super::*;
use crate::lexer::TokenKind;

/// Strip Newline/Eof so assertions focus on the interesting kinds.
fn kinds(tokens: Vec<Token>) -> Vec<TokenKind> {
    tokens
        .into_iter()
        .map(|t| t.kind)
        .filter(|k| !matches!(k, TokenKind::Newline | TokenKind::Eof))
        .collect()
}

fn ints(source: &str) -> Vec<i64> {
    preprocess_str(source)
        .unwrap()
        .into_iter()
        .filter_map(|t| match t.kind {
            TokenKind::Integer(v) => Some(v),
            _ => None,
        })
        .collect()
}

// ── chain semantics ─────────────────────────────────────────────────

#[test]
fn ifdef_keeps_the_branch_when_defined() {
    assert_eq!(ints("$define A\n$ifdef A\n1\n$endif\n2\n"), vec![1, 2]);
}

#[test]
fn ifdef_drops_the_branch_when_undefined() {
    assert_eq!(ints("$ifdef A\n1\n$endif\n2\n"), vec![2]);
}

#[test]
fn ifndef_is_the_negated_ifdef() {
    assert_eq!(ints("$ifndef A\n1\n$endif\n"), vec![1]);
    assert_eq!(ints("$define A\n$ifndef A\n1\n$endif\n2\n"), vec![2]);
}

#[test]
fn chain_takes_the_first_true_branch_only() {
    // Both $elseifdef B arms are true; only the first wins.
    let src = "$define B\n\
               $ifdef A\n1\n$elseifdef B\n2\n$elseifdef B\n3\n$else\n4\n$endif\n";
    assert_eq!(ints(src), vec![2]);
}

#[test]
fn chain_mixes_elseif_and_elseifdef() {
    let mut pp = Preprocessor::new();
    pp.add_cli_define("N=5").unwrap();
    let tokens = preprocess_str_with(
        pp,
        "$ifdef MISSING\n1\n$elseif N == 4\n2\n$elseifdef N\n3\n$else\n4\n$endif\n",
    )
    .unwrap();
    assert_eq!(kinds(tokens), vec![TokenKind::Integer(3)]);
}

#[test]
fn else_activates_when_no_branch_was_true() {
    assert_eq!(ints("$ifdef A\n1\n$else\n2\n$endif\n"), vec![2]);
}

#[test]
fn else_stays_dead_after_a_taken_branch() {
    assert_eq!(ints("$define A\n$ifdef A\n1\n$else\n2\n$endif\n"), vec![1]);
}

#[test]
fn if_evaluates_its_expression() {
    assert_eq!(ints("$if 2 > 1\n1\n$endif\n"), vec![1]);
    assert_eq!(ints("$if 1 > 2\n1\n$endif\n2\n"), vec![2]);
}

#[test]
fn plan_example_bucket_chain() {
    let src = "$define MAX_SIZE 600\n\
               $if MAX_SIZE > 4096\n64\n$elseif MAX_SIZE > 512\n16\n$else\n4\n$endif\n";
    assert_eq!(ints(src), vec![16]);
}

#[test]
fn chains_nest_inside_an_active_branch() {
    let src = "$define A\n$ifdef A\n$ifdef A\n1\n$endif\n2\n$ifdef B\n3\n$endif\n4\n$endif\n";
    assert_eq!(ints(src), vec![1, 2, 4]);
}

#[test]
fn elseif_after_a_taken_branch_is_not_evaluated() {
    // UNDEFINED_NAME would be an error if the $elseif were evaluated.
    assert_eq!(ints("$if 1\n1\n$elseif UNDEFINED_NAME\n2\n$endif\n"), vec![1]);
}

// ── skipped-branch behaviour ────────────────────────────────────────

#[test]
fn skipped_branch_tracks_nested_chains() {
    // The inner $else must not activate anything; the inner $endif must
    // not close the outer chain.
    let src = "$ifdef MISSING\n$ifdef ALSO\n1\n$else\n2\n$endif\n3\n$else\n4\n$endif\n";
    assert_eq!(ints(src), vec![4]);
}

#[test]
fn inert_if_inside_a_skipped_branch_is_not_evaluated() {
    // An undefined identifier in a skipped $if must not error.
    let src = "$ifdef MISSING\n$if TOTALLY_UNDEFINED > 3\n1\n$endif\n$endif\n2\n";
    assert_eq!(ints(src), vec![2]);
}

#[test]
fn define_inside_a_skipped_branch_does_not_define() {
    let tokens = preprocess_str("$ifdef MISSING\n$define X 9\n$endif\nX\n").unwrap();
    assert_eq!(kinds(tokens), vec![TokenKind::Identifier("X".to_string())]);
}

#[test]
fn define_inside_a_skipped_branch_is_no_redefinition() {
    // The skipped $define never happened, so the later one is first.
    assert_eq!(
        ints("$ifdef MISSING\n$define X 9\n$endif\n$define X 5\nX\n"),
        vec![5]
    );
}

#[test]
fn import_inside_a_skipped_branch_is_inert() {
    // No `-I` roots are registered, so resolving this would error.
    let src = "$ifdef MISSING\n$import does/not/exist\n$endif\n";
    assert!(preprocess_str(src).is_ok());
}

#[test]
fn unknown_directive_inside_a_skipped_branch_is_inert() {
    assert!(preprocess_str("$ifdef MISSING\n$frobnicate all the things\n$endif\n").is_ok());
}

#[test]
fn midline_dollar_inside_a_skipped_branch_is_discarded() {
    assert!(preprocess_str("$ifdef MISSING\ni32 x $ y\n$endif\n").is_ok());
}

// ── chain-shape errors ──────────────────────────────────────────────

#[test]
fn stray_endif_is_an_error() {
    let err = preprocess_str("$endif\n").unwrap_err();
    assert!(matches!(
        err,
        PreprocessError::StrayConditional {
            directive: "endif",
            ..
        }
    ));
}

#[test]
fn stray_else_and_elseif_are_errors() {
    for (src, directive) in [
        ("$else\n", "else"),
        ("$elseif 1\n", "elseif"),
        ("$elseifdef A\n", "elseifdef"),
    ] {
        let err = preprocess_str(src).unwrap_err();
        assert!(
            matches!(
                &err,
                PreprocessError::StrayConditional { directive: d, .. } if *d == directive
            ),
            "`{src}` must be a stray-conditional error, got {err:?}"
        );
    }
}

#[test]
fn elseif_after_else_is_an_error() {
    let err = preprocess_str("$ifdef A\n$else\n$elseif 1\n$endif\n").unwrap_err();
    let PreprocessError::ConditionalAfterElse {
        directive: "elseif",
        else_pos,
        ..
    } = err
    else {
        panic!("expected ConditionalAfterElse, got {err:?}");
    };
    assert_eq!(else_pos.line, 2);
}

#[test]
fn double_else_is_an_error() {
    let err = preprocess_str("$ifdef A\n$else\n$else\n$endif\n").unwrap_err();
    assert!(matches!(
        err,
        PreprocessError::ConditionalAfterElse {
            directive: "else",
            ..
        }
    ));
}

#[test]
fn chain_shape_is_enforced_even_in_skipped_regions() {
    let err = preprocess_str("$ifdef MISSING\n$ifdef X\n$else\n$else\n$endif\n$endif\n")
        .unwrap_err();
    assert!(matches!(
        err,
        PreprocessError::ConditionalAfterElse {
            directive: "else",
            ..
        }
    ));
}

#[test]
fn unterminated_chain_names_the_opening_directive() {
    // The inner chain closes; the unterminated one is the outer $if.
    let err = preprocess_str("$if 1\n$ifdef A\n$endif\n").unwrap_err();
    let PreprocessError::UnterminatedConditional {
        directive: "if",
        pos,
    } = err
    else {
        panic!("expected UnterminatedConditional, got {err:?}");
    };
    assert_eq!((pos.line, pos.column), (1, 1));
}

#[test]
fn unterminated_nested_chain_reports_the_outermost() {
    let err = preprocess_str("$ifdef A\n$ifdef B\n").unwrap_err();
    assert!(matches!(
        err,
        PreprocessError::UnterminatedConditional {
            directive: "ifdef",
            pos,
        } if pos.line == 1
    ));
}

#[test]
fn extra_tokens_after_else_are_an_error() {
    let err = preprocess_str("$ifdef A\n$else garbage\n$endif\n").unwrap_err();
    assert!(matches!(
        err,
        PreprocessError::TrailingTokens {
            directive: "else",
            ..
        }
    ));
}

#[test]
fn extra_tokens_after_endif_are_an_error() {
    let err = preprocess_str("$ifdef A\n$endif A\n").unwrap_err();
    assert!(matches!(
        err,
        PreprocessError::TrailingTokens {
            directive: "endif",
            ..
        }
    ));
}

#[test]
fn ifdef_operand_must_be_a_single_identifier() {
    let err = preprocess_str("$ifdef\n$endif\n").unwrap_err();
    assert!(matches!(
        err,
        PreprocessError::ExpectedName {
            directive: "ifdef",
            ..
        }
    ));
    let err = preprocess_str("$ifndef A B\n$endif\n").unwrap_err();
    assert!(matches!(
        err,
        PreprocessError::TrailingTokens {
            directive: "ifndef",
            ..
        }
    ));
}

// ── top-level enforcement ───────────────────────────────────────────

#[test]
fn directive_inside_a_block_is_an_error() {
    let src = "fn main(u32 argc, u8 **argv) -> i32 {\n$ifdef A\n    return 1\n$endif\n}\n";
    let err = preprocess_str(src).unwrap_err();
    let PreprocessError::DirectiveInsideBlock(pos) = err else {
        panic!("expected DirectiveInsideBlock, got {err:?}");
    };
    assert_eq!(pos.line, 2);
}

#[test]
fn directive_after_a_closed_block_is_fine() {
    assert_eq!(ints("{\n}\n$define X 7\nX\n"), vec![7]);
}

#[test]
fn platform_defines_reach_if_expressions() {
    // ASPECT_VERSION_MAJOR is a builtin integer define.
    assert_eq!(ints("$if ASPECT_VERSION_MAJOR >= 0\n1\n$endif\n"), vec![1]);
}
