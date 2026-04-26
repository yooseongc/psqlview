//! In-memory editing buffer for the SQL editor.
//!
//! `TextBuffer` owns a `Vec<String>` of lines plus a `Cursor` and an
//! optional `Selection` anchor. It exposes only structural mutations
//! (insert a char, split a line, delete a range); higher-level edit
//! semantics live in `super::edit`.
//!
//! Line storage uses `String` (UTF-8) and column offsets are measured in
//! Unicode scalar values (chars), not bytes, so cursor movement stays
//! intuitive for non-ASCII identifiers.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Cursor {
    pub row: usize,
    pub col: usize,
}

impl Cursor {
    pub fn new(row: usize, col: usize) -> Self {
        Self { row, col }
    }
}

#[derive(Debug, Clone)]
pub struct TextBuffer {
    lines: Vec<String>,
    cursor: Cursor,
    /// Anchor of an active selection. The selected range is the closed
    /// region between `selection_anchor` and `cursor`, ordered.
    selection_anchor: Option<Cursor>,
}

impl Default for TextBuffer {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            cursor: Cursor::default(),
            selection_anchor: None,
        }
    }
}

impl TextBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_text(s: &str) -> Self {
        let mut buf = Self::default();
        buf.replace_all(s);
        buf
    }

    // -- inspectors --

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    pub fn cursor(&self) -> Cursor {
        self.cursor
    }

    pub fn line_chars(&self, row: usize) -> usize {
        self.lines.get(row).map(|l| l.chars().count()).unwrap_or(0)
    }

    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.lines.len() == 1 && self.lines[0].is_empty()
    }

    pub fn selection_anchor(&self) -> Option<Cursor> {
        self.selection_anchor
    }

    /// Returns the (start, end) of the active selection, ordered so the
    /// first element is lexicographically smaller. `None` if no selection.
    pub fn selection_range(&self) -> Option<(Cursor, Cursor)> {
        let a = self.selection_anchor?;
        let b = self.cursor;
        Some(if cursor_le(a, b) { (a, b) } else { (b, a) })
    }

    pub fn is_selecting(&self) -> bool {
        self.selection_anchor.is_some()
    }

    // -- selection state --

    pub fn start_selection(&mut self) {
        self.selection_anchor = Some(self.cursor);
    }

    pub fn cancel_selection(&mut self) {
        self.selection_anchor = None;
    }

    // -- raw cursor placement (clamped) --

    pub fn set_cursor(&mut self, row: usize, col: usize) {
        let row = row.min(self.lines.len().saturating_sub(1));
        let col = col.min(self.line_chars(row));
        self.cursor = Cursor { row, col };
    }

    // -- mutations --

    /// Replaces all content, resets cursor to (0,0), clears selection.
    pub fn replace_all(&mut self, s: &str) {
        self.lines = if s.is_empty() {
            vec![String::new()]
        } else {
            s.split('\n').map(str::to_string).collect()
        };
        self.cursor = Cursor::default();
        self.selection_anchor = None;
    }

    /// Inserts a single character at the cursor and advances it. Tabs
    /// are inserted literally; newlines are not — use `insert_newline`.
    pub fn insert_char(&mut self, c: char) {
        debug_assert!(c != '\n');
        let line = &mut self.lines[self.cursor.row];
        let byte_idx = char_to_byte_idx(line, self.cursor.col);
        line.insert(byte_idx, c);
        self.cursor.col += 1;
    }

    /// Splits the current line at the cursor, dropping a newline.
    pub fn insert_newline(&mut self) {
        let line = &mut self.lines[self.cursor.row];
        let byte_idx = char_to_byte_idx(line, self.cursor.col);
        let tail = line[byte_idx..].to_string();
        line.truncate(byte_idx);
        self.lines.insert(self.cursor.row + 1, tail);
        self.cursor.row += 1;
        self.cursor.col = 0;
    }

    /// Deletes the character before the cursor (Backspace). If at
    /// column 0, joins with the previous line.
    pub fn backspace(&mut self) {
        if self.cursor.col > 0 {
            let line = &mut self.lines[self.cursor.row];
            let prev = self.cursor.col - 1;
            let byte_lo = char_to_byte_idx(line, prev);
            let byte_hi = char_to_byte_idx(line, self.cursor.col);
            line.replace_range(byte_lo..byte_hi, "");
            self.cursor.col = prev;
        } else if self.cursor.row > 0 {
            let removed = self.lines.remove(self.cursor.row);
            self.cursor.row -= 1;
            self.cursor.col = self.line_chars(self.cursor.row);
            self.lines[self.cursor.row].push_str(&removed);
        }
    }

    /// Deletes the character at the cursor (Delete). If at end of
    /// line, joins with the next line.
    pub fn delete_forward(&mut self) {
        let line_len = self.line_chars(self.cursor.row);
        if self.cursor.col < line_len {
            let line = &mut self.lines[self.cursor.row];
            let byte_lo = char_to_byte_idx(line, self.cursor.col);
            let byte_hi = char_to_byte_idx(line, self.cursor.col + 1);
            line.replace_range(byte_lo..byte_hi, "");
        } else if self.cursor.row + 1 < self.lines.len() {
            let next = self.lines.remove(self.cursor.row + 1);
            self.lines[self.cursor.row].push_str(&next);
        }
    }

    /// Deletes everything inside the active selection (if any). Returns
    /// the removed text, the selection start (where the cursor lands),
    /// and `true` if anything was actually removed.
    pub fn delete_selection(&mut self) -> Option<(String, Cursor)> {
        let (start, end) = self.selection_range()?;
        if start == end {
            self.selection_anchor = None;
            return None;
        }
        let removed = self.text_in_range(start, end);
        self.delete_range(start, end);
        self.cursor = start;
        self.selection_anchor = None;
        Some((removed, start))
    }

    pub fn delete_range(&mut self, start: Cursor, end: Cursor) {
        if start.row == end.row {
            let line = &mut self.lines[start.row];
            let lo = char_to_byte_idx(line, start.col);
            let hi = char_to_byte_idx(line, end.col);
            line.replace_range(lo..hi, "");
            return;
        }
        // Multi-line: keep prefix of start row + suffix of end row.
        let head_prefix = {
            let line = &self.lines[start.row];
            let lo = char_to_byte_idx(line, start.col);
            line[..lo].to_string()
        };
        let tail_suffix = {
            let line = &self.lines[end.row];
            let hi = char_to_byte_idx(line, end.col);
            line[hi..].to_string()
        };
        self.lines[start.row] = head_prefix;
        self.lines[start.row].push_str(&tail_suffix);
        // Drop the inner lines between start.row and end.row inclusive
        // (we already merged the survivors into start.row).
        self.lines.drain(start.row + 1..=end.row);
    }

    /// Returns the text between two cursor points (start <= end).
    pub fn text_in_range(&self, start: Cursor, end: Cursor) -> String {
        if start.row == end.row {
            let line = &self.lines[start.row];
            let lo = char_to_byte_idx(line, start.col);
            let hi = char_to_byte_idx(line, end.col);
            return line[lo..hi].to_string();
        }
        let mut out = String::new();
        // First line: from start.col to end of line.
        let first = &self.lines[start.row];
        let lo = char_to_byte_idx(first, start.col);
        out.push_str(&first[lo..]);
        out.push('\n');
        // Middle lines: whole.
        for r in (start.row + 1)..end.row {
            out.push_str(&self.lines[r]);
            out.push('\n');
        }
        // Last line: from start to end.col.
        let last = &self.lines[end.row];
        let hi = char_to_byte_idx(last, end.col);
        out.push_str(&last[..hi]);
        out
    }
}

fn cursor_le(a: Cursor, b: Cursor) -> bool {
    (a.row, a.col) <= (b.row, b.col)
}

/// Converts a 0-based char-column inside a line to its byte index. The
/// requested column is allowed to be exactly `line.chars().count()`,
/// which returns `line.len()` (one past the last byte).
fn char_to_byte_idx(line: &str, col: usize) -> usize {
    if col == 0 {
        return 0;
    }
    let mut iter = line.char_indices();
    for _ in 0..col {
        match iter.next() {
            Some(_) => continue,
            None => return line.len(),
        }
    }
    iter.next().map(|(b, _)| b).unwrap_or(line.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer_has_one_blank_line() {
        let b = TextBuffer::new();
        assert_eq!(b.lines(), &[String::new()]);
        assert_eq!(b.line_count(), 1);
        assert!(b.is_empty());
    }

    #[test]
    fn from_str_splits_on_newline() {
        let b = TextBuffer::from_text("a\nb\nc");
        assert_eq!(b.lines(), &["a", "b", "c"]);
    }

    #[test]
    fn replace_all_resets_cursor() {
        let mut b = TextBuffer::from_text("xx");
        b.set_cursor(0, 2);
        b.replace_all("y");
        assert_eq!(b.cursor(), Cursor::new(0, 0));
        assert_eq!(b.text(), "y");
    }

    #[test]
    fn insert_char_advances_cursor() {
        let mut b = TextBuffer::new();
        b.insert_char('a');
        b.insert_char('b');
        assert_eq!(b.text(), "ab");
        assert_eq!(b.cursor(), Cursor::new(0, 2));
    }

    #[test]
    fn insert_newline_splits_line() {
        let mut b = TextBuffer::from_text("hello");
        b.set_cursor(0, 2);
        b.insert_newline();
        assert_eq!(b.text(), "he\nllo");
        assert_eq!(b.cursor(), Cursor::new(1, 0));
    }

    #[test]
    fn backspace_within_line() {
        let mut b = TextBuffer::from_text("ab");
        b.set_cursor(0, 2);
        b.backspace();
        assert_eq!(b.text(), "a");
        assert_eq!(b.cursor(), Cursor::new(0, 1));
    }

    #[test]
    fn backspace_at_line_head_joins_previous() {
        let mut b = TextBuffer::from_text("ab\ncd");
        b.set_cursor(1, 0);
        b.backspace();
        assert_eq!(b.text(), "abcd");
        assert_eq!(b.cursor(), Cursor::new(0, 2));
    }

    #[test]
    fn delete_forward_within_line() {
        let mut b = TextBuffer::from_text("abc");
        b.set_cursor(0, 1);
        b.delete_forward();
        assert_eq!(b.text(), "ac");
        assert_eq!(b.cursor(), Cursor::new(0, 1));
    }

    #[test]
    fn delete_forward_at_line_end_joins_next() {
        let mut b = TextBuffer::from_text("ab\ncd");
        b.set_cursor(0, 2);
        b.delete_forward();
        assert_eq!(b.text(), "abcd");
        assert_eq!(b.cursor(), Cursor::new(0, 2));
    }

    #[test]
    fn delete_selection_single_line() {
        let mut b = TextBuffer::from_text("abcdef");
        b.set_cursor(0, 1);
        b.start_selection();
        b.set_cursor(0, 4);
        let (removed, _) = b.delete_selection().expect("removed");
        assert_eq!(removed, "bcd");
        assert_eq!(b.text(), "aef");
        assert_eq!(b.cursor(), Cursor::new(0, 1));
    }

    #[test]
    fn delete_selection_multi_line() {
        let mut b = TextBuffer::from_text("ab\ncd\nef");
        b.set_cursor(0, 1);
        b.start_selection();
        b.set_cursor(2, 1);
        let (removed, _) = b.delete_selection().expect("removed");
        assert_eq!(removed, "b\ncd\ne");
        assert_eq!(b.text(), "af");
    }

    #[test]
    fn unicode_columns_are_in_chars_not_bytes() {
        let mut b = TextBuffer::from_text("\u{ac00}\u{ac01}"); // 가나
        assert_eq!(b.line_chars(0), 2);
        b.set_cursor(0, 1);
        b.insert_char('!');
        assert_eq!(b.text(), "\u{ac00}!\u{ac01}");
    }

    #[test]
    fn selection_range_orders_anchors() {
        let mut b = TextBuffer::from_text("abc");
        b.set_cursor(0, 2);
        b.start_selection();
        b.set_cursor(0, 0);
        let (s, e) = b.selection_range().unwrap();
        assert_eq!(s, Cursor::new(0, 0));
        assert_eq!(e, Cursor::new(0, 2));
    }
}
