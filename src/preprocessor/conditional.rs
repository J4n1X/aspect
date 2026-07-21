//! `$ifdef` / `$ifndef` / `$if` / `$elseif` / `$elseifdef` / `$else` /
//! `$endif` — conditional compilation: chain tracking and the `$if`
//! constant-expression evaluator.
//!
//! A chain opens with `$ifdef`/`$ifndef`/`$if`, takes any mix of
//! `$elseif`/`$elseifdef` and at most one `$else`, and closes with `$endif`;
//! chains nest arbitrarily. Exactly one branch is active: the first whose
//! condition is true, or the `$else`.
//!
//! Inside a false branch ordinary tokens are dropped and non-conditional
//! directives are inert. Conditional directives are still processed so nesting
//! stays matched — but a chain opened inside a skipped region is *inert*: its
//! conditions are never evaluated (an undefined identifier in a skipped `$if`
//! is not an error) and no branch can activate. Chain *shape* errors are
//! enforced even in skipped regions: the directive line is always parsed, only
//! its effect suppressed.
//!
//! A chain must open and close within one file: a closing directive whose
//! innermost open chain belongs to a different file is stray, and a chain
//! still open when its file ends is unterminated.
//!
//! `$if EXPR` evaluates a constant integer expression; see [`super::expr_eval`]
//! for its grammar, precedence, and error semantics.

use crate::lexer::{Position, Token};

use super::defines::ScopedDefines;
use super::errors::PreprocessError;
use super::expr_eval::{eval_if_expr, single_name};

/// Where a chain stands in its branch progression.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Branch {
    /// No branch has been true yet — a later `$elseif*`/`$else` may still
    /// activate one. The current branch is being skipped.
    Pending,
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

/// The stack of open conditional chains, owned by the driver. Every routed
/// directive mutates the top frame; `active` gates the rest of the walk.
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

    /// `rest` is everything after the directive name, newline already stripped.
    pub(crate) fn handle(
        &mut self,
        directive: &str,
        rest: &[Token],
        pos: Position,
        defines: &ScopedDefines,
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
        defines: &ScopedDefines,
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
        defines: &ScopedDefines,
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
        defines: &ScopedDefines,
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
        defines: &ScopedDefines,
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

#[cfg(test)]
mod tests;
