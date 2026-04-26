//! Find / Find-Replace overlay render.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use super::{FindMode, FindState, ReplaceFocus};

pub fn draw(frame: &mut Frame<'_>, state: &FindState, editor_area: Rect) {
    let needs_height: u16 = if state.mode == FindMode::Replace {
        4
    } else {
        3
    };
    if editor_area.height < needs_height {
        return;
    }
    let area = Rect {
        x: editor_area.x,
        y: editor_area.y + editor_area.height - needs_height,
        width: editor_area.width,
        height: needs_height,
    };
    let case_label = if state.case_sensitive { "Aa" } else { "aA" };
    let status = state.status_label();
    let title = match state.mode {
        FindMode::Find => format!(
            " Find {case_label}  {status}  [Enter / F3 next \u{00b7} Shift+F3 prev \u{00b7} Alt+C case \u{00b7} Esc] "
        ),
        FindMode::Replace => format!(
            " Find/Replace {case_label}  {status}  [Tab field \u{00b7} Enter replace \u{00b7} Alt+A all \u{00b7} Esc] "
        ),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Yellow));

    let needle_active = state.mode == FindMode::Find || state.focus == ReplaceFocus::Needle;
    let replacement_active =
        state.mode == FindMode::Replace && state.focus == ReplaceFocus::Replacement;
    let active_caret = Span::styled("\u{2588}", Style::default().fg(Color::Yellow));

    let mut content_lines: Vec<Line<'static>> = Vec::with_capacity(2);
    let needle_label_style = if needle_active {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let mut needle_spans: Vec<Span<'static>> = vec![
        Span::styled(" Find: ", needle_label_style),
        Span::styled(
            state.needle.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if needle_active {
        needle_spans.push(active_caret.clone());
    }
    content_lines.push(Line::from(needle_spans));

    if state.mode == FindMode::Replace {
        let label_style = if replacement_active {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let mut spans: Vec<Span<'static>> = vec![
            Span::styled(" Repl: ", label_style),
            Span::styled(
                state.replacement.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        if replacement_active {
            spans.push(active_caret);
        }
        content_lines.push(Line::from(spans));
    }

    let paragraph = Paragraph::new(content_lines).block(block);
    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}
