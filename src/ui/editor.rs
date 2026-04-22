use crossterm::event::KeyEvent;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::{Block, Borders};
use ratatui::Frame;
use tui_textarea::{Input, TextArea};

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
        area.set_placeholder_text("-- F5 to run, Tab cycles focus");
        area.set_cursor_line_style(Style::default().add_modifier(Modifier::UNDERLINED));
        area.set_style(Style::default().fg(Color::White));
        Self { area }
    }

    pub fn text(&self) -> String {
        self.area.lines().join("\n")
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        let input = Input::from(key);
        self.area.input(input);
    }
}

pub fn draw(frame: &mut Frame<'_>, state: &mut EditorState, focused: bool, area: Rect) {
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" SQL editor  [F5 run] ")
        .border_style(focus_style(focused));
    state.area.set_block(block);
    frame.render_widget(&state.area, area);
}
