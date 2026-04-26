//! EXPLAIN-shape detection and pretty-printer.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;

use crate::types::{CellValue, ResultSet};

/// Detects EXPLAIN-shaped results: a single column named "QUERY
/// PLAN" where each row holds one line of plan text. Returns the
/// lines if detected, `None` otherwise.
pub(super) fn extract_explain_lines(set: &ResultSet) -> Option<Vec<String>> {
    if set.columns.len() != 1 {
        return None;
    }
    if !set.columns[0].name.eq_ignore_ascii_case("QUERY PLAN") {
        return None;
    }
    Some(
        set.rows
            .iter()
            .map(|r| match r.first() {
                Some(CellValue::Text(s)) => s.clone(),
                Some(other) => other.to_string(),
                None => String::new(),
            })
            .collect(),
    )
}

/// Renders a single EXPLAIN plan line: depth-indented, with the node
/// name in bold cyan and the cost / timing tail dimmed. Slow nodes
/// (actual time over a millisecond threshold) get a red accent so
/// the hot spot is easy to find.
fn explain_line(raw: &str) -> Line<'static> {
    let trimmed_left = raw.trim_start();
    let depth_chars = raw.len() - trimmed_left.len();
    let indent: String = " ".repeat(depth_chars);

    let (head, tail) = match trimmed_left.find("  (") {
        Some(idx) => (&trimmed_left[..idx], &trimmed_left[idx..]),
        None => (trimmed_left, ""),
    };

    let mut spans: Vec<Span<'static>> = Vec::with_capacity(3);
    spans.push(Span::raw(indent));
    let head_style =
        if trimmed_left.starts_with("Planning ") || trimmed_left.starts_with("Execution ") {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        };
    spans.push(Span::styled(head.to_string(), head_style));

    if !tail.is_empty() {
        let tail_style = if let Some(ms) = parse_actual_total_ms(tail) {
            if ms >= 100.0 {
                Style::default().fg(Color::Red)
            } else if ms >= 10.0 {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::DarkGray)
            }
        } else {
            Style::default().fg(Color::DarkGray)
        };
        spans.push(Span::styled(tail.to_string(), tail_style));
    }

    Line::from(spans)
}

/// Parses the second number out of `actual time=X..Y` if present,
/// returning Y in milliseconds.
pub(super) fn parse_actual_total_ms(tail: &str) -> Option<f64> {
    let needle = "actual time=";
    let start = tail.find(needle)? + needle.len();
    let rest = &tail[start..];
    let dotdot = rest.find("..")?;
    let after = &rest[dotdot + 2..];
    let end = after
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(after.len());
    after[..end].parse::<f64>().ok()
}

pub(super) fn draw_explain(
    frame: &mut Frame<'_>,
    lines: &[String],
    selected: usize,
    block: Block<'_>,
    area: Rect,
) {
    let visible = area.height.saturating_sub(2) as usize;
    let scroll = if visible == 0 || lines.len() <= visible {
        0
    } else {
        let half = visible / 2;
        selected
            .saturating_sub(half)
            .min(lines.len().saturating_sub(visible))
    };
    let body: Vec<Line<'static>> = lines
        .iter()
        .skip(scroll)
        .take(visible.max(1))
        .enumerate()
        .map(|(i, raw)| {
            let mut line = explain_line(raw);
            if scroll + i == selected {
                line.style = Style::default().bg(Color::DarkGray);
            }
            line
        })
        .collect();
    let p = Paragraph::new(body).block(block);
    frame.render_widget(p, area);
}
