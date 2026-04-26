//! Public edit operations on `EditorState` — replace-prefix, insert,
//! file-load (`set_text`), block indent / outdent, find/replace
//! callbacks, scroll, goto-line, error-position jump.

use super::buffer::Cursor;
use super::util::insert_text;
use super::EditorState;

impl EditorState {
    /// Replaces the identifier prefix ending at the cursor with `new`.
    /// Assumes the cursor sits immediately after the prefix (the state
    /// right after typing or after the autocomplete popup opens).
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

    /// Inserts an arbitrary string at the cursor, preserving newlines
    /// and tabs. Used for bracketed paste. CRLF normalized to LF so
    /// Windows clipboards don't produce blank lines.
    pub fn insert_str(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        let pre = self.buf.clone();
        if insert_text(&mut self.buf, s) {
            self.undo.record(&pre);
        }
    }

    /// Replaces the entire buffer with `s`. Used for history recall
    /// and "Open file". Clears undo so the recall isn't itself
    /// undoable.
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
    /// buffer's range. Selection is cleared so the goto-line jump
    /// doesn't accidentally extend a selection the user had active.
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
    pub fn jump_caret(&mut self, c: Cursor) {
        self.buf.cancel_selection();
        self.buf.set_cursor(c.row, c.col);
    }

    /// Replaces the inclusive char range `[start, end)` with `text`.
    /// Single-line ranges only (the find needle is single-line, but
    /// the replacement itself may contain `\n`s — those split lines
    /// as `insert_newline` does). Pushes a single undo snapshot.
    pub fn replace_range(&mut self, start: Cursor, end: Cursor, text: &str) {
        debug_assert!(start.row == end.row);
        let pre = self.buf.clone();
        self.apply_replace(start, end, text);
        self.undo.record(&pre);
    }

    /// Replaces every range in `ranges` with `text` as a single undo
    /// transaction. Iterates right-to-left so already-processed
    /// replacements don't shift the offsets of later ones.
    pub fn replace_all(&mut self, ranges: &[(Cursor, Cursor)], text: &str) {
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

    fn apply_replace(&mut self, start: Cursor, end: Cursor, text: &str) {
        self.buf.cancel_selection();
        self.buf.set_cursor(start.row, start.col);
        let count = end.col.saturating_sub(start.col);
        for _ in 0..count {
            self.buf.delete_forward();
        }
        insert_text(&mut self.buf, text);
    }

    /// Moves the cursor to the 1-based character position used by
    /// Postgres error reports. Returns `true` if the position fell
    /// inside the buffer, `false` if it was past the end.
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
        // Adjust saved cursor / selection to account for the two
        // spaces inserted on each row that was inside the range.
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

    /// Removes up to two leading spaces from every line in the
    /// inclusive range.
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
}
