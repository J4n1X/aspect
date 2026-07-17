//! The `$if` / `$elseif` constant-expression evaluator.
//!
//! `$if EXPR` takes a constant integer expression over integer literals,
//! `defined(NAME)` (1 or 0, the name deliberately *not* expanded), defined
//! names (expanded through the define table first; they must reduce to a
//! constant expression — an *undefined* identifier is an error, never
//! silently 0), parentheses, unary `!` / `-`, and the binary operators
//! `|| && == != < > <= >= | ^ & << >> + - * / %`. Precedence mirrors the
//! parser's `INFIX_OPS` table so `$if` agrees with runtime expressions —
//! note this means comparisons bind *looser* than bitwise operators
//! (`1 & 3 == 1` is `(1 & 3) == 1`), unlike C. Nonzero is true. The whole
//! expression must be evaluable: division/modulo by zero is an error even
//! on the dead side of `&&`/`||` (no short-circuit), arithmetic wraps, and
//! shift counts are masked to 0..=63.
//!
//! [`single_name`] — the single-identifier operand shape of `$ifdef` /
//! `$ifndef` / `$elseifdef` — also lives here and is shared with the chain
//! machine in [`super::conditional`].

use crate::lexer::{Position, Token, TokenKind};

use super::defines::ScopedDefines;
use super::errors::PreprocessError;

/// Parse `rest` as exactly one identifier — the shared operand shape of
/// `$ifdef` / `$ifndef` / `$elseifdef`.
pub(crate) fn single_name(
    directive: &'static str,
    rest: &[Token],
    pos: Position,
) -> Result<String, PreprocessError> {
    let Some((name_token, extra)) = rest.split_first() else {
        return Err(PreprocessError::ExpectedName { directive, pos });
    };
    let TokenKind::Identifier(name) = &name_token.kind else {
        return Err(PreprocessError::ExpectedName {
            directive,
            pos: name_token.pos,
        });
    };
    if let Some(extra) = extra.first() {
        return Err(PreprocessError::TrailingTokens {
            directive,
            pos: extra.pos,
        });
    }
    Ok(name.clone())
}

// ── `$if` constant-expression evaluator ─────────────────────────────────────

/// Evaluate a `$if` / `$elseif` expression to its integer value. `rest` is
/// the directive line after the name; `pos` is the directive's position,
/// used for errors that have no token to point at (e.g. an empty
/// expression).
///
/// Two passes: [`expand_operands`] resolves `defined(NAME)` and splices
/// define expansions in, then a Pratt walk evaluates the flat token slice.
pub(crate) fn eval_if_expr(
    rest: &[Token],
    defines: &ScopedDefines,
    pos: Position,
) -> Result<i64, PreprocessError> {
    let expanded = expand_operands(rest, defines)?;
    if expanded.is_empty() {
        return Err(PreprocessError::MalformedIfExpr {
            detail: "expected a constant integer expression".to_string(),
            pos,
        });
    }
    let mut eval = Eval {
        tokens: &expanded,
        i: 0,
        defines,
        last_pos: pos,
    };
    let value = eval.expr(0)?;
    if let Some(extra) = eval.peek() {
        return Err(PreprocessError::MalformedIfExpr {
            detail: format!("unexpected trailing token `{}`", extra.lexeme),
            pos: extra.pos,
        });
    }
    Ok(value)
}

/// Resolve `defined(NAME)` (the name deliberately NOT expanded — its
/// definedness is the operand) and expand every other identifier through
/// the define table. Identifiers with no define survive verbatim and are
/// reported by the evaluator, which can tell "undefined" apart from
/// "defined but not constant".
fn expand_operands(rest: &[Token], defines: &ScopedDefines) -> Result<Vec<Token>, PreprocessError> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < rest.len() {
        let token = &rest[i];
        match &token.kind {
            TokenKind::Identifier(name) if name == "defined" => {
                let (open, name_token, close) = (rest.get(i + 1), rest.get(i + 2), rest.get(i + 3));
                let operand = match (open.map(|t| &t.kind), name_token, close.map(|t| &t.kind)) {
                    (
                        Some(TokenKind::OpenParen),
                        Some(Token {
                            kind: TokenKind::Identifier(tested),
                            ..
                        }),
                        Some(TokenKind::CloseParen),
                    ) => tested,
                    _ => {
                        return Err(PreprocessError::MalformedIfExpr {
                            detail: "`defined` expects `defined(NAME)`".to_string(),
                            pos: token.pos,
                        });
                    }
                };
                let value = i64::from(defines.is_defined(operand));
                out.push(Token::new(
                    TokenKind::Integer(value),
                    token.pos,
                    value.to_string(),
                ));
                i += 4;
            }
            _ => {
                defines.expand_into(&mut out, token);
                i += 1;
            }
        }
    }
    Ok(out)
}

/// Pratt evaluator over the expanded token slice. Values are `i64`;
/// comparisons and logical operators yield 1/0.
struct Eval<'a, 'b> {
    tokens: &'a [Token],
    i: usize,
    /// Only consulted to word the identifier-operand error precisely.
    defines: &'a ScopedDefines<'b>,
    /// Position of the last consumed token — anchors "expression ended
    /// unexpectedly" errors.
    last_pos: Position,
}

/// Binding power of an infix operator, mirroring the parser's `INFIX_OPS`
/// table (src/parser/expressions.rs) so `$if` and runtime expressions
/// agree. All infix operators are left-associative.
fn infix_prec(kind: &TokenKind) -> Option<u8> {
    Some(match kind {
        TokenKind::LogicalOr => 1,
        TokenKind::LogicalAnd => 2,
        TokenKind::Equal
        | TokenKind::NotEqual
        | TokenKind::Less
        | TokenKind::Greater
        | TokenKind::LessEqual
        | TokenKind::GreaterEqual => 3,
        TokenKind::Pipe => 4,
        TokenKind::Caret => 5,
        TokenKind::Ampersand => 6,
        TokenKind::LeftShift | TokenKind::RightShift => 7,
        TokenKind::Plus | TokenKind::Minus => 10,
        TokenKind::Asterisk | TokenKind::Slash | TokenKind::Percent => 20,
        _ => return None,
    })
}

impl<'a, 'b> Eval<'a, 'b> {
    // Returned references borrow the token slice, not `self`, so a token
    // can stay in hand across further cursor movement.
    fn peek(&self) -> Option<&'a Token> {
        self.tokens.get(self.i)
    }

    fn advance(&mut self) -> Option<&'a Token> {
        let token = self.tokens.get(self.i)?;
        self.i += 1;
        self.last_pos = token.pos;
        Some(token)
    }

    /// Precedence-climbing loop: fold every infix operator binding at
    /// least as tightly as `min_prec`.
    fn expr(&mut self, min_prec: u8) -> Result<i64, PreprocessError> {
        let mut left = self.unary()?;
        while let Some(token) = self.peek() {
            let Some(prec) = infix_prec(&token.kind) else {
                break;
            };
            if prec < min_prec {
                break;
            }
            let (kind, pos) = (token.kind.clone(), token.pos);
            self.advance();
            let right = self.expr(prec + 1)?;
            left = apply(&kind, left, right, pos)?;
        }
        Ok(left)
    }

    /// Unary `!` and `-` bind tighter than every binary operator and stack
    /// (`!!x`, `- -x`).
    fn unary(&mut self) -> Result<i64, PreprocessError> {
        match self.peek().map(|t| &t.kind) {
            Some(TokenKind::LogicalNot) => {
                self.advance();
                Ok(i64::from(self.unary()? == 0))
            }
            Some(TokenKind::Minus) => {
                self.advance();
                Ok(self.unary()?.wrapping_neg())
            }
            _ => self.primary(),
        }
    }

    fn primary(&mut self) -> Result<i64, PreprocessError> {
        let Some(token) = self.advance() else {
            return Err(PreprocessError::MalformedIfExpr {
                detail: "expression ended unexpectedly".to_string(),
                pos: self.last_pos,
            });
        };
        match &token.kind {
            TokenKind::Integer(value) => Ok(*value),
            TokenKind::OpenParen => {
                let open_pos = token.pos;
                let value = self.expr(0)?;
                match self.advance() {
                    Some(t) if t.kind == TokenKind::CloseParen => Ok(value),
                    Some(t) => Err(PreprocessError::MalformedIfExpr {
                        detail: format!("expected `)`, found `{}`", t.lexeme),
                        pos: t.pos,
                    }),
                    None => Err(PreprocessError::MalformedIfExpr {
                        detail: "unclosed `(`".to_string(),
                        pos: open_pos,
                    }),
                }
            }
            // Identifiers surviving expansion: undefined names error loudly
            // (never silently 0); defined names that reach here expanded to
            // something non-constant (e.g. a self-referential define).
            TokenKind::Identifier(name) => {
                if self.defines.is_defined(name) {
                    Err(PreprocessError::MalformedIfExpr {
                        detail: format!(
                            "`{name}` does not expand to a constant integer expression"
                        ),
                        pos: token.pos,
                    })
                } else {
                    Err(PreprocessError::UndefinedInIfExpr {
                        name: name.clone(),
                        pos: token.pos,
                    })
                }
            }
            _ => Err(PreprocessError::MalformedIfExpr {
                detail: format!("unexpected `{}`", token.lexeme),
                pos: token.pos,
            }),
        }
    }
}

/// Apply one binary operator. Arithmetic wraps (never panics); `/` and `%`
/// by zero are errors at the operator's position; shift counts are masked
/// to 0..=63 (deterministic where C leaves UB).
fn apply(op: &TokenKind, left: i64, right: i64, pos: Position) -> Result<i64, PreprocessError> {
    Ok(match op {
        TokenKind::LogicalOr => i64::from(left != 0 || right != 0),
        TokenKind::LogicalAnd => i64::from(left != 0 && right != 0),
        TokenKind::Equal => i64::from(left == right),
        TokenKind::NotEqual => i64::from(left != right),
        TokenKind::Less => i64::from(left < right),
        TokenKind::Greater => i64::from(left > right),
        TokenKind::LessEqual => i64::from(left <= right),
        TokenKind::GreaterEqual => i64::from(left >= right),
        TokenKind::Pipe => left | right,
        TokenKind::Caret => left ^ right,
        TokenKind::Ampersand => left & right,
        TokenKind::LeftShift => left.wrapping_shl(mask_shift(right)),
        TokenKind::RightShift => left.wrapping_shr(mask_shift(right)),
        TokenKind::Plus => left.wrapping_add(right),
        TokenKind::Minus => left.wrapping_sub(right),
        TokenKind::Asterisk => left.wrapping_mul(right),
        TokenKind::Slash => {
            if right == 0 {
                return Err(PreprocessError::IfDivisionByZero { pos });
            }
            left.wrapping_div(right)
        }
        TokenKind::Percent => {
            if right == 0 {
                return Err(PreprocessError::IfDivisionByZero { pos });
            }
            left.wrapping_rem(right)
        }
        other => unreachable!("infix_prec admitted non-operator `{other:?}`"),
    })
}

/// Mask a shift count into 0..=63 (i64 width).
fn mask_shift(count: i64) -> u32 {
    u32::try_from(count & 63).expect("masked shift count fits u32")
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::defines::DefineTable;
    use crate::lexer::tokenize;

    fn eval_with(defines: &DefineTable, expr: &str) -> Result<i64, PreprocessError> {
        let mut tokens = tokenize(expr.to_string()).unwrap();
        tokens.retain(|t| !matches!(t.kind, TokenKind::Newline | TokenKind::Eof));
        // These tests only use global (`-D`) defines, so the module-unaware
        // global view is the right scope.
        eval_if_expr(&tokens, &defines.global_view(), Position::start())
    }

    fn eval(expr: &str) -> Result<i64, PreprocessError> {
        eval_with(&DefineTable::new(), expr)
    }

    // ── `$if` evaluator ─────────────────────────────────────────────────

    #[test]
    fn evaluator_multiplies_before_adding() {
        assert_eq!(eval("1 + 2 * 3").unwrap(), 7);
        assert_eq!(eval("1 + 2 * 3 == 7").unwrap(), 1);
    }

    #[test]
    fn evaluator_shifts_bind_looser_than_arithmetic() {
        assert_eq!(eval("1 << 4").unwrap(), 16);
        assert_eq!(eval("1 << 3 + 1").unwrap(), 16); // 1 << (3 + 1)
        assert_eq!(eval("256 >> 2 * 2").unwrap(), 16); // 256 >> 4
    }

    #[test]
    fn evaluator_bitwise_binds_tighter_than_comparison() {
        // Parser-table precedence (unlike C): `&`/`|` bind tighter than `==`.
        assert_eq!(eval("1 & 3 == 1").unwrap(), 1); // (1 & 3) == 1
        assert_eq!(eval("1 | 2 == 3").unwrap(), 1); // (1 | 2) == 3
        assert_eq!(eval("4 ^ 5 == 1").unwrap(), 1); // (4 ^ 5) == 1
    }

    #[test]
    fn evaluator_logical_operators_normalise_to_bool() {
        assert_eq!(eval("2 && 3").unwrap(), 1);
        assert_eq!(eval("0 || 2 && 0").unwrap(), 0); // && before ||
        assert_eq!(eval("1 || 0").unwrap(), 1);
    }

    #[test]
    fn evaluator_parentheses_override_precedence() {
        assert_eq!(eval("(1 + 2) * 3").unwrap(), 9);
    }

    #[test]
    fn evaluator_unary_minus_and_not() {
        assert_eq!(eval("-3 + 5").unwrap(), 2);
        assert_eq!(eval("1 - -2").unwrap(), 3);
        assert_eq!(eval("-(2 + 3)").unwrap(), -5);
        assert_eq!(eval("!0").unwrap(), 1);
        assert_eq!(eval("!7").unwrap(), 0);
        assert_eq!(eval("!!7").unwrap(), 1);
    }

    #[test]
    fn evaluator_defined_and_negation() {
        let mut defines = DefineTable::new();
        defines.add_cli_define("A=0").unwrap();
        // defined() tests definedness, not the value — A=0 is still defined.
        assert_eq!(eval_with(&defines, "defined(A)").unwrap(), 1);
        assert_eq!(eval_with(&defines, "defined(B)").unwrap(), 0);
        assert_eq!(eval_with(&defines, "!defined(A)").unwrap(), 0);
        assert_eq!(eval_with(&defines, "defined(A) && !defined(B)").unwrap(), 1);
    }

    #[test]
    fn evaluator_expands_defines_into_expressions() {
        let mut defines = DefineTable::new();
        defines.add_cli_define("N=4").unwrap();
        defines.add_cli_define("MAX=40 + 2").unwrap();
        assert_eq!(eval_with(&defines, "N * 2").unwrap(), 8);
        assert_eq!(eval_with(&defines, "MAX == 42").unwrap(), 1);
    }

    #[test]
    fn evaluator_undefined_identifier_is_an_error() {
        let err = eval("MYSTERY > 2").unwrap_err();
        let PreprocessError::UndefinedInIfExpr { name, .. } = err else {
            panic!("expected UndefinedInIfExpr, got {err:?}");
        };
        assert_eq!(name, "MYSTERY");
    }

    #[test]
    fn evaluator_non_constant_define_is_an_error() {
        let mut defines = DefineTable::new();
        defines.add_cli_define("X=X").unwrap(); // self-referential
        let err = eval_with(&defines, "X").unwrap_err();
        assert!(matches!(
            &err,
            PreprocessError::MalformedIfExpr { detail, .. }
                if detail.contains("constant integer expression")
        ));
    }

    #[test]
    fn evaluator_division_by_zero_is_an_error() {
        for expr in ["1 / 0", "5 % 0", "1 / (2 - 2)"] {
            assert!(
                matches!(eval(expr), Err(PreprocessError::IfDivisionByZero { .. })),
                "`{expr}` must be a division-by-zero error"
            );
        }
    }

    #[test]
    fn evaluator_rejects_trailing_tokens() {
        assert!(matches!(
            eval("1 2"),
            Err(PreprocessError::MalformedIfExpr { .. })
        ));
    }

    #[test]
    fn evaluator_rejects_an_empty_expression() {
        assert!(matches!(
            eval(""),
            Err(PreprocessError::MalformedIfExpr { .. })
        ));
    }

    #[test]
    fn evaluator_rejects_malformed_defined() {
        for expr in ["defined A", "defined(3)", "defined("] {
            assert!(
                matches!(eval(expr), Err(PreprocessError::MalformedIfExpr { .. })),
                "`{expr}` must be a malformed-expression error"
            );
        }
    }

    #[test]
    fn evaluator_rejects_stray_keywords() {
        assert!(matches!(
            eval("1 + return"),
            Err(PreprocessError::MalformedIfExpr { .. })
        ));
    }
}
