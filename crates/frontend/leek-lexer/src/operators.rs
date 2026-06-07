//! Multi-character operator and punctuation lexing — each
//! disambiguation lives in its own `op_*` method to keep
//! [`Lexer::next_token`](crate::Lexer::next_token) flat and
//! readable.

use leek_syntax::SyntaxKind;

use crate::Lexer;

impl Lexer<'_> {
    pub(crate) fn dot_or_dotdot(&mut self, start: usize) {
        if self.peek_at(1) == Some(b'.') {
            self.push(SyntaxKind::DotDot, start, 2);
        } else {
            self.single(SyntaxKind::Dot, start);
        }
    }

    pub(crate) fn op_plus(&mut self, start: usize) {
        match self.peek_at(1) {
            Some(b'+') => self.push(SyntaxKind::PlusPlus, start, 2),
            Some(b'=') => self.push(SyntaxKind::PlusEq, start, 2),
            _ => self.single(SyntaxKind::Plus, start),
        }
    }

    pub(crate) fn op_minus(&mut self, start: usize) {
        match self.peek_at(1) {
            Some(b'-') => self.push(SyntaxKind::MinusMinus, start, 2),
            Some(b'=') => self.push(SyntaxKind::MinusEq, start, 2),
            Some(b'>') => self.push(SyntaxKind::Arrow, start, 2),
            _ => self.single(SyntaxKind::Minus, start),
        }
    }

    pub(crate) fn op_star(&mut self, start: usize) {
        match (self.peek_at(1), self.peek_at(2)) {
            (Some(b'*'), Some(b'=')) => self.push(SyntaxKind::StarStarEq, start, 3),
            (Some(b'*'), _) => self.push(SyntaxKind::StarStar, start, 2),
            (Some(b'='), _) => self.push(SyntaxKind::StarEq, start, 2),
            _ => self.single(SyntaxKind::Star, start),
        }
    }

    pub(crate) fn op_backslash(&mut self, start: usize) {
        if self.peek_at(1) == Some(b'=') {
            self.push(SyntaxKind::BackslashEq, start, 2);
        } else {
            self.single(SyntaxKind::Backslash, start);
        }
    }

    pub(crate) fn op_percent(&mut self, start: usize) {
        if self.peek_at(1) == Some(b'=') {
            self.push(SyntaxKind::PercentEq, start, 2);
        } else {
            self.single(SyntaxKind::Percent, start);
        }
    }

    pub(crate) fn op_eq(&mut self, start: usize) {
        match (self.peek_at(1), self.peek_at(2)) {
            (Some(b'='), Some(b'=')) => self.push(SyntaxKind::EqEqEq, start, 3),
            (Some(b'='), _) => self.push(SyntaxKind::EqEq, start, 2),
            (Some(b'>'), _) => self.push(SyntaxKind::FatArrow, start, 2),
            _ => self.single(SyntaxKind::Eq, start),
        }
    }

    pub(crate) fn op_bang(&mut self, start: usize) {
        match (self.peek_at(1), self.peek_at(2)) {
            (Some(b'='), Some(b'=')) => self.push(SyntaxKind::NotEqEq, start, 3),
            (Some(b'='), _) => self.push(SyntaxKind::NotEq, start, 2),
            _ => self.single(SyntaxKind::Bang, start),
        }
    }

    pub(crate) fn op_lt(&mut self, start: usize) {
        match (self.peek_at(1), self.peek_at(2)) {
            (Some(b'<'), Some(b'=')) => self.push(SyntaxKind::ShiftLeftEq, start, 3),
            (Some(b'<'), _) => self.push(SyntaxKind::ShiftLeft, start, 2),
            (Some(b'='), _) => self.push(SyntaxKind::Le, start, 2),
            _ => self.single(SyntaxKind::Lt, start),
        }
    }

    pub(crate) fn op_gt(&mut self, start: usize) {
        match (self.peek_at(1), self.peek_at(2), self.peek_at(3)) {
            (Some(b'>'), Some(b'>'), Some(b'=')) => self.push(SyntaxKind::UShiftRightEq, start, 4),
            (Some(b'>'), Some(b'>'), _) => self.push(SyntaxKind::UShiftRight, start, 3),
            (Some(b'>'), Some(b'='), _) => self.push(SyntaxKind::ShiftRightEq, start, 3),
            (Some(b'>'), _, _) => self.push(SyntaxKind::ShiftRight, start, 2),
            (Some(b'='), _, _) => self.push(SyntaxKind::Ge, start, 2),
            _ => self.single(SyntaxKind::Gt, start),
        }
    }

    pub(crate) fn op_amp(&mut self, start: usize) {
        match self.peek_at(1) {
            Some(b'&') => self.push(SyntaxKind::AmpAmp, start, 2),
            Some(b'=') => self.push(SyntaxKind::AmpEq, start, 2),
            _ => self.single(SyntaxKind::Amp, start),
        }
    }

    pub(crate) fn op_pipe(&mut self, start: usize) {
        match self.peek_at(1) {
            Some(b'|') => self.push(SyntaxKind::PipePipe, start, 2),
            Some(b'=') => self.push(SyntaxKind::PipeEq, start, 2),
            _ => self.single(SyntaxKind::Pipe, start),
        }
    }

    pub(crate) fn op_caret(&mut self, start: usize) {
        if self.peek_at(1) == Some(b'=') {
            self.push(SyntaxKind::CaretEq, start, 2);
        } else {
            self.single(SyntaxKind::Caret, start);
        }
    }

    pub(crate) fn op_question(&mut self, start: usize) {
        match (self.peek_at(1), self.peek_at(2)) {
            (Some(b'?'), Some(b'=')) => self.push(SyntaxKind::QuestionQuestionEq, start, 3),
            (Some(b'?'), _) => self.push(SyntaxKind::QuestionQuestion, start, 2),
            _ => self.single(SyntaxKind::Question, start),
        }
    }
}
