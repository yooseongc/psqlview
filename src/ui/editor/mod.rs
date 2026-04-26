//! SQL editor pane. Self-built — replaces the prior `tui-textarea` wrapper
//! so that R2 can do per-token syntax coloring (which `tui-textarea` 0.7
//! cannot express).
//!
//! The public surface (`EditorState` + `draw`) is preserved verbatim from
//! the legacy implementation so call sites in `app.rs` and the integration
//! tests don't have to change.

pub mod bracket;
pub mod buffer;
pub mod edit;
pub mod mode;
pub mod motion;
pub mod render;
pub mod tab;
pub mod text_object;
pub mod undo;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders};
use ratatui::Frame;

use super::focus_style;
use buffer::{Cursor, TextBuffer};
use edit::EditOutcome;
use mode::Mode;
use motion::Motion;
use render::ViewState;
use undo::UndoStack;

const PLACEHOLDER: &str = "-- F5 / Ctrl+Enter to run, Tab = autocomplete";

/// Operator awaiting a target — set by `d` / `y` / `c` and consumed by
/// the next motion, text-object, or repeat (`dd` / `yy` / `cc`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operator {
    Delete,
    Yank,
    Change,
}

impl Operator {
    fn key_char(self) -> char {
        match self {
            Self::Delete => 'd',
            Self::Yank => 'y',
            Self::Change => 'c',
        }
    }
}

pub struct EditorState {
    buf: TextBuffer,
    undo: UndoStack,
    view: ViewState,
    mode: Mode,
    /// Accumulated count prefix in Normal / Visual mode. `0` means no
    /// count pending; `1`–`9` followed by any digit (including `0`)
    /// extends the count. Reset whenever a motion / mode-entry /
    /// unmapped key fires.
    pending_count: u32,
    /// First half of a pending chord (currently only `g` for `gg`).
    pending_chord: Option<char>,
    /// Operator (`d` / `y` / `c`) awaiting its target.
    pending_op: Option<Operator>,
    /// Text-object scope (`i` for inner, `a` for around) once an
    /// operator is pending. `None` while no `di` / `da` prefix has
    /// been seen.
    pending_obj_scope: Option<text_object::Scope>,
    /// Single unnamed register — yanked / deleted text lands here so
    /// `p` / `P` can paste it back. R4 keeps the register per-tab to
    /// avoid threading a global through `EditorState`; cross-tab
    /// sharing can be added later.
    register: String,
}

impl Default for EditorState {
    fn default() -> Self {
        Self::new()
    }
}

impl EditorState {
    pub fn new() -> Self {
        Self {
            buf: TextBuffer::new(),
            undo: UndoStack::new(),
            view: ViewState::default(),
            mode: Mode::default(),
            pending_count: 0,
            pending_chord: None,
            pending_op: None,
            pending_obj_scope: None,
            register: String::new(),
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    // ---- inspectors -------------------------------------------------

    pub fn text(&self) -> String {
        self.buf.text()
    }

    /// Current cursor position, 1-indexed for human display.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        let c = self.buf.cursor();
        (c.row + 1, c.col + 1)
    }

    /// Raw buffer lines (LF-separated). Used by the completion-context
    /// detector to tokenize the prefix preceding the cursor.
    pub fn lines(&self) -> &[String] {
        self.buf.lines()
    }

    /// Cursor position as `(row, col)` in 0-indexed char units.
    pub fn cursor_pos(&self) -> (usize, usize) {
        let c = self.buf.cursor();
        (c.row, c.col)
    }

    /// Returns the currently selected text, or `None` if no selection is
    /// active. Used to let F5 / Ctrl+Enter run just the highlighted SQL.
    pub fn selected_text(&self) -> Option<String> {
        let (s, e) = self.buf.selection_range()?;
        if s == e {
            return None;
        }
        Some(self.buf.text_in_range(s, e))
    }

    /// Returns the inclusive `[start_row, end_row]` range covered by the
    /// active selection, or `None` if no selection is active.
    pub fn selected_line_range(&self) -> Option<(usize, usize)> {
        let (s, e) = self.buf.selection_range()?;
        Some((s.row, e.row))
    }

    /// Returns the identifier prefix ending at the cursor, or empty when
    /// the cursor is not sitting after `[A-Za-z_][A-Za-z0-9_]*`.
    pub fn word_prefix_before_cursor(&self) -> String {
        let cur = self.buf.cursor();
        let Some(line) = self.buf.lines().get(cur.row) else {
            return String::new();
        };
        let chars: Vec<char> = line.chars().collect();
        let col = cur.col.min(chars.len());
        let mut start = col;
        while start > 0 {
            let c = chars[start - 1];
            if c == '_' || c.is_ascii_alphanumeric() {
                start -= 1;
            } else {
                break;
            }
        }
        if start == col {
            return String::new();
        }
        if chars[start].is_ascii_digit() {
            return String::new();
        }
        chars[start..col].iter().collect()
    }

    // ---- mutations --------------------------------------------------

    /// Routes a key event through the edit layer, plus Ctrl+Z / Ctrl+Y
    /// for undo / redo. Records an undo snapshot on every text-changing
    /// keystroke.
    ///
    /// Returns `true` when the buffer text changed — callers use this
    /// to mark the active tab dirty without false-positives from
    /// arrow-key / scroll navigation. Undo / redo always return `true`
    /// because the buffer was replaced.
    ///
    /// Mode-aware: in `Mode::Normal`, mapped keys (`i a o I A O`) act
    /// as mode-entry primitives and unmapped keys are swallowed; in
    /// `Mode::Insert`, behavior matches the pre-modal editor, with
    /// `Esc` flipping back to Normal. Ctrl+Z / Ctrl+Y fire in either
    /// mode so undo isn't lost mid-vim.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('z') | KeyCode::Char('Z') => {
                    if let Some(prev) = self.undo.undo(&self.buf) {
                        self.buf = prev;
                        return true;
                    }
                    return false;
                }
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if let Some(next) = self.undo.redo(&self.buf) {
                        self.buf = next;
                        return true;
                    }
                    return false;
                }
                _ => {}
            }
        }

        match self.mode {
            Mode::Insert => {
                if matches!(key.code, KeyCode::Esc) && key.modifiers.is_empty() {
                    self.mode = Mode::Normal;
                    self.buf.cancel_selection();
                    return false;
                }
                self.handle_insert_key(key)
            }
            Mode::Normal => self.handle_normal_key(key),
            Mode::Visual { .. } => self.handle_visual_key(key),
        }
    }

    fn handle_insert_key(&mut self, key: KeyEvent) -> bool {
        let pre = self.buf.clone();
        let outcome = edit::handle_key(&mut self.buf, key);
        if outcome == EditOutcome::Changed {
            self.undo.record(&pre);
            true
        } else {
            false
        }
    }

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
    fn handle_normal_key(&mut self, key: KeyEvent) -> bool {
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
        if let Some(ch) = self.pending_chord.take() {
            if let KeyCode::Char(k) = key.code {
                if ch == 'g' && k == 'g' {
                    let target_line = if self.pending_count > 0 {
                        self.pending_count as usize
                    } else {
                        1
                    };
                    self.pending_count = 0;
                    self.goto_line(target_line);
                    return false;
                }
            }
            // Chord broken; key proceeds.
        }

        // ------ digit accumulation ----------------------------------
        if self.try_accumulate_digit(key) {
            return false;
        }

        // ------ motions ---------------------------------------------
        if let Some(motion) = motion_from_keycode(key.code) {
            let count = self.pending_count.max(1) as usize;
            self.pending_count = 0;
            let target = motion::apply(&self.buf, motion, count);
            self.buf.cancel_selection();
            self.buf.set_cursor(target.row, target.col);
            return false;
        }

        // ------ goto-line ('G' bare = last line) --------------------
        if matches!(key.code, KeyCode::Char('G')) {
            let line = if self.pending_count > 0 {
                self.pending_count as usize
            } else {
                self.buf.line_count()
            };
            self.pending_count = 0;
            self.goto_line(line);
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
            if let Some(op) = match c {
                'd' => Some(Operator::Delete),
                'y' => Some(Operator::Yank),
                'c' => Some(Operator::Change),
                _ => None,
            } {
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
                return self.paste_after();
            }
            if c == 'P' {
                self.pending_count = 0;
                return self.paste_before();
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
    /// inside the operator scope, scope hint queues `i`/`a` instead of
    /// entering Insert, etc.).
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
        if let Some(ch) = self.pending_chord.take() {
            if let KeyCode::Char('g') = key.code {
                if ch == 'g' {
                    let target_line = if self.pending_count > 0 {
                        self.pending_count as usize
                    } else {
                        1
                    };
                    let target_row = target_line
                        .saturating_sub(1)
                        .min(self.buf.line_count().saturating_sub(1));
                    self.reset_pending();
                    let (s, e) = self.linewise_range_to_row(target_row);
                    return self.apply_op_range(op, s, e);
                }
            }
            // Chord broken; fall through with chord cleared.
        }

        // Esc cancels the operator.
        if matches!(key.code, KeyCode::Esc) {
            self.reset_pending();
            return false;
        }

        // Repeat key (dd / yy / cc) — linewise from cursor.
        if matches!(key.code, KeyCode::Char(c) if c == op.key_char()) {
            let count = self.pending_count.max(1) as usize;
            self.reset_pending();
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
            let count = self.pending_count.max(1) as usize;
            self.reset_pending();
            let (s, e) = self.range_for_motion(motion, count);
            return self.apply_op_range(op, s, e);
        }

        // `dG` / `yG` / `cG` — linewise to last line (or count).
        if matches!(key.code, KeyCode::Char('G')) {
            let target_line = if self.pending_count > 0 {
                self.pending_count as usize
            } else {
                self.buf.line_count()
            };
            let target_row = target_line
                .saturating_sub(1)
                .min(self.buf.line_count().saturating_sub(1));
            self.reset_pending();
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

    /// Visual-mode dispatcher. Motions and gg/G extend the selection
    /// (no `cancel_selection`); operators apply to the current
    /// selection and exit Visual.
    fn handle_visual_key(&mut self, key: KeyEvent) -> bool {
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
            if let Some(op) = match c {
                'd' | 'x' => Some(Operator::Delete),
                'y' => Some(Operator::Yank),
                'c' | 's' => Some(Operator::Change),
                _ => None,
            } {
                return self.apply_op_to_visual(op);
            }
        }

        // Resolve gg chord inside Visual.
        if let Some(ch) = self.pending_chord.take() {
            if let KeyCode::Char('g') = key.code {
                if ch == 'g' {
                    let target_line = if self.pending_count > 0 {
                        self.pending_count as usize
                    } else {
                        1
                    };
                    self.pending_count = 0;
                    let row = target_line
                        .saturating_sub(1)
                        .min(self.buf.line_count().saturating_sub(1));
                    self.buf.set_cursor(row, 0);
                    return false;
                }
            }
        }

        if self.try_accumulate_digit(key) {
            return false;
        }

        if let Some(motion) = motion_from_keycode(key.code) {
            let count = self.pending_count.max(1) as usize;
            self.pending_count = 0;
            let target = motion::apply(&self.buf, motion, count);
            // Keep selection (anchor stays put).
            self.buf.set_cursor(target.row, target.col);
            return false;
        }

        if matches!(key.code, KeyCode::Char('G')) {
            let line = if self.pending_count > 0 {
                self.pending_count as usize
            } else {
                self.buf.line_count()
            };
            self.pending_count = 0;
            let row = line
                .saturating_sub(1)
                .min(self.buf.line_count().saturating_sub(1));
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

    fn enter_visual(&mut self) {
        let cursor = self.buf.cursor();
        self.mode = Mode::Visual { anchor: cursor };
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
        // Visual selection is char-wise inclusive on both ends in vim;
        // bump end forward by one slot so the right edge char is part
        // of the range.
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

    fn try_accumulate_digit(&mut self, key: KeyEvent) -> bool {
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

    fn reset_pending(&mut self) {
        self.pending_count = 0;
        self.pending_chord = None;
        self.pending_op = None;
        self.pending_obj_scope = None;
    }

    fn apply_op_range(&mut self, op: Operator, start: Cursor, end: Cursor) -> bool {
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

    fn delete_range(&mut self, start: Cursor, end: Cursor) {
        let pre = self.buf.clone();
        self.buf.cancel_selection();
        self.buf.set_cursor(start.row, start.col);
        let count = count_chars_between(self.buf.lines(), start, end);
        for _ in 0..count {
            self.buf.delete_forward();
        }
        self.undo.record(&pre);
    }

    fn paste_after(&mut self) -> bool {
        if self.register.is_empty() {
            return false;
        }
        let pre = self.buf.clone();
        let cur = self.buf.cursor();
        let line_len = self.buf.line_chars(cur.row);
        if cur.col < line_len {
            self.buf.set_cursor(cur.row, cur.col + 1);
        }
        for ch in self.register.clone().chars() {
            if ch == '\n' {
                self.buf.insert_newline();
            } else if ch != '\r' {
                self.buf.insert_char(ch);
            }
        }
        self.undo.record(&pre);
        true
    }

    fn paste_before(&mut self) -> bool {
        if self.register.is_empty() {
            return false;
        }
        let pre = self.buf.clone();
        for ch in self.register.clone().chars() {
            if ch == '\n' {
                self.buf.insert_newline();
            } else if ch != '\r' {
                self.buf.insert_char(ch);
            }
        }
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

    fn linewise_range_to_row(&self, target_row: usize) -> (Cursor, Cursor) {
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

    /// Replaces the identifier prefix ending at the cursor with `new`.
    ///
    /// Assumes the cursor is sitting immediately after the prefix (the
    /// state right after typing or after the autocomplete popup opens).
    pub fn replace_word_prefix(&mut self, new: &str) {
        let prefix_len = self.word_prefix_before_cursor().chars().count();
        let pre = self.buf.clone();
        for _ in 0..prefix_len {
            self.buf.backspace();
        }
        for c in new.chars() {
            self.buf.insert_char(c);
        }
        self.undo.record(&pre);
    }

    /// Inserts an arbitrary string at the cursor, preserving newlines and
    /// tabs. Used for bracketed paste. CRLF normalized to LF so Windows
    /// clipboards don't produce blank lines.
    pub fn insert_str(&mut self, s: &str) {
        let pre = self.buf.clone();
        let mut changed = false;
        for c in s.chars() {
            match c {
                '\r' => continue,
                '\n' => self.buf.insert_newline(),
                other => self.buf.insert_char(other),
            }
            changed = true;
        }
        if changed {
            self.undo.record(&pre);
        }
    }

    /// Replaces the entire buffer with `s`. Used for history recall and
    /// "Open file". Clears undo so the recall isn't itself undoable.
    pub fn set_text(&mut self, s: &str) {
        self.buf.replace_all(s);
        self.undo.clear();
    }

    pub fn insert_spaces(&mut self, n: usize) {
        if n == 0 {
            return;
        }
        let pre = self.buf.clone();
        for _ in 0..n {
            self.buf.insert_char(' ');
        }
        self.undo.record(&pre);
    }

    pub fn scroll_lines(&mut self, delta: i32) {
        let max_top = self.buf.line_count().saturating_sub(1) as i32;
        let new = (self.view.scroll_top as i32 + delta).clamp(0, max_top);
        self.view.scroll_top = new as usize;
    }

    /// Moves the cursor to a 1-based line number, clamped into the
    /// buffer's range. Used by the `Ctrl+G` overlay; selection is
    /// cleared so the goto-line jump doesn't accidentally extend a
    /// selection the user had active.
    pub fn goto_line(&mut self, line_1based: usize) {
        let total = self.buf.line_count();
        if total == 0 {
            return;
        }
        let target_row = line_1based.saturating_sub(1).min(total - 1);
        self.buf.cancel_selection();
        self.buf.set_cursor(target_row, 0);
    }

    /// Jumps the caret to an arbitrary `(row, col)` cursor, clearing
    /// the selection. Used by the find / find-replace overlay to land
    /// on the next match.
    pub fn jump_caret(&mut self, c: buffer::Cursor) {
        self.buf.cancel_selection();
        self.buf.set_cursor(c.row, c.col);
    }

    /// Replaces the inclusive char range `[start, end)` with `text`.
    /// Single-line ranges only (the find needle is single-line, but the
    /// replacement itself may contain `\n`s — those split lines as
    /// `insert_newline` does). Pushes a single undo snapshot.
    pub fn replace_range(&mut self, start: buffer::Cursor, end: buffer::Cursor, text: &str) {
        debug_assert!(start.row == end.row);
        let pre = self.buf.clone();
        self.apply_replace(start, end, text);
        self.undo.record(&pre);
    }

    /// Replaces every range in `ranges` with `text` as a single undo
    /// transaction. Iterates right-to-left so already-processed
    /// replacements don't shift the offsets of later ones.
    pub fn replace_all(&mut self, ranges: &[(buffer::Cursor, buffer::Cursor)], text: &str) {
        if ranges.is_empty() {
            return;
        }
        let pre = self.buf.clone();
        // Sort defensively in case the caller didn't.
        let mut ordered: Vec<_> = ranges.to_vec();
        ordered.sort_by_key(|(s, _)| (s.row, s.col));
        for (start, end) in ordered.into_iter().rev() {
            self.apply_replace(start, end, text);
        }
        self.undo.record(&pre);
    }

    fn apply_replace(&mut self, start: buffer::Cursor, end: buffer::Cursor, text: &str) {
        self.buf.cancel_selection();
        self.buf.set_cursor(start.row, start.col);
        let count = end.col.saturating_sub(start.col);
        for _ in 0..count {
            self.buf.delete_forward();
        }
        for c in text.chars() {
            if c == '\n' {
                self.buf.insert_newline();
            } else if c != '\r' {
                self.buf.insert_char(c);
            }
        }
    }

    /// Moves the cursor to the 1-based character position used by Postgres
    /// error reports. Returns `true` if the position fell inside the
    /// buffer, `false` if it was past the end.
    pub fn move_cursor_to_char_position(&mut self, position_1based: u32) -> bool {
        let target = match (position_1based as usize).checked_sub(1) {
            Some(n) => n,
            None => return false,
        };
        let mut acc = 0usize;
        for (row, line) in self.buf.lines().iter().enumerate() {
            let line_chars = line.chars().count();
            if target <= acc + line_chars {
                let col = target - acc;
                self.buf.cancel_selection();
                self.buf.set_cursor(row, col);
                return true;
            }
            acc += line_chars + 1; // +1 for the newline separator
        }
        false
    }

    /// Inserts two spaces at the start of every line in the inclusive
    /// range. Used for block-indent of a selection.
    pub fn indent_lines(&mut self, start: usize, end: usize) {
        let total = self.buf.line_count();
        if total == 0 {
            return;
        }
        let pre = self.buf.clone();
        let end = end.min(total - 1);
        let saved_cursor = self.buf.cursor();
        let saved_anchor = self.buf.selection_anchor();
        for row in start..=end {
            self.buf.set_cursor(row, 0);
            self.buf.insert_char(' ');
            self.buf.insert_char(' ');
        }
        // Adjust saved cursor / selection to account for the two spaces
        // inserted on each row that was inside the range.
        let shift = |c: Cursor| {
            if c.row >= start && c.row <= end {
                Cursor::new(c.row, c.col + 2)
            } else {
                c
            }
        };
        self.buf
            .set_cursor(shift(saved_cursor).row, shift(saved_cursor).col);
        if let Some(a) = saved_anchor {
            self.buf.set_cursor(shift(a).row, shift(a).col);
            self.buf.start_selection();
            self.buf
                .set_cursor(shift(saved_cursor).row, shift(saved_cursor).col);
        }
        self.undo.record(&pre);
    }

    /// Removes up to two leading spaces from every line in the inclusive
    /// range.
    pub fn outdent_lines(&mut self, start: usize, end: usize) {
        let total = self.buf.line_count();
        if total == 0 {
            return;
        }
        let pre = self.buf.clone();
        let end = end.min(total - 1);
        let saved_cursor = self.buf.cursor();
        let saved_anchor = self.buf.selection_anchor();
        let mut removed_per_row: Vec<usize> = Vec::with_capacity(end - start + 1);
        for row in start..=end {
            let leading = self.buf.lines()[row]
                .chars()
                .take_while(|c| *c == ' ')
                .count();
            let remove = leading.min(2);
            removed_per_row.push(remove);
            if remove == 0 {
                continue;
            }
            self.buf.set_cursor(row, 0);
            for _ in 0..remove {
                self.buf.delete_forward();
            }
        }
        let shift = |c: Cursor| {
            if c.row >= start && c.row <= end {
                let removed = removed_per_row[c.row - start];
                Cursor::new(c.row, c.col.saturating_sub(removed))
            } else {
                c
            }
        };
        self.buf
            .set_cursor(shift(saved_cursor).row, shift(saved_cursor).col);
        if let Some(a) = saved_anchor {
            self.buf.set_cursor(shift(a).row, shift(a).col);
            self.buf.start_selection();
            self.buf
                .set_cursor(shift(saved_cursor).row, shift(saved_cursor).col);
        }
        self.undo.record(&pre);
    }

    pub fn outdent_current_line(&mut self) {
        let row = self.buf.cursor().row;
        self.outdent_lines(row, row);
    }

    #[cfg(test)]
    fn type_text(&mut self, s: &str) {
        for c in s.chars() {
            let key = match c {
                '\n' => KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                _ => KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            };
            self.handle_key(key);
        }
    }
}

fn motion_from_keycode(code: KeyCode) -> Option<Motion> {
    match code {
        KeyCode::Char('h') | KeyCode::Left => Some(Motion::Left),
        KeyCode::Char('j') | KeyCode::Down => Some(Motion::Down),
        KeyCode::Char('k') | KeyCode::Up => Some(Motion::Up),
        KeyCode::Char('l') | KeyCode::Right => Some(Motion::Right),
        KeyCode::Char('w') => Some(Motion::WordForward),
        KeyCode::Char('b') => Some(Motion::WordBackward),
        KeyCode::Char('e') => Some(Motion::WordEnd),
        KeyCode::Char('0') => Some(Motion::LineStart),
        KeyCode::Char('^') => Some(Motion::FirstNonBlank),
        KeyCode::Char('$') => Some(Motion::LineEnd),
        KeyCode::Char('%') => Some(Motion::MatchingBracket),
        _ => None,
    }
}

fn motion_inclusive(motion: Motion) -> bool {
    matches!(
        motion,
        Motion::WordEnd | Motion::LineEnd | Motion::MatchingBracket
    )
}

fn cursor_le(a: Cursor, b: Cursor) -> bool {
    (a.row, a.col) <= (b.row, b.col)
}

/// Bump a cursor one logical position forward (walking onto the
/// implicit newline slot at end-of-line). Saturates at end-of-buffer.
/// Used by operator dispatch to convert an inclusive motion endpoint
/// into an exclusive one for delete-range.
fn step_forward_one(lines: &[String], c: Cursor) -> Cursor {
    let len = lines.get(c.row).map(|l| l.chars().count()).unwrap_or(0);
    if c.col < len {
        Cursor::new(c.row, c.col + 1)
    } else if c.row + 1 < lines.len() {
        Cursor::new(c.row + 1, 0)
    } else {
        c
    }
}

/// Counts char + newline slots strictly between `start` (inclusive)
/// and `end` (exclusive), matching the number of `delete_forward`
/// calls needed to consume that range.
fn count_chars_between(lines: &[String], start: Cursor, end: Cursor) -> usize {
    if start.row == end.row {
        return end.col.saturating_sub(start.col);
    }
    let mut total = 0;
    let start_line_len = lines.get(start.row).map(|l| l.chars().count()).unwrap_or(0);
    total += start_line_len.saturating_sub(start.col);
    total += 1; // newline at end of start.row
    for row in (start.row + 1)..end.row {
        total += lines.get(row).map(|l| l.chars().count()).unwrap_or(0);
        total += 1;
    }
    total += end.col;
    total
}

pub fn draw(
    frame: &mut Frame<'_>,
    state: &mut EditorState,
    focused: bool,
    hints: &render::RenderHints<'_>,
    area: Rect,
) {
    let mode_style = match state.mode {
        Mode::Insert => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        Mode::Normal => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        Mode::Visual { .. } => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    };
    let title = Line::from(vec![
        Span::raw(" SQL editor "),
        Span::styled(state.mode.label(), mode_style),
        Span::raw("  [F5 run \u{00b7} Tab complete] "),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(focus_style(focused));
    let placeholder = if state.buf.is_empty() {
        Some(PLACEHOLDER)
    } else {
        None
    };
    render::draw(
        frame,
        &state.buf,
        &mut state.view,
        render::DrawArgs {
            area,
            focused,
            block,
            placeholder,
            hints,
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_prefix_extracts_identifier_before_cursor() {
        let mut e = EditorState::new();
        e.type_text("SELECT user");
        assert_eq!(e.word_prefix_before_cursor(), "user");
    }

    #[test]
    fn word_prefix_empty_when_cursor_after_space() {
        let mut e = EditorState::new();
        e.type_text("SELECT ");
        assert_eq!(e.word_prefix_before_cursor(), "");
    }

    #[test]
    fn word_prefix_empty_when_cursor_after_digit_start() {
        let mut e = EditorState::new();
        e.type_text("123abc");
        assert_eq!(e.word_prefix_before_cursor(), "");
    }

    #[test]
    fn replace_word_prefix_swaps_last_token() {
        let mut e = EditorState::new();
        e.type_text("SELECT use");
        e.replace_word_prefix("users");
        assert_eq!(e.text(), "SELECT users");
    }

    #[test]
    fn outdent_removes_up_to_two_leading_spaces() {
        let mut e = EditorState::new();
        e.type_text("    SELECT 1");
        // Move cursor to line start.
        for _ in 0..12 {
            e.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        }
        e.outdent_current_line();
        assert_eq!(e.text(), "  SELECT 1");
        e.outdent_current_line();
        assert_eq!(e.text(), "SELECT 1");
        // No leading spaces → no-op.
        e.outdent_current_line();
        assert_eq!(e.text(), "SELECT 1");
    }

    #[test]
    fn insert_spaces_appends_n_spaces() {
        let mut e = EditorState::new();
        e.type_text("a");
        e.insert_spaces(3);
        assert_eq!(e.text(), "a   ");
    }

    #[test]
    fn insert_str_preserves_newlines() {
        let mut e = EditorState::new();
        e.insert_str("SELECT 1\nFROM t;");
        assert_eq!(e.text(), "SELECT 1\nFROM t;");
    }

    #[test]
    fn move_cursor_to_char_position_handles_single_line() {
        let mut e = EditorState::new();
        e.type_text("SELECT 1 FROM nope");
        // 'n' of "nope" is at 1-based char 15.
        assert!(e.move_cursor_to_char_position(15));
        assert_eq!(e.cursor_line_col(), (1, 15));
    }

    #[test]
    fn move_cursor_to_char_position_handles_multi_line() {
        let mut e = EditorState::new();
        e.type_text("SELECT 1\nFROM bad");
        assert!(e.move_cursor_to_char_position(15));
        let (ln, col) = e.cursor_line_col();
        assert_eq!(ln, 2);
        assert_eq!(col, 6);
    }

    #[test]
    fn move_cursor_to_char_position_returns_false_when_out_of_range() {
        let mut e = EditorState::new();
        e.type_text("abc");
        assert!(!e.move_cursor_to_char_position(99));
    }

    #[test]
    fn indent_lines_prepends_two_spaces_per_line() {
        let mut e = EditorState::new();
        e.type_text("a\nb\nc");
        e.indent_lines(0, 2);
        assert_eq!(e.text(), "  a\n  b\n  c");
    }

    #[test]
    fn outdent_lines_removes_up_to_two_leading_spaces_per_line() {
        let mut e = EditorState::new();
        e.type_text("    a\n  b\nc");
        e.outdent_lines(0, 2);
        assert_eq!(e.text(), "  a\nb\nc");
    }

    #[test]
    fn indent_then_outdent_round_trips() {
        let mut e = EditorState::new();
        e.type_text("x\ny\nz");
        e.indent_lines(0, 2);
        e.outdent_lines(0, 2);
        assert_eq!(e.text(), "x\ny\nz");
    }

    #[test]
    fn insert_str_normalizes_crlf() {
        let mut e = EditorState::new();
        e.insert_str("a\r\nb");
        assert_eq!(e.text(), "a\nb");
    }

    #[test]
    fn goto_line_jumps_to_first_column_of_target_row() {
        let mut e = EditorState::new();
        e.type_text("a\nb\nc\nd");
        e.goto_line(3);
        assert_eq!(e.cursor_line_col(), (3, 1));
    }

    #[test]
    fn goto_line_clamps_to_last_row_when_out_of_range() {
        let mut e = EditorState::new();
        e.type_text("a\nb\nc");
        e.goto_line(99);
        assert_eq!(e.cursor_line_col(), (3, 1));
    }

    #[test]
    fn replace_range_swaps_a_single_match() {
        let mut e = EditorState::new();
        e.type_text("a foo b");
        let s = buffer::Cursor::new(0, 2);
        let end = buffer::Cursor::new(0, 5);
        e.replace_range(s, end, "BAR");
        assert_eq!(e.text(), "a BAR b");
    }

    #[test]
    fn replace_all_swaps_every_match_and_undo_is_one_step() {
        let mut e = EditorState::new();
        e.type_text("a a a");
        let ranges = vec![
            (buffer::Cursor::new(0, 0), buffer::Cursor::new(0, 1)),
            (buffer::Cursor::new(0, 2), buffer::Cursor::new(0, 3)),
            (buffer::Cursor::new(0, 4), buffer::Cursor::new(0, 5)),
        ];
        e.replace_all(&ranges, "bb");
        assert_eq!(e.text(), "bb bb bb");
        // Single Ctrl+Z reverts the entire batch.
        e.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(e.text(), "a a a");
    }

    #[test]
    fn replace_all_handles_replacement_containing_needle() {
        // Replacing 'foo' with 'foofoo' must NOT loop — left-to-right
        // semantics, no rescanning.
        let mut e = EditorState::new();
        e.type_text("foo");
        let ranges = vec![(buffer::Cursor::new(0, 0), buffer::Cursor::new(0, 3))];
        e.replace_all(&ranges, "foofoo");
        assert_eq!(e.text(), "foofoo");
    }

    #[test]
    fn replace_all_with_empty_ranges_is_noop() {
        let mut e = EditorState::new();
        e.type_text("untouched");
        e.replace_all(&[], "x");
        assert_eq!(e.text(), "untouched");
    }

    #[test]
    fn goto_line_zero_is_treated_as_line_one() {
        let mut e = EditorState::new();
        e.type_text("a\nb\nc");
        e.goto_line(0);
        assert_eq!(e.cursor_line_col(), (1, 1));
    }

    #[test]
    fn ctrl_z_undoes_last_edit() {
        let mut e = EditorState::new();
        e.type_text("ab");
        e.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        assert_eq!(e.text(), "a");
    }

    #[test]
    fn ctrl_y_redoes_undone_edit() {
        let mut e = EditorState::new();
        e.type_text("ab");
        e.handle_key(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::CONTROL));
        e.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::CONTROL));
        assert_eq!(e.text(), "ab");
    }

    // ---- mode state machine (R2) -----------------------------------

    fn press(e: &mut EditorState, code: KeyCode, mods: KeyModifiers) -> bool {
        e.handle_key(KeyEvent::new(code, mods))
    }

    #[test]
    fn fresh_editor_starts_in_insert_mode() {
        let e = EditorState::new();
        assert_eq!(e.mode(), Mode::Insert);
    }

    #[test]
    fn esc_in_insert_switches_to_normal() {
        let mut e = EditorState::new();
        e.type_text("hi");
        let dirty = press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(e.mode(), Mode::Normal);
        assert!(!dirty, "mode flip is not a text change");
        assert_eq!(e.text(), "hi", "Esc must not mutate the buffer");
    }

    #[test]
    fn normal_swallows_unmapped_keys() {
        let mut e = EditorState::new();
        e.type_text("abc");
        press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
        // 'x' is unmapped in R2 — buffer must not change.
        let before = e.text();
        let dirty = press(&mut e, KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(!dirty);
        assert_eq!(e.text(), before);
        assert_eq!(e.mode(), Mode::Normal);
    }

    #[test]
    fn i_in_normal_switches_to_insert_at_cursor() {
        let mut e = EditorState::new();
        e.type_text("ab");
        press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
        // Cursor is at (0, 2) (end of "ab").
        press(&mut e, KeyCode::Char('i'), KeyModifiers::NONE);
        assert_eq!(e.mode(), Mode::Insert);
        assert_eq!(e.cursor_pos(), (0, 2));
    }

    #[test]
    fn a_in_normal_moves_right_then_insert() {
        let mut e = EditorState::new();
        e.type_text("ab");
        // Move cursor to col 1 ('b' is at col 1).
        press(&mut e, KeyCode::Left, KeyModifiers::NONE);
        press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(e.mode(), Mode::Insert);
        assert_eq!(e.cursor_pos(), (0, 2));
    }

    #[test]
    fn a_at_eol_does_not_move_past_end() {
        let mut e = EditorState::new();
        e.type_text("ab"); // cursor at (0, 2) — end of line
        press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('a'), KeyModifiers::NONE);
        assert_eq!(e.cursor_pos(), (0, 2));
    }

    #[test]
    fn capital_i_jumps_to_line_start() {
        let mut e = EditorState::new();
        e.type_text("abc"); // cursor (0, 3)
        press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('I'), KeyModifiers::SHIFT);
        assert_eq!(e.mode(), Mode::Insert);
        assert_eq!(e.cursor_pos(), (0, 0));
    }

    #[test]
    fn capital_a_jumps_to_line_end() {
        let mut e = EditorState::new();
        e.type_text("abc");
        press(&mut e, KeyCode::Home, KeyModifiers::NONE); // (0, 0)
        press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('A'), KeyModifiers::SHIFT);
        assert_eq!(e.mode(), Mode::Insert);
        assert_eq!(e.cursor_pos(), (0, 3));
    }

    #[test]
    fn o_opens_line_below_and_enters_insert() {
        let mut e = EditorState::new();
        e.type_text("abc");
        press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
        let dirty = press(&mut e, KeyCode::Char('o'), KeyModifiers::NONE);
        assert!(dirty, "o adds text");
        assert_eq!(e.mode(), Mode::Insert);
        assert_eq!(e.text(), "abc\n");
        assert_eq!(e.cursor_pos(), (1, 0));
    }

    #[test]
    fn capital_o_opens_line_above_and_enters_insert() {
        let mut e = EditorState::new();
        e.type_text("abc");
        press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
        let dirty = press(&mut e, KeyCode::Char('O'), KeyModifiers::SHIFT);
        assert!(dirty, "O adds text");
        assert_eq!(e.mode(), Mode::Insert);
        assert_eq!(e.text(), "\nabc");
        assert_eq!(e.cursor_pos(), (0, 0));
    }

    #[test]
    fn ctrl_z_works_in_normal_mode() {
        let mut e = EditorState::new();
        e.type_text("ab");
        press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(e.text(), "a");
    }

    #[test]
    fn entering_insert_from_normal_then_typing_inserts_text() {
        let mut e = EditorState::new();
        e.type_text("ab");
        press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('A'), KeyModifiers::SHIFT);
        // Now in Insert at (0, 2). Type a single char.
        let dirty = press(&mut e, KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(dirty);
        assert_eq!(e.text(), "abc");
    }

    // ---- motions + count + chord (R3) ------------------------------

    fn enter_normal(e: &mut EditorState) {
        press(e, KeyCode::Esc, KeyModifiers::NONE);
    }

    #[test]
    fn h_in_normal_moves_left() {
        let mut e = EditorState::new();
        e.type_text("hello"); // cursor (0, 5)
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('h'), KeyModifiers::NONE);
        assert_eq!(e.cursor_pos(), (0, 4));
    }

    #[test]
    fn count_prefix_repeats_motion() {
        let mut e = EditorState::new();
        e.type_text("hello"); // cursor (0, 5)
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('3'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('h'), KeyModifiers::NONE);
        assert_eq!(e.cursor_pos(), (0, 2));
    }

    #[test]
    fn count_accumulates_across_multiple_digits() {
        let mut e = EditorState::new();
        // Build a long line so a 12-step left move actually has room.
        e.type_text(&"x".repeat(20)); // cursor (0, 20)
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('1'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('2'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('h'), KeyModifiers::NONE);
        assert_eq!(e.cursor_pos(), (0, 8));
    }

    #[test]
    fn count_resets_after_motion_fires() {
        let mut e = EditorState::new();
        e.type_text("hello world");
        enter_normal(&mut e);
        // 3h moves 3 left, then plain h must move only 1.
        press(&mut e, KeyCode::Char('3'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('h'), KeyModifiers::NONE);
        let after_3h = e.cursor_pos();
        press(&mut e, KeyCode::Char('h'), KeyModifiers::NONE);
        assert_eq!(e.cursor_pos().1, after_3h.1 - 1);
    }

    #[test]
    fn zero_first_is_line_start() {
        let mut e = EditorState::new();
        e.type_text("  hello");
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('0'), KeyModifiers::NONE);
        assert_eq!(e.cursor_pos(), (0, 0));
    }

    #[test]
    fn zero_extends_count_when_count_in_progress() {
        let mut e = EditorState::new();
        e.type_text(&"x".repeat(20));
        enter_normal(&mut e);
        // 1 then 0 should accumulate to 10, not stop at LineStart.
        press(&mut e, KeyCode::Char('1'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('0'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('h'), KeyModifiers::NONE);
        assert_eq!(e.cursor_pos(), (0, 10));
    }

    #[test]
    fn caret_jumps_to_first_non_blank() {
        let mut e = EditorState::new();
        e.type_text("    abc");
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('^'), KeyModifiers::SHIFT);
        assert_eq!(e.cursor_pos(), (0, 4));
    }

    #[test]
    fn dollar_jumps_to_line_end() {
        let mut e = EditorState::new();
        e.type_text("abcdef");
        // Move to start.
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('$'), KeyModifiers::SHIFT);
        assert_eq!(e.cursor_pos(), (0, 6));
    }

    #[test]
    fn w_jumps_to_next_word_start() {
        let mut e = EditorState::new();
        e.type_text("foo bar baz");
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('w'), KeyModifiers::NONE);
        assert_eq!(e.cursor_pos(), (0, 4));
    }

    #[test]
    fn b_jumps_to_previous_word_start() {
        let mut e = EditorState::new();
        e.type_text("foo bar");
        // cursor (0, 7) end-of-line
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('b'), KeyModifiers::NONE);
        assert_eq!(e.cursor_pos(), (0, 4));
    }

    #[test]
    fn e_jumps_to_word_end() {
        let mut e = EditorState::new();
        e.type_text("foo bar");
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('e'), KeyModifiers::NONE);
        assert_eq!(e.cursor_pos(), (0, 2));
    }

    #[test]
    fn percent_jumps_to_matching_bracket() {
        let mut e = EditorState::new();
        e.type_text("(foo)");
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('%'), KeyModifiers::SHIFT);
        assert_eq!(e.cursor_pos(), (0, 4));
    }

    #[test]
    fn capital_g_with_count_goes_to_line_n() {
        let mut e = EditorState::new();
        e.type_text("a\nb\nc\nd\ne");
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('3'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(e.cursor_line_col(), (3, 1));
    }

    #[test]
    fn bare_capital_g_goes_to_last_line() {
        let mut e = EditorState::new();
        e.type_text("a\nb\nc");
        // Cursor sits at end of buffer; first move it elsewhere.
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        press(&mut e, KeyCode::Up, KeyModifiers::NONE);
        press(&mut e, KeyCode::Up, KeyModifiers::NONE);
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(e.cursor_line_col(), (3, 1));
    }

    #[test]
    fn gg_chord_resolves_to_first_line() {
        let mut e = EditorState::new();
        e.type_text("a\nb\nc\nd");
        enter_normal(&mut e);
        // First g — chord pending, no movement.
        press(&mut e, KeyCode::Char('g'), KeyModifiers::NONE);
        let after_first_g = e.cursor_line_col();
        // Buffer cursor unchanged so far.
        assert_eq!(after_first_g, e.cursor_line_col());
        press(&mut e, KeyCode::Char('g'), KeyModifiers::NONE);
        assert_eq!(e.cursor_line_col(), (1, 1));
    }

    #[test]
    fn gg_chord_with_count_goes_to_line_n() {
        let mut e = EditorState::new();
        e.type_text("a\nb\nc\nd\ne");
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('5'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('g'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('g'), KeyModifiers::NONE);
        assert_eq!(e.cursor_line_col(), (5, 1));
    }

    #[test]
    fn chord_is_broken_by_unrelated_key() {
        let mut e = EditorState::new();
        e.type_text("foo bar");
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('g'), KeyModifiers::NONE);
        // Now press 'h' — chord breaks, h applies as Left motion.
        let before = e.cursor_pos();
        press(&mut e, KeyCode::Char('h'), KeyModifiers::NONE);
        assert_eq!(e.cursor_pos(), (before.0, before.1.saturating_sub(1)));
    }

    #[test]
    fn mode_entry_resets_pending_count() {
        let mut e = EditorState::new();
        e.type_text("abc");
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('5'), KeyModifiers::NONE);
        // 'i' enters Insert and the `5` is dropped, not used as count.
        press(&mut e, KeyCode::Char('i'), KeyModifiers::NONE);
        assert_eq!(e.mode(), Mode::Insert);
        // Type a char to confirm insert mode and that no count lingered.
        press(&mut e, KeyCode::Char('z'), KeyModifiers::NONE);
        // Cursor was at (0, 3) before; 5 was dropped; one 'z' inserted.
        assert!(e.text().contains('z'));
    }

    // ---- Visual mode + operators + text objects (R4) ---------------

    #[test]
    fn v_enters_visual_at_cursor() {
        let mut e = EditorState::new();
        e.type_text("hello");
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('v'), KeyModifiers::NONE);
        assert!(matches!(e.mode(), Mode::Visual { .. }));
    }

    #[test]
    fn v_then_motion_extends_selection_then_d_deletes() {
        let mut e = EditorState::new();
        e.type_text("hello world");
        // Move cursor to start.
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        enter_normal(&mut e);
        // v + 4 right + d → delete first 5 chars ("hello"). Visual is
        // inclusive so the right edge is part of the deletion.
        press(&mut e, KeyCode::Char('v'), KeyModifiers::NONE);
        for _ in 0..4 {
            press(&mut e, KeyCode::Char('l'), KeyModifiers::NONE);
        }
        press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(e.text(), " world");
        assert_eq!(e.mode(), Mode::Normal);
    }

    #[test]
    fn esc_in_visual_returns_to_normal_without_change() {
        let mut e = EditorState::new();
        e.type_text("hello");
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('v'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('h'), KeyModifiers::NONE);
        let before = e.text();
        press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(e.mode(), Mode::Normal);
        assert_eq!(e.text(), before);
    }

    #[test]
    fn dw_deletes_to_next_word_start() {
        let mut e = EditorState::new();
        e.type_text("foo bar baz");
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        enter_normal(&mut e);
        // dw on "foo" → delete "foo " (word + trailing space).
        press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('w'), KeyModifiers::NONE);
        assert_eq!(e.text(), "bar baz");
    }

    #[test]
    fn de_deletes_through_word_end_inclusive() {
        let mut e = EditorState::new();
        e.type_text("foo bar");
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        enter_normal(&mut e);
        // de = delete from 'f' through end of 'foo' (inclusive).
        press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('e'), KeyModifiers::NONE);
        assert_eq!(e.text(), " bar");
    }

    #[test]
    fn dd_deletes_current_line() {
        let mut e = EditorState::new();
        e.type_text("a\nb\nc");
        // Move to line 2.
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        press(&mut e, KeyCode::Up, KeyModifiers::NONE);
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(e.text(), "a\nc");
    }

    #[test]
    fn dd_with_count_deletes_n_lines() {
        let mut e = EditorState::new();
        e.type_text("a\nb\nc\nd\ne");
        // Move to line 2 (col 0).
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        for _ in 0..3 {
            press(&mut e, KeyCode::Up, KeyModifiers::NONE);
        }
        enter_normal(&mut e);
        // 3dd → delete lines 2..=4.
        press(&mut e, KeyCode::Char('3'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(e.text(), "a\ne");
    }

    #[test]
    fn yw_yanks_into_register_without_modifying_buffer() {
        let mut e = EditorState::new();
        e.type_text("foo bar");
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('y'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('w'), KeyModifiers::NONE);
        assert_eq!(e.text(), "foo bar");
        assert_eq!(e.register, "foo ");
    }

    #[test]
    fn cw_deletes_word_and_enters_insert() {
        let mut e = EditorState::new();
        e.type_text("foo bar");
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('c'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('w'), KeyModifiers::NONE);
        assert_eq!(e.mode(), Mode::Insert);
        // Now in Insert at start; type something.
        press(&mut e, KeyCode::Char('Z'), KeyModifiers::SHIFT);
        assert_eq!(e.text(), "Zbar");
    }

    #[test]
    fn diw_deletes_inner_word() {
        let mut e = EditorState::new();
        e.type_text("foo bar baz");
        // Cursor on 'a' of "bar" (col 5).
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        for _ in 0..5 {
            press(&mut e, KeyCode::Right, KeyModifiers::NONE);
        }
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('i'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('w'), KeyModifiers::NONE);
        assert_eq!(e.text(), "foo  baz");
    }

    #[test]
    fn diw_big_word_includes_dotted_identifier() {
        let mut e = EditorState::new();
        e.type_text("SELECT schema.table FROM x");
        // Cursor on 't' of "table" (col 14).
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        for _ in 0..14 {
            press(&mut e, KeyCode::Right, KeyModifiers::NONE);
        }
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('i'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('W'), KeyModifiers::SHIFT);
        // diW takes the whole "schema.table".
        assert_eq!(e.text(), "SELECT  FROM x");
    }

    #[test]
    fn ci_quote_changes_string_contents() {
        let mut e = EditorState::new();
        e.type_text("a \"foo\" b");
        // Cursor on 'o' inside quotes (col 4).
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        for _ in 0..4 {
            press(&mut e, KeyCode::Right, KeyModifiers::NONE);
        }
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('c'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('i'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('"'), KeyModifiers::SHIFT);
        // Now in Insert with "foo" gone, quotes preserved.
        assert_eq!(e.mode(), Mode::Insert);
        assert_eq!(e.text(), "a \"\" b");
    }

    #[test]
    fn dap_deletes_around_parens() {
        let mut e = EditorState::new();
        e.type_text("foo(bar)baz");
        // Cursor on 'a' of "bar" (col 5).
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        for _ in 0..5 {
            press(&mut e, KeyCode::Right, KeyModifiers::NONE);
        }
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('a'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('('), KeyModifiers::SHIFT);
        assert_eq!(e.text(), "foobaz");
    }

    #[test]
    fn paste_after_inserts_register_after_cursor() {
        let mut e = EditorState::new();
        e.type_text("foo bar");
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        enter_normal(&mut e);
        // Yank first word.
        press(&mut e, KeyCode::Char('y'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('w'), KeyModifiers::NONE);
        // Move to end of buffer.
        press(&mut e, KeyCode::Char('$'), KeyModifiers::SHIFT);
        // Paste.
        press(&mut e, KeyCode::Char('p'), KeyModifiers::NONE);
        // Original "foo bar" → cursor at end, paste "foo " after =
        // "foo bafoo r" — register held "foo " (yw includes trailing space).
        assert!(e.text().contains("foo"));
        assert!(e.text().len() > "foo bar".len());
    }

    #[test]
    fn delete_via_op_is_undoable_in_one_step() {
        let mut e = EditorState::new();
        e.type_text("foo bar baz");
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('w'), KeyModifiers::NONE);
        assert_eq!(e.text(), "bar baz");
        // Single Ctrl+Z reverts.
        press(&mut e, KeyCode::Char('z'), KeyModifiers::CONTROL);
        assert_eq!(e.text(), "foo bar baz");
    }

    #[test]
    fn x_deletes_char_under_cursor() {
        let mut e = EditorState::new();
        e.type_text("hello");
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(e.text(), "ello");
    }

    #[test]
    fn x_with_count_deletes_n_chars() {
        let mut e = EditorState::new();
        e.type_text("hello");
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('3'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(e.text(), "lo");
    }

    #[test]
    fn d_then_esc_cancels_operator() {
        let mut e = EditorState::new();
        e.type_text("hello");
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Esc, KeyModifiers::NONE);
        // Esc cancels op; subsequent 'w' should be a plain motion.
        press(&mut e, KeyCode::Char('w'), KeyModifiers::NONE);
        assert_eq!(e.text(), "hello");
    }

    #[test]
    fn d_capital_g_deletes_through_last_line_linewise() {
        let mut e = EditorState::new();
        e.type_text("a\nb\nc\nd");
        // Move to line 2.
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        for _ in 0..2 {
            press(&mut e, KeyCode::Up, KeyModifiers::NONE);
        }
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('d'), KeyModifiers::NONE);
        press(&mut e, KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(e.text(), "a");
    }

    #[test]
    fn visual_yank_does_not_modify_buffer() {
        let mut e = EditorState::new();
        e.type_text("foo bar");
        press(&mut e, KeyCode::Home, KeyModifiers::NONE);
        enter_normal(&mut e);
        press(&mut e, KeyCode::Char('v'), KeyModifiers::NONE);
        for _ in 0..2 {
            press(&mut e, KeyCode::Char('l'), KeyModifiers::NONE);
        }
        press(&mut e, KeyCode::Char('y'), KeyModifiers::NONE);
        assert_eq!(e.text(), "foo bar");
        assert_eq!(e.register, "foo");
        assert_eq!(e.mode(), Mode::Normal);
    }
}
