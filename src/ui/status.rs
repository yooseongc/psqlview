use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{App, FocusPane, QueryStatus};

pub fn draw(frame: &mut Frame<'_>, app: &App, area: Rect) {
    let mut spans: Vec<Span> = Vec::new();

    let label = app
        .session
        .as_ref()
        .map(|s| format!("{} · pg {}", s.label, s.server_version.display()))
        .unwrap_or_else(|| "disconnected".into());
    spans.push(Span::styled(
        label,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw("  "));

    let focus_label = match app.focus {
        FocusPane::Tree => "focus: schema".to_string(),
        FocusPane::Editor => {
            let (line, col) = app.editor.cursor_line_col();
            format!("focus: editor  ln {line}, col {col}")
        }
        FocusPane::Results => "focus: results".to_string(),
    };
    spans.push(Span::styled(focus_label, Style::default().fg(Color::Gray)));
    spans.push(Span::raw("  "));

    let status_span = match &app.query_status {
        QueryStatus::Idle => Span::styled("idle", Style::default().fg(Color::DarkGray)),
        QueryStatus::Running { started_at, .. } => Span::styled(
            format!(
                "running {:.1}s (Esc cancels)",
                started_at.elapsed().as_secs_f32()
            ),
            Style::default().fg(Color::Yellow),
        ),
        QueryStatus::Done { elapsed } => Span::styled(
            format!("done in {}ms", elapsed.as_millis()),
            Style::default().fg(Color::Green),
        ),
        QueryStatus::Cancelled => Span::styled("cancelled", Style::default().fg(Color::Yellow)),
        QueryStatus::Failed(_) => Span::styled("error", Style::default().fg(Color::Red)),
    };
    spans.push(status_span);
    spans.push(Span::raw("  "));

    spans.push(Span::styled(
        "F2/F3/F4 tree/editor/results · F5 run · Esc cancel · Ctrl+Q quit",
        Style::default().fg(Color::DarkGray),
    ));

    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}
