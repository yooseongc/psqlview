use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders};
use ratatui::Frame;
use tui_textarea::{Input, Key, Scrolling, TextArea};

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
    fn insert_str_normalizes_crlf() {
        let mut e = EditorState::new();
        e.insert_str("a\r\nb");
        assert_eq!(e.text(), "a\nb");
    }
}
