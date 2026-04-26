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
pub mod render;
pub mod tab;
pub mod undo;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::widgets::{Block, Borders};
use ratatui::Frame;

use super::focus_style;
use buffer::{Cursor, TextBuffer};
use edit::EditOutcome;
use render::ViewState;
use undo::UndoStack;

const PLACEHOLDER: &str = "-- F5 / Ctrl+Enter to run, Tab = autocomplete";

pub struct EditorState {
    buf: TextBuffer,
    undo: UndoStack,
    view: ViewState,
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
        }
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
        let pre = self.buf.clone();
        let outcome = edit::handle_key(&mut self.buf, key);
        if outcome == EditOutcome::Changed {
            self.undo.record(&pre);
            true
        } else {
            false
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

pub fn draw(
    frame: &mut Frame<'_>,
    state: &mut EditorState,
    focused: bool,
    hints: &render::RenderHints<'_>,
    area: Rect,
) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" SQL editor  [F5 run \u{00b7} Tab complete] ")
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
}
