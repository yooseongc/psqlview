use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders};
use ratatui::Frame;
use tui_textarea::{CursorMove, Input, Key, Scrolling, TextArea};

/// Single regex highlighting common SQL keywords. `(?i)` makes it case-
/// insensitive so users can write `select` or `SELECT` and either gets
/// the same accent. Keep the alternation list in sync with
/// [`crate::ui::autocomplete::SQL_KEYWORDS`] when adding or removing
/// keywords.
const SQL_KEYWORD_REGEX: &str = r"(?i)\b(SELECT|FROM|WHERE|JOIN|ON|LEFT|RIGHT|INNER|OUTER|FULL|CROSS|GROUP|BY|HAVING|ORDER|LIMIT|OFFSET|INSERT|INTO|VALUES|UPDATE|SET|DELETE|CREATE|TABLE|VIEW|INDEX|DROP|ALTER|ADD|COLUMN|AS|DISTINCT|UNION|ALL|WITH|CASE|WHEN|THEN|ELSE|END|AND|OR|NOT|NULL|IS|IN|LIKE|ILIKE|BETWEEN|EXPLAIN|ANALYZE|RETURNING)\b";

use super::focus_style;

pub struct EditorState {
    area: TextArea<'static>,
}

impl Default for EditorState {
    fn default() -> Self {
        Self::new()
    }
}

impl EditorState {
    pub fn new() -> Self {
        let mut area = TextArea::default();
        area.set_line_number_style(Style::default().fg(Color::DarkGray));
        area.set_placeholder_text("-- F5 / Ctrl+Enter to run, Tab = autocomplete");
        area.set_cursor_line_style(Style::default().add_modifier(Modifier::UNDERLINED));
        area.set_style(Style::default().fg(Color::White));
        // Light syntax highlighting via the regex search facility:
        // every SQL keyword gets the same cyan/bold accent. Single
        // pattern, so we can't differentiate keywords from strings or
        // numbers, but it's a meaningful visual cue for free.
        let _ = area.set_search_pattern(SQL_KEYWORD_REGEX);
        area.set_search_style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
        Self { area }
    }

    pub fn text(&self) -> String {
        self.area.lines().join("\n")
    }

    /// Current cursor position, 1-indexed for human display.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        let (row, col) = self.area.cursor();
        (row + 1, col + 1)
    }

    /// Returns the currently selected text, or `None` if no selection is
    /// active. Used to let F5/Ctrl+Enter run just the highlighted SQL.
    pub fn selected_text(&self) -> Option<String> {
        let ((r0, c0), (r1, c1)) = self.area.selection_range()?;
        let lines = self.area.lines();
        if r0 == r1 {
            let line = lines.get(r0)?;
            let chars: Vec<char> = line.chars().collect();
            let end = c1.min(chars.len());
            let start = c0.min(end);
            return Some(chars[start..end].iter().collect());
        }
        let mut out = String::new();
        // First line: from c0 to end.
        if let Some(first) = lines.get(r0) {
            let chars: Vec<char> = first.chars().collect();
            let start = c0.min(chars.len());
            out.extend(&chars[start..]);
            out.push('\n');
        }
        // Middle lines: whole line.
        for r in (r0 + 1)..r1 {
            if let Some(line) = lines.get(r) {
                out.push_str(line);
                out.push('\n');
            }
        }
        // Last line: from start to c1.
        if let Some(last) = lines.get(r1) {
            let chars: Vec<char> = last.chars().collect();
            let end = c1.min(chars.len());
            out.extend(&chars[..end]);
        }
        Some(out)
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        let input = Input::from(key);
        self.area.input(input);
    }

    /// Returns the identifier prefix ending at the cursor column, or an empty
    /// string when the cursor is not sitting after `[A-Za-z_][A-Za-z0-9_]*`.
    pub fn word_prefix_before_cursor(&self) -> String {
        let (row, col) = self.area.cursor();
        let Some(line) = self.area.lines().get(row) else {
            return String::new();
        };
        let chars: Vec<char> = line.chars().collect();
        let col = col.min(chars.len());
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
        // First character of a SQL identifier cannot be a digit.
        if chars[start].is_ascii_digit() {
            return String::new();
        }
        chars[start..col].iter().collect()
    }

    /// Replaces the identifier prefix ending at the cursor with `new`.
    ///
    /// Assumes the cursor is sitting immediately after the prefix (this is
    /// the state right after typing or after the autocomplete popup opens
    /// — the only places this is called from). If the cursor were elsewhere,
    /// the backspaces would eat the wrong characters.
    pub fn replace_word_prefix(&mut self, new: &str) {
        let prefix_len = self.word_prefix_before_cursor().chars().count();
        for _ in 0..prefix_len {
            self.area.input(Input {
                key: Key::Backspace,
                ctrl: false,
                alt: false,
                shift: false,
            });
        }
        for c in new.chars() {
            self.area.input(Input {
                key: Key::Char(c),
                ctrl: false,
                alt: false,
                shift: false,
            });
        }
    }

    /// Inserts an arbitrary string at the cursor, preserving newlines and
    /// tabs. Used for bracketed paste. CRLF is normalized to LF so Windows
    /// clipboard contents don't produce blank lines.
    pub fn insert_str(&mut self, s: &str) {
        for c in s.chars() {
            let key = match c {
                '\r' => continue, // drop; real newline is the following \n
                '\n' => Key::Enter,
                '\t' => Key::Tab,
                other => Key::Char(other),
            };
            self.area.input(Input {
                key,
                ctrl: false,
                alt: false,
                shift: false,
            });
        }
    }

    /// Scrolls the viewport by `delta` lines (negative = up). Matches the
    /// `i32` signature used by `ResultsState::scroll_rows` /
    /// `SchemaTreeState::scroll_rows` so callers don't juggle types;
    /// the value is clamped into `i16` for tui-textarea internally.
    pub fn scroll_lines(&mut self, delta: i32) {
        let rows = delta.clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        self.area.scroll(Scrolling::Delta { rows, cols: 0 });
    }

    /// Replaces the entire buffer with `s`. Used for history recall.
    pub fn set_text(&mut self, s: &str) {
        // Select all and delete, then insert. Preserves tui-textarea's
        // internal history/undo stack without poking at private state.
        self.area.select_all();
        self.area.cut();
        self.insert_str(s);
    }

    pub fn insert_spaces(&mut self, n: usize) {
        for _ in 0..n {
            self.area.input(Input {
                key: Key::Char(' '),
                ctrl: false,
                alt: false,
                shift: false,
            });
        }
    }

    /// Returns the inclusive `[start_row, end_row]` range covered by the
    /// active selection, or `None` if no selection is active.
    pub fn selected_line_range(&self) -> Option<(usize, usize)> {
        let ((r0, _), (r1, _)) = self.area.selection_range()?;
        Some((r0, r1))
    }

    /// Inserts two spaces at the start of every line in the inclusive
    /// range. Used for block-indent of a selection.
    pub fn indent_lines(&mut self, start: usize, end: usize) {
        let total = self.area.lines().len();
        if total == 0 {
            return;
        }
        let end = end.min(total - 1);
        for row in start..=end {
            // Move to head of `row`, insert two spaces.
            self.move_cursor_to(row, 0);
            self.area.input(Input {
                key: Key::Char(' '),
                ctrl: false,
                alt: false,
                shift: false,
            });
            self.area.input(Input {
                key: Key::Char(' '),
                ctrl: false,
                alt: false,
                shift: false,
            });
        }
    }

    /// Removes up to two leading spaces from every line in the inclusive
    /// range. Lines with no leading whitespace are left alone.
    pub fn outdent_lines(&mut self, start: usize, end: usize) {
        let total = self.area.lines().len();
        if total == 0 {
            return;
        }
        let end = end.min(total - 1);
        for row in start..=end {
            let leading = self
                .area
                .lines()
                .get(row)
                .map(|l| l.chars().take_while(|c| *c == ' ').count())
                .unwrap_or(0);
            let remove = leading.min(2);
            if remove == 0 {
                continue;
            }
            self.move_cursor_to(row, 0);
            for _ in 0..remove {
                self.area.input(Input {
                    key: Key::Delete,
                    ctrl: false,
                    alt: false,
                    shift: false,
                });
            }
        }
    }

    fn move_cursor_to(&mut self, row: usize, col: usize) {
        // CursorMove::Jump uses u16 row/col and clamps to buffer bounds.
        // Cancel any active selection first so callers don't mutate it.
        self.area.cancel_selection();
        self.area.move_cursor(CursorMove::Jump(
            row.try_into().unwrap_or(u16::MAX),
            col.try_into().unwrap_or(u16::MAX),
        ));
    }

    /// Removes up to 2 leading spaces from the current line. No-op if the line
    /// has no leading whitespace. Cursor column is clamped to the new line
    /// length if it was inside the removed run.
    pub fn outdent_current_line(&mut self) {
        let (row, col) = self.area.cursor();
        let Some(line) = self.area.lines().get(row).cloned() else {
            return;
        };
        let leading = line.chars().take_while(|c| *c == ' ').count();
        let remove = leading.min(2);
        if remove == 0 {
            return;
        }
        // Move cursor to line start, delete `remove` chars, restore column.
        // tui-textarea has no direct "delete N chars" API from arbitrary
        // positions, so we use Home + Delete.
        self.area.input(Input {
            key: Key::Home,
            ctrl: false,
            alt: false,
            shift: false,
        });
        for _ in 0..remove {
            self.area.input(Input {
                key: Key::Delete,
                ctrl: false,
                alt: false,
                shift: false,
            });
        }
        let new_col = col.saturating_sub(remove);
        for _ in 0..new_col {
            self.area.input(Input {
                key: Key::Right,
                ctrl: false,
                alt: false,
                shift: false,
            });
        }
    }

    #[cfg(test)]
    fn type_text(&mut self, s: &str) {
        use crossterm::event::{KeyCode, KeyModifiers};
        for c in s.chars() {
            let key = match c {
                '\n' => KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                _ => KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            };
            self.handle_key(key);
        }
    }
}

pub fn draw(frame: &mut Frame<'_>, state: &mut EditorState, focused: bool, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" SQL editor  [F5 run \u{00b7} Tab complete] ")
        .border_style(focus_style(focused));
    state.area.set_block(block);
    frame.render_widget(&state.area, area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyModifiers};

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
}
