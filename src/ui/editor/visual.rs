//! Visual-mode dispatcher and the operator path that applies to the
//! current selection.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::mode::Mode;
use super::motion;
use super::normal::Operator;
use super::util::{matches_chord_resolution, motion_from_keycode, step_forward_one};
use super::EditorState;

impl EditorState {
    /// Visual-mode dispatcher. Motions and gg/G extend the selection
    /// (no `cancel_selection`); operators apply to the current
    /// selection and exit Visual.
    pub(super) fn handle_visual_key(&mut self, key: KeyEvent) -> bool {
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            self.pending_count = 0;
            self.pending_chord = None;
            return false;
        }

        // Esc / `v` toggle out.
        if matches!(key.code, KeyCode::Esc | KeyCode::Char('v')) {
            self.exit_visual();
            return false;
        }

        // Operators apply to current selection.
        if let KeyCode::Char(c) = key.code {
            // Visual `s` / `x` are aliases for change / delete.
            let op = match c {
                'x' => Some(Operator::Delete),
                's' => Some(Operator::Change),
                other => Operator::from_char(other),
            };
            if let Some(op) = op {
                return self.apply_op_to_visual(op);
            }
        }

        // Resolve gg chord inside Visual — selection follows the
        // cursor without `cancel_selection`.
        if matches_chord_resolution(self.pending_chord.take(), key) {
            let row = self.take_gg_target_row();
            self.buf.set_cursor(row, 0);
            return false;
        }

        if self.try_accumulate_digit(key) {
            return false;
        }

        if let Some(motion) = motion_from_keycode(key.code) {
            let count = self.take_count();
            let target = motion::apply(&self.buf, motion, count);
            // Keep selection (anchor stays put).
            self.buf.set_cursor(target.row, target.col);
            return false;
        }

        if matches!(key.code, KeyCode::Char('G')) {
            let row = self.take_capital_g_target_row();
            self.buf.set_cursor(row, 0);
            return false;
        }
        if matches!(key.code, KeyCode::Char('g')) {
            self.pending_chord = Some('g');
            return false;
        }

        self.pending_count = 0;
        false
    }

    pub(super) fn enter_visual(&mut self) {
        self.mode = Mode::Visual;
        self.buf.cancel_selection();
        self.buf.start_selection();
    }

    fn exit_visual(&mut self) {
        self.mode = Mode::Normal;
        self.buf.cancel_selection();
        self.pending_count = 0;
        self.pending_chord = None;
    }

    fn apply_op_to_visual(&mut self, op: Operator) -> bool {
        let Some((s, e)) = self.buf.selection_range() else {
            self.exit_visual();
            return false;
        };
        // Visual selection is char-wise inclusive on both ends in
        // vim; bump end forward by one slot so the right edge char
        // is part of the range.
        let e = step_forward_one(self.buf.lines(), e);
        self.register = self.buf.text_in_range(s, e);
        let dirty = match op {
            Operator::Yank => false,
            Operator::Delete => {
                self.delete_range(s, e);
                true
            }
            Operator::Change => {
                self.delete_range(s, e);
                self.mode = Mode::Insert;
                self.buf.cancel_selection();
                self.pending_count = 0;
                self.pending_chord = None;
                return true;
            }
        };
        self.exit_visual();
        dirty
    }
}
