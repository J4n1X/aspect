//! `$ifdef` / `$ifndef` / `$if` / `$elseif` / `$elseifdef` / `$else` /
//! `$endif` — conditional compilation: chain tracking and the `$if`
//! constant-expression evaluator.
//!
//! ## Chains
//!
//! A chain opens with `$ifdef NAME`, `$ifndef NAME`, or `$if EXPR`, is
//! followed by any mix of `$elseif EXPR` / `$elseifdef NAME`, at most one
//! `$else`, and closes with `$endif`. Chains nest arbitrarily. Exactly one
//! branch of a chain is active: the first whose condition is true (or the
//! `$else`, if no condition was). The open chains form a stack of
//! [`Frame`]s owned by [`ConditionalStack`]; the driver consults
//! [`ConditionalStack::active`] on every ordinary token and every
//! non-conditional directive.
//!
//! ## Skipping
//!
//! Inside a false/inactive branch ordinary tokens are dropped and
//! non-conditional directives are inert (`$define` does not define,
//! `$import` does not resolve, unknown names do not error). Conditional
//! directives ARE still processed so nesting stays matched — but a chain
//! opened inside a skipped region is *inert*: its conditions are never
//! evaluated (an undefined identifier in a skipped `$if` is not an error)
//! and none of its branches can activate. Chain *shape* errors (stray
//! directives, `$elseif`/`$else` after `$else`, trailing tokens on
//! `$else`/`$endif`, missing `$ifdef` names) are enforced even in skipped
//! regions: the directive line itself is always parsed, only its effect is
//! suppressed.
//!
//! A chain must open and close within one file: a `$endif` (or `$elseif*`/
//! `$else`) whose innermost open chain was opened by a different file is
//! stray, and a chain still open when its file ends is unterminated (the
//! error names the opening directive).
//!
//! ## The `$if` evaluator
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

use crate::lexer::{Position, Token, TokenKind};

use super::defines::{self, DefineTable};
use super::errors::PreprocessError;

/// Where a chain stands in its branch progression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Branch {
    /// No branch has been true yet — a later `$elseif*`/`$else` may still
    /// activate one. The current branch is being skipped.
    Pending,
    /// The current branch is the chain's one active branch.
    Live,
    /// A branch was already taken — everything up to `$endif` is skipped.
    Done,
}

/// One open `$if`/`$ifdef`/`$ifndef` chain.
#[derive(Debug)]
struct Frame {
    /// The opening directive's name, for unterminated-chain diagnostics.
    directive: &'static str,
    /// The opening directive's position; its `file_id` also pins the chain
    /// to the file it was opened in.
    opened: Position,
    /// Whether the surrounding context was active when the chain opened.
    /// `false` makes the whole chain inert: conditions are never evaluated
    /// and no branch ever activates — the frame exists only to keep
    /// nesting matched.
    parent_active: bool,
    /// Branch progression. Inert frames stay `Pending` forever.
    state: Branch,
    /// Set once `$else` is seen; later `$elseif*`/`$else` are errors.
    else_pos: Option<Position>,
}

/// The stack of open conditional chains. Owned by the driver
/// (`Preprocessor.conditionals`); every directive the driver routes here
/// mutates the top frame, and [`ConditionalStack::active`] gates the rest
/// of the driver's walk.
#[derive(Debug, Default)]
pub struct ConditionalStack {
    frames: Vec<Frame>,
}

impl ConditionalStack {
    /// True iff tokens currently flow to the output: no open chain, or the
    /// innermost chain's current branch is live under an active parent.
    /// (An inner frame's `parent_active` already folds in every outer
    /// frame, so only the top of the stack needs consulting.)
    pub(crate) fn active(&self) -> bool {
        self.frames
            .last()
            .is_none_or(|frame| frame.parent_active && frame.state == Branch::Live)
    }

    /// Number of open chains — snapshot at file entry, compare at file end.
    pub(crate) fn depth(&self) -> usize {
        self.frames.len()
    }

    /// The outermost chain opened at or beyond `baseline` that is still
    /// open — the driver's per-file unterminated-chain check.
    pub(crate) fn unterminated_since(&self, baseline: usize) -> Option<(&'static str, Position)> {
        self.frames
            .get(baseline)
            .map(|frame| (frame.directive, frame.opened))
    }

    /// Process one conditional directive line. `rest` is everything after
    /// the directive name, newline already stripped.
    pub(crate) fn handle(
        &mut self,
        directive: &str,
        rest: &[Token],
        pos: Position,
        defines: &DefineTable,
    ) -> Result<(), PreprocessError> {
        match directive {
            "ifdef" => self.open_ifdef("ifdef", rest, pos, defines),
            "ifndef" => self.open_ifdef("ifndef", rest, pos, defines),
            "if" => self.open_if(rest, pos, defines),
            "elseif" => self.chain_elseif(rest, pos, defines),
            "elseifdef" => self.chain_elseifdef(rest, pos, defines),
            "else" => self.chain_else(rest, pos),
            "endif" => self.chain_endif(rest, pos),
            other => unreachable!("driver routed non-conditional directive `${other}` here"),
        }
    }

    /// `$ifdef NAME` / `$ifndef NAME` — open a chain on definedness.
    fn open_ifdef(
        &mut self,
        directive: &'static str,
        rest: &[Token],
        pos: Position,
        defines: &DefineTable,
    ) -> Result<(), PreprocessError> {
        let name = single_name(directive, rest, pos)?;
        let parent_active = self.active();
        let taken = parent_active && (defines.is_defined(&name) == (directive == "ifdef"));
        self.push(directive, pos, parent_active, taken);
        Ok(())
    }

    /// `$if EXPR` — open a chain on a constant expression. The expression
    /// is only evaluated when the chain could actually activate; inside a
    /// skipped region it is inert (never an error).
    fn open_if(
        &mut self,
        rest: &[Token],
        pos: Position,
        defines: &DefineTable,
    ) -> Result<(), PreprocessError> {
        let parent_active = self.active();
        let taken = parent_active && eval_if_expr(rest, defines, pos)? != 0;
        self.push("if", pos, parent_active, taken);
        Ok(())
    }

    fn push(&mut self, directive: &'static str, pos: Position, parent_active: bool, taken: bool) {
        self.frames.push(Frame {
            directive,
            opened: pos,
            parent_active,
            state: if taken { Branch::Live } else { Branch::Pending },
            else_pos: None,
        });
    }

    /// `$elseif EXPR` — first-true-wins: evaluated only while the chain is
    /// still `Pending` under an active parent.
    fn chain_elseif(
        &mut self,
        rest: &[Token],
        pos: Position,
        defines: &DefineTable,
    ) -> Result<(), PreprocessError> {
        let frame = self.top("elseif", pos)?;
        if let Some(else_pos) = frame.else_pos {
            return Err(PreprocessError::ConditionalAfterElse {
                directive: "elseif",
                else_pos,
                pos,
            });
        }
        if frame.parent_active && frame.state == Branch::Pending {
            if eval_if_expr(rest, defines, pos)? != 0 {
                frame.state = Branch::Live;
            }
        } else if frame.state == Branch::Live {
            frame.state = Branch::Done;
        }
        Ok(())
    }

    /// `$elseifdef NAME` — the definedness form of `$elseif`. The name is
    /// validated even when the branch cannot activate (syntax is always
    /// checked; only evaluation is suppressed).
    fn chain_elseifdef(
        &mut self,
        rest: &[Token],
        pos: Position,
        defines: &DefineTable,
    ) -> Result<(), PreprocessError> {
        let frame = self.top("elseifdef", pos)?;
        if let Some(else_pos) = frame.else_pos {
            return Err(PreprocessError::ConditionalAfterElse {
                directive: "elseifdef",
                else_pos,
                pos,
            });
        }
        let name = single_name("elseifdef", rest, pos)?;
        if frame.parent_active && frame.state == Branch::Pending {
            if defines.is_defined(&name) {
                frame.state = Branch::Live;
            }
        } else if frame.state == Branch::Live {
            frame.state = Branch::Done;
        }
        Ok(())
    }

    /// `$else` — activates iff no branch was taken; at most one per chain.
    fn chain_else(&mut self, rest: &[Token], pos: Position) -> Result<(), PreprocessError> {
        let frame = self.top("else", pos)?;
        if let Some(else_pos) = frame.else_pos {
            return Err(PreprocessError::ConditionalAfterElse {
                directive: "else",
                else_pos,
                pos,
            });
        }
        if let Some(extra) = rest.first() {
            return Err(PreprocessError::TrailingTokens {
                directive: "else",
                pos: extra.pos,
            });
        }
        frame.else_pos = Some(pos);
        if frame.parent_active {
            frame.state = match frame.state {
                Branch::Pending => Branch::Live,
                Branch::Live | Branch::Done => Branch::Done,
            };
        }
        Ok(())
    }

    /// `$endif` — close the innermost chain.
    fn chain_endif(&mut self, rest: &[Token], pos: Position) -> Result<(), PreprocessError> {
        self.top("endif", pos)?;
        if let Some(extra) = rest.first() {
            return Err(PreprocessError::TrailingTokens {
                directive: "endif",
                pos: extra.pos,
            });
        }
        self.frames.pop();
        Ok(())
    }

    /// The innermost open chain, or a stray-directive error. A frame opened
    /// by a *different file* (an importer) is not this file's to continue —
    /// chains never span file boundaries.
    fn top(&mut self, directive: &'static str, pos: Position) -> Result<&mut Frame, PreprocessError> {
        match self.frames.last_mut() {
            Some(frame) if frame.opened.file_id == pos.file_id => Ok(frame),
            _ => Err(PreprocessError::StrayConditional { directive, pos }),
        }
    }
}

/// Parse `rest` as exactly one identifier — the shared operand shape of
/// `$ifdef` / `$ifndef` / `$elseifdef`.
fn single_name(
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
    defines: &DefineTable,
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
fn expand_operands(rest: &[Token], defines: &DefineTable) -> Result<Vec<Token>, PreprocessError> {
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
                defines::expand_into(&mut out, token, defines);
                i += 1;
            }
        }
    }
    Ok(out)
}

/// Pratt evaluator over the expanded token slice. Values are `i64`;
/// comparisons and logical operators yield 1/0.
struct Eval<'a> {
    tokens: &'a [Token],
    i: usize,
    /// Only consulted to word the identifier-operand error precisely.
    defines: &'a DefineTable,
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

impl<'a> Eval<'a> {
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
    use super::super::{Preprocessor, preprocess_str, preprocess_str_with};
    use super::*;
    use crate::lexer::tokenize;

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

    fn eval_with(defines: &DefineTable, expr: &str) -> Result<i64, PreprocessError> {
        let mut tokens = tokenize(expr.to_string()).unwrap();
        tokens.retain(|t| !matches!(t.kind, TokenKind::Newline | TokenKind::Eof));
        eval_if_expr(&tokens, defines, Position::start())
    }

    fn eval(expr: &str) -> Result<i64, PreprocessError> {
        eval_with(&DefineTable::new(), expr)
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

    #[test]
    fn platform_defines_reach_if_expressions() {
        // TJLB_VERSION_MAJOR is a builtin integer define.
        assert_eq!(ints("$if TJLB_VERSION_MAJOR >= 0\n1\n$endif\n"), vec![1]);
    }
}
