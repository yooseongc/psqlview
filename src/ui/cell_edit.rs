//! Inline cell-edit modal — single-line input box at the bottom of
//! the editor area. Owned by `App::cell_edit`.
//!
//! Captures one new value for a single result-set cell. On Enter the
//! caller parses the input via `sql_format::parse_cell_input` against
//! the cell's original type and (on success) opens the
//! [`crate::ui::confirm_update::ConfirmUpdateState`] modal with the
//! generated UPDATE.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::types::CellValue;

#[derive(Debug)]
pub struct CellEditState {
    /// Result-set row index (in the current sort order).
    pub row: usize,
    /// Result-set column index.
    pub col: usize,
    /// Column name — purely for the prompt title.
    pub col_name: String,
    /// Original cell value — used as both the parsing template and
    /// the shown "was" hint.
    pub original: CellValue,
    /// Live input. Pre-populated with the original value's display
    /// rendering so the user can edit rather than retype.
    pub input: String,
}

impl CellEditState {
    pub fn new(row: usize, col: usize, col_name: String, original: CellValue) -> Self {
        let input = match &original {
            CellValue::Null => String::new(),
            v => v.to_string(),
        };
        Self {
            row,
            col,
            col_name,
            original,
            input,
        }
    }

    pub fn push_char(&mut self, c: char) {
        self.input.push(c);
    }

    pub fn pop_char(&mut self) {
        self.input.pop();
    }

    /// Convenience used by the App to wipe the input and start over
    /// — useful when the user wants to clear to NULL (single Backspace
    /// loop is fine too, but Ctrl+U is the conventional shortcut).
    pub fn clear_input(&mut self) {
        self.input.clear();
    }
}

pub fn draw(frame: &mut Frame<'_>, state: &CellEditState, editor_area: Rect) {
    if editor_area.height < 3 {
        return;
    }
    let area = Rect {
        x: editor_area.x,
        y: editor_area.y + editor_area.height - 3,
        width: editor_area.width,
        height: 3,
    };
    let title = format!(
        " Cell edit: {} (row {}, col {})  [Enter confirm \u{00b7} Esc cancel \u{00b7} Backspace] ",
        state.col_name,
        state.row + 1,
        state.col + 1,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Yellow));
    let line = Line::from(vec![
        Span::styled(
            state.input.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("\u{2588}", Style::default().fg(Color::Yellow)),
    ]);
    let p = Paragraph::new(line).block(block);
    frame.render_widget(Clear, area);
    frame.render_widget(p, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_pre_populates_input_with_original_display() {
        let s = CellEditState::new(0, 1, "name".into(), CellValue::Text("alice".into()));
        assert_eq!(s.input, "alice");
        let s = CellEditState::new(0, 0, "id".into(), CellValue::Int(42));
        assert_eq!(s.input, "42");
        let s = CellEditState::new(0, 0, "n".into(), CellValue::Null);
        assert_eq!(s.input, "");
    }

    #[test]
    fn push_pop_clear_round_trip() {
        let mut s = CellEditState::new(0, 0, "c".into(), CellValue::Text(String::new()));
        s.push_char('a');
        s.push_char('b');
        assert_eq!(s.input, "ab");
        s.pop_char();
        assert_eq!(s.input, "a");
        s.clear_input();
        assert_eq!(s.input, "");
    }
}
