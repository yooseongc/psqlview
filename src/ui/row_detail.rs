use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::types::ResultSet;

/// Modal that shows every column of a single row — mostly for cells whose
/// truncated grid display hides the full value (long text, jsonb, etc).
#[derive(Default)]
pub struct RowDetailState {
    pub open: bool,
    pub scroll: u16,
}

impl RowDetailState {
    pub fn open(&mut self) {
        self.open = true;
        self.scroll = 0;
    }
    pub fn close(&mut self) {
        self.open = false;
        self.scroll = 0;
    }
    pub fn scroll_up(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_sub(n);
    }
    pub fn scroll_down(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_add(n);
    }
}

pub fn draw(
    frame: &mut Frame<'_>,
    state: &RowDetailState,
    result: &ResultSet,
    row_idx: usize,
    screen: Rect,
) {
    if !state.open {
        return;
    }
    let Some(row) = result.rows.get(row_idx) else {
        return;
    };

    // Centered modal: 70% × 70% of the screen, bounded.
    let w = (screen.width * 7 / 10).max(40).min(screen.width);
    let h = (screen.height * 7 / 10).max(8).min(screen.height);
    let x = screen.x + screen.width.saturating_sub(w) / 2;
    let y = screen.y + screen.height.saturating_sub(h) / 2;
    let area = Rect {
        x,
        y,
        width: w,
        height: h,
    };

    let mut lines: Vec<Line> = Vec::new();
    for (i, col) in result.columns.iter().enumerate() {
        lines.push(Line::from(vec![
            Span::styled(
                col.name.clone(),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("  [{}]", col.type_name),
                Style::default().fg(Color::DarkGray),
            ),
        ]));
        let value = row
            .get(i)
            .map(|c| c.to_string())
            .unwrap_or_else(|| "<missing>".into());
        for sub in value.split('\n') {
            lines.push(Line::from(Span::styled(
                format!("  {sub}"),
                Style::default().fg(Color::White),
            )));
        }
        lines.push(Line::from(""));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(
            " Row {} of {}  [Esc/Enter to close] ",
            row_idx + 1,
            result.rows.len()
        ))
        .border_style(Style::default().fg(Color::Yellow));

    let total_lines = lines.len() as u16;
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((clamp_scroll(state.scroll, total_lines, h), 0));

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

fn clamp_scroll(requested: u16, total: u16, visible: u16) -> u16 {
    let max = total.saturating_sub(visible.saturating_sub(2));
    requested.min(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_scroll_never_exceeds_available() {
        assert_eq!(clamp_scroll(0, 10, 5), 0);
        assert_eq!(clamp_scroll(999, 10, 5), 10u16.saturating_sub(3));
        // visible >= total → no scroll.
        assert_eq!(clamp_scroll(5, 10, 100), 0);
    }

    #[test]
    fn state_toggles() {
        let mut s = RowDetailState::default();
        assert!(!s.open);
        s.open();
        assert!(s.open);
        s.scroll_down(5);
        assert_eq!(s.scroll, 5);
        s.close();
        assert!(!s.open);
        assert_eq!(s.scroll, 0);
    }
}
