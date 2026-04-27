use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::app::{App, FocusPane, QueryStatus};
use crate::db::TxStatus;

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
            let (line, col) = app.editor().cursor_line_col();
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

    if let Some(badge) = tx_badge_span(app.session.as_ref().map(|s| s.tx)) {
        spans.push(badge);
        spans.push(Span::raw("  "));
    }

    spans.push(Span::styled(
        "F1 help · F2/F3/F4 panes · F5 run · Esc cancel · Ctrl+Q quit",
        Style::default().fg(Color::DarkGray),
    ));

    let line = Line::from(spans);
    frame.render_widget(Paragraph::new(line), area);
}

/// Returns the transaction-state badge for the status bar, or `None`
/// when the session is in Idle (default) — Idle is invisible so the
/// status line stays compact during normal use.
pub fn tx_badge_span(tx: Option<TxStatus>) -> Option<Span<'static>> {
    match tx? {
        TxStatus::Idle => None,
        TxStatus::Active => Some(Span::styled(
            "[TX]",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        TxStatus::InError => Some(Span::styled(
            "[TX!]",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tx_badge_is_hidden_when_idle_or_disconnected() {
        assert!(tx_badge_span(None).is_none());
        assert!(tx_badge_span(Some(TxStatus::Idle)).is_none());
    }

    #[test]
    fn tx_badge_shows_for_active_and_error_states() {
        let active = tx_badge_span(Some(TxStatus::Active)).expect("badge");
        // Span content is the rendered text — assert via Display.
        assert_eq!(active.content, "[TX]");
        let in_error = tx_badge_span(Some(TxStatus::InError)).expect("badge");
        assert_eq!(in_error.content, "[TX!]");
    }
}
