//! `Ctrl+G` inline overlay — jump the editor caret to a 1-based line
//! number.
//!
//! Owned by `App::goto_line`. While `Some`, it is an application-level
//! modal that absorbs all keystrokes (slotted between `file_prompt` and
//! `find` in the modal precedence chain). The overlay is a 3-row
//! bordered prompt anchored to the bottom of the editor pane — same
//! visual idiom as `file_prompt`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

#[derive(Debug, Default)]
pub struct GotoLineState {
    pub input: String,
}

impl GotoLineState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn input(&self) -> &str {
        &self.input
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum GotoLineOutcome {
    /// Keep the prompt open with the (possibly mutated) input.
    Stay,
    /// Close the prompt without jumping the caret.
    Cancel,
    /// Close the prompt and jump to this 1-based line number.
    Submit(usize),
}

/// Routes a key into the prompt. Only digits / Backspace / Enter / Esc
/// have meaning — everything else is silently swallowed so the user
/// can't accidentally pollute the input with stray characters.
pub fn handle_key(state: &mut GotoLineState, key: KeyEvent) -> GotoLineOutcome {
    match key.code {
        KeyCode::Esc => GotoLineOutcome::Cancel,
        KeyCode::Enter => match state.input.parse::<usize>() {
            Ok(n) => GotoLineOutcome::Submit(n),
            Err(_) => GotoLineOutcome::Cancel,
        },
        KeyCode::Backspace => {
            state.input.pop();
            GotoLineOutcome::Stay
        }
        KeyCode::Char(c)
            if (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
                && c.is_ascii_digit() =>
        {
            state.input.push(c);
            GotoLineOutcome::Stay
        }
        _ => GotoLineOutcome::Stay,
    }
}

pub fn draw(frame: &mut Frame<'_>, state: &GotoLineState, editor_area: Rect) {
    if editor_area.height < 3 {
        return;
    }
    let h: u16 = 3;
    let area = Rect {
        x: editor_area.x,
        y: editor_area.y + editor_area.height - h,
        width: editor_area.width,
        height: h,
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Goto line  [Enter \u{00b7} Esc cancel] ")
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

    let paragraph = Paragraph::new(line).block(block);
    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn digits_extend_input() {
        let mut s = GotoLineState::new();
        assert_eq!(
            handle_key(&mut s, k(KeyCode::Char('4'))),
            GotoLineOutcome::Stay
        );
        assert_eq!(
            handle_key(&mut s, k(KeyCode::Char('2'))),
            GotoLineOutcome::Stay
        );
        assert_eq!(s.input(), "42");
    }

    #[test]
    fn non_digit_chars_are_ignored() {
        let mut s = GotoLineState::new();
        s.input = "1".into();
        assert_eq!(
            handle_key(&mut s, k(KeyCode::Char('a'))),
            GotoLineOutcome::Stay
        );
        assert_eq!(
            handle_key(&mut s, k(KeyCode::Char('-'))),
            GotoLineOutcome::Stay
        );
        assert_eq!(s.input(), "1");
    }

    #[test]
    fn backspace_pops_input() {
        let mut s = GotoLineState::new();
        s.input = "123".into();
        assert_eq!(
            handle_key(&mut s, k(KeyCode::Backspace)),
            GotoLineOutcome::Stay
        );
        assert_eq!(s.input(), "12");
    }

    #[test]
    fn esc_cancels() {
        let mut s = GotoLineState::new();
        s.input = "42".into();
        assert_eq!(handle_key(&mut s, k(KeyCode::Esc)), GotoLineOutcome::Cancel);
    }

    #[test]
    fn enter_with_number_submits() {
        let mut s = GotoLineState::new();
        s.input = "42".into();
        assert_eq!(
            handle_key(&mut s, k(KeyCode::Enter)),
            GotoLineOutcome::Submit(42)
        );
    }

    #[test]
    fn enter_with_empty_cancels() {
        let mut s = GotoLineState::new();
        assert_eq!(
            handle_key(&mut s, k(KeyCode::Enter)),
            GotoLineOutcome::Cancel
        );
    }
}
