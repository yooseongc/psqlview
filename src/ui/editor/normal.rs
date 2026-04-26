//! Normal-mode + operator-pending dispatch.
//!
//! `Normal` and `Op-pending` are separate dispatchers but share most
//! of the helpers (count accumulation, gg/G targets, linewise range
//! math, the `Operator` enum). They live together here so the count/
//! chord state machine stays in one file.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::buffer::Cursor;
use super::mode::Mode;
use super::motion::{self, Motion};
use super::text_object;
use super::util::{
    cursor_le, insert_text, matches_chord_resolution, motion_from_keycode, motion_inclusive,
    step_forward_one,
};
use super::EditorState;

/// Operator awaiting a target — set by `d` / `y` / `c` and consumed
/// by the next motion, text-object, or repeat (`dd` / `yy` / `cc`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Operator {
    Delete,
    Yank,
    Change,
}

impl Operator {
    pub(super) fn from_char(c: char) -> Option<Self> {
        match c {
            'd' => Some(Self::Delete),
            'y' => Some(Self::Yank),
            'c' => Some(Self::Change),
            _ => None,
        }
    }
}

impl EditorState {
    /// Normal-mode dispatcher. Dispatch order:
    /// 1. Modifier combos (Ctrl / Alt) — reset transient state and drop.
    /// 2. Operator-pending state (`d` / `y` / `c` already pressed) —
    ///    next key chooses the target (motion, text object, repeat
    ///    `dd`/`yy`/`cc`, or `gg`/`G` for linewise jumps).
    /// 3. Pending chord (`g` for `gg`).
    /// 4. Digit accumulation.
    /// 5. Motions, `G` / `gg`, `v` enter Visual, `d`/`y`/`c` start
    ///    operator, `p`/`P` paste, `x` delete-char, mode-entry
    ///    primitives, unmapped keys swallowed.
    pub(super) fn handle_normal_key(&mut self, key: KeyEvent) -> bool {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            self.reset_pending();
            return false;
        }

        // ------ operator-pending (d / y / c already pressed) --------
        if self.pending_op.is_some() {
            return self.handle_op_pending_key(key);
        }

        // ------ pending chord (gg, no operator) ---------------------
        if matches_chord_resolution(self.pending_chord.take(), key) {
            let row = self.take_gg_target_row();
            self.buf.cancel_selection();
            self.buf.set_cursor(row, 0);
            return false;
        }

        // ------ digit accumulation ----------------------------------
        if self.try_accumulate_digit(key) {
            return false;
        }

        // ------ motions ---------------------------------------------
        if let Some(motion) = motion_from_keycode(key.code) {
            let count = self.take_count();
            let target = motion::apply(&self.buf, motion, count);
            self.buf.cancel_selection();
            self.buf.set_cursor(target.row, target.col);
            return false;
        }

        // ------ goto-line ('G' bare = last line) --------------------
        if matches!(key.code, KeyCode::Char('G')) {
            let row = self.take_capital_g_target_row();
            self.buf.cancel_selection();
            self.buf.set_cursor(row, 0);
            return false;
        }
        if matches!(key.code, KeyCode::Char('g')) {
            self.pending_chord = Some('g');
            return false;
        }

        // ------ Visual mode entry -----------------------------------
        if matches!(key.code, KeyCode::Char('v')) {
            self.pending_count = 0;
            self.enter_visual();
            return false;
        }

        // ------ operators / paste / x -------------------------------
        if let KeyCode::Char(c) = key.code {
            if let Some(op) = Operator::from_char(c) {
                self.pending_op = Some(op);
                return false;
            }
            if c == 'x' {
                let cur = self.buf.cursor();
                let line_len = self.buf.line_chars(cur.row);
                if cur.col >= line_len {
                    self.pending_count = 0;
                    return false;
                }
                let count = self.pending_count.max(1) as usize;
                self.pending_count = 0;
                let end_col = (cur.col + count).min(line_len);
                let s = cur;
                let e = Cursor::new(cur.row, end_col);
                return self.apply_op_range(Operator::Delete, s, e);
            }
            if c == 'p' {
                self.pending_count = 0;
                return self.paste(true);
            }
            if c == 'P' {
                self.pending_count = 0;
                return self.paste(false);
            }
        }

        // ------ mode-entry primitives -------------------------------
        match key.code {
            KeyCode::Char('i') => {
                self.pending_count = 0;
                self.mode = Mode::Insert;
                false
            }
            KeyCode::Char('a') => {
                self.pending_count = 0;
                self.buf.cancel_selection();
                let c = self.buf.cursor();
                let line_len = self.buf.line_chars(c.row);
                self.buf.set_cursor(c.row, (c.col + 1).min(line_len));
                self.mode = Mode::Insert;
                false
            }
            KeyCode::Char('I') => {
                self.pending_count = 0;
                self.buf.cancel_selection();
                let row = self.buf.cursor().row;
                self.buf.set_cursor(row, 0);
                self.mode = Mode::Insert;
                false
            }
            KeyCode::Char('A') => {
                self.pending_count = 0;
                self.buf.cancel_selection();
                let row = self.buf.cursor().row;
                let line_len = self.buf.line_chars(row);
                self.buf.set_cursor(row, line_len);
                self.mode = Mode::Insert;
                false
            }
            KeyCode::Char('o') => {
                self.pending_count = 0;
                let pre = self.buf.clone();
                self.buf.cancel_selection();
                let row = self.buf.cursor().row;
                let line_len = self.buf.line_chars(row);
                self.buf.set_cursor(row, line_len);
                self.buf.insert_newline();
                self.undo.record(&pre);
                self.mode = Mode::Insert;
                true
            }
            KeyCode::Char('O') => {
                self.pending_count = 0;
                let pre = self.buf.clone();
                self.buf.cancel_selection();
                let row = self.buf.cursor().row;
                self.buf.set_cursor(row, 0);
                self.buf.insert_newline();
                self.buf.set_cursor(row, 0);
                self.undo.record(&pre);
                self.mode = Mode::Insert;
                true
            }
            _ => {
                self.pending_count = 0;
                false
            }
        }
    }

    /// Operator-pending handler. Splits out from `handle_normal_key`
    /// because the dispatch ordering is different (chord resolves
    /// inside the operator scope, scope hint queues `i`/`a` instead
    /// of entering Insert, etc.).
    fn handle_op_pending_key(&mut self, key: KeyEvent) -> bool {
        let op = self.pending_op.expect("called with no pending op");

        // Text-object scope previously queued: this key is the object char.
        if let Some(scope) = self.pending_obj_scope.take() {
            self.pending_op = None;
            let obj_char = match key.code {
                KeyCode::Char(c) => c,
                _ => {
                    self.reset_pending();
                    return false;
                }
            };
            self.pending_count = 0;
            if let Some((s, e)) = text_object::resolve(&self.buf, scope, obj_char) {
                return self.apply_op_range(op, s, e);
            }
            return false;
        }

        // Resolve gg-style chord inside operator-pending: `dgg` /
        // `ygg` / `cgg` go linewise from cursor row to the target line.
        if matches_chord_resolution(self.pending_chord.take(), key) {
            let target_row = self.take_gg_target_row();
            let (s, e) = self.linewise_range_to_row(target_row);
            return self.apply_op_range(op, s, e);
        }

        // Esc cancels the operator.
        if matches!(key.code, KeyCode::Esc) {
            self.reset_pending();
            return false;
        }

        // Repeat key (dd / yy / cc) — linewise from cursor.
        if matches!(key.code, KeyCode::Char(c) if Operator::from_char(c) == Some(op)) {
            let count = self.take_count();
            let (s, e) = self.linewise_range(count);
            return self.apply_op_range(op, s, e);
        }

        // Inner / Around scope hint.
        if matches!(key.code, KeyCode::Char('i')) {
            self.pending_obj_scope = Some(text_object::Scope::Inner);
            return false;
        }
        if matches!(key.code, KeyCode::Char('a')) {
            self.pending_obj_scope = Some(text_object::Scope::Around);
            return false;
        }

        // Counts inside operator scope (e.g. `d3w`).
        if self.try_accumulate_digit(key) {
            return false;
        }

        // Motion-driven range.
        if let Some(motion) = motion_from_keycode(key.code) {
            let count = self.take_count();
            let (s, e) = self.range_for_motion(motion, count);
            return self.apply_op_range(op, s, e);
        }

        // `dG` / `yG` / `cG` — linewise to last line (or count).
        if matches!(key.code, KeyCode::Char('G')) {
            let target_row = self.take_capital_g_target_row();
            let (s, e) = self.linewise_range_to_row(target_row);
            return self.apply_op_range(op, s, e);
        }

        // `dg` / `yg` / `cg` start the chord (resolves on next `g`).
        if matches!(key.code, KeyCode::Char('g')) {
            self.pending_chord = Some('g');
            return false;
        }

        // Anything else cancels the pending operator.
        self.reset_pending();
        false
    }

    pub(super) fn try_accumulate_digit(&mut self, key: KeyEvent) -> bool {
        match key.code {
            KeyCode::Char(c @ '1'..='9') => {
                let d = (c as u8 - b'0') as u32;
                self.pending_count = self.pending_count.saturating_mul(10).saturating_add(d);
                true
            }
            KeyCode::Char('0') if self.pending_count > 0 => {
                self.pending_count = self.pending_count.saturating_mul(10);
                true
            }
            _ => false,
        }
    }

    /// Consumes the pending count and clears every other pending
    /// state — operator, object scope, chord. Returns at least 1
    /// so motion handlers can pass it straight through. Called from
    /// every dispatcher branch that *terminates* a key sequence
    /// (motion, linewise, mode entry); branches that *queue* state
    /// (chord starter, scope hint) must skip it.
    pub(super) fn take_count(&mut self) -> usize {
        let n = self.pending_count.max(1) as usize;
        self.reset_pending();
        n
    }

    /// Row index for `gg` / `5gg` (target = pending count or 1).
    /// Consumes pending state and clamps into the buffer's row range.
    pub(super) fn take_gg_target_row(&mut self) -> usize {
        let line = self.pending_count.max(1) as usize;
        self.reset_pending();
        line.saturating_sub(1)
            .min(self.buf.line_count().saturating_sub(1))
    }

    /// Row index for `G` / `5G` (target = pending count or last line).
    /// Consumes pending state and clamps into the buffer's row range.
    pub(super) fn take_capital_g_target_row(&mut self) -> usize {
        let line = if self.pending_count > 0 {
            self.pending_count as usize
        } else {
            self.buf.line_count()
        };
        self.reset_pending();
        line.saturating_sub(1)
            .min(self.buf.line_count().saturating_sub(1))
    }

    pub(super) fn reset_pending(&mut self) {
        self.pending_count = 0;
        self.pending_chord = None;
        self.pending_op = None;
        self.pending_obj_scope = None;
    }

    pub(super) fn apply_op_range(&mut self, op: Operator, start: Cursor, end: Cursor) -> bool {
        if start == end {
            return false;
        }
        self.register = self.buf.text_in_range(start, end);
        match op {
            Operator::Yank => false,
            Operator::Delete => {
                self.delete_range(start, end);
                true
            }
            Operator::Change => {
                self.delete_range(start, end);
                self.mode = Mode::Insert;
                true
            }
        }
    }

    pub(super) fn delete_range(&mut self, start: Cursor, end: Cursor) {
        let pre = self.buf.clone();
        self.buf.cancel_selection();
        self.buf.delete_range(start, end);
        self.buf.set_cursor(start.row, start.col);
        self.undo.record(&pre);
    }

    /// Paste the unnamed register at the cursor. `after = true`
    /// shifts past the current char first (vim `p`); `false` inserts
    /// at the cursor (vim `P`). Borrows the register out via
    /// `mem::take` so the buffer-mutation borrow doesn't conflict
    /// with reading it.
    fn paste(&mut self, after: bool) -> bool {
        if self.register.is_empty() {
            return false;
        }
        let pre = self.buf.clone();
        if after {
            let cur = self.buf.cursor();
            let line_len = self.buf.line_chars(cur.row);
            if cur.col < line_len {
                self.buf.set_cursor(cur.row, cur.col + 1);
            }
        }
        let text = std::mem::take(&mut self.register);
        insert_text(&mut self.buf, &text);
        self.register = text;
        self.undo.record(&pre);
        true
    }

    fn range_for_motion(&self, motion: Motion, count: usize) -> (Cursor, Cursor) {
        let cursor = self.buf.cursor();
        let target = motion::apply(&self.buf, motion, count);
        let (lo, hi) = if cursor_le(cursor, target) {
            (cursor, target)
        } else {
            (target, cursor)
        };
        if motion_inclusive(motion) {
            let bumped = step_forward_one(self.buf.lines(), hi);
            (lo, bumped)
        } else {
            (lo, hi)
        }
    }

    fn linewise_range(&self, count: usize) -> (Cursor, Cursor) {
        let cur_row = self.buf.cursor().row;
        let last_row =
            (cur_row + count.saturating_sub(1)).min(self.buf.line_count().saturating_sub(1));
        self.linewise_range_inclusive(cur_row, last_row)
    }

    pub(super) fn linewise_range_to_row(&self, target_row: usize) -> (Cursor, Cursor) {
        let cur_row = self.buf.cursor().row;
        let (lo, hi) = if cur_row <= target_row {
            (cur_row, target_row)
        } else {
            (target_row, cur_row)
        };
        self.linewise_range_inclusive(lo, hi)
    }

    fn linewise_range_inclusive(&self, lo: usize, hi: usize) -> (Cursor, Cursor) {
        let total = self.buf.line_count();
        if hi + 1 < total {
            (Cursor::new(lo, 0), Cursor::new(hi + 1, 0))
        } else if lo > 0 {
            (
                Cursor::new(lo - 1, self.buf.line_chars(lo - 1)),
                Cursor::new(hi, self.buf.line_chars(hi)),
            )
        } else {
            (Cursor::new(0, 0), Cursor::new(hi, self.buf.line_chars(hi)))
        }
    }
}
