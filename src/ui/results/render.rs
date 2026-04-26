//! Top-level results-pane render — block + title + dispatch into
//! the explain or table layout. Column-width and cell-truncation
//! helpers live here too.

use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use super::explain::draw_explain;
use super::{ResultsState, SortDir, SortState};
use crate::app::QueryStatus;
use crate::types::{CellValue, ColumnMeta, ResultSet};
use crate::ui::focus_style;

pub(super) const MAX_CELL_WIDTH: u16 = 40;
pub(super) const MIN_CELL_WIDTH: u16 = 4;

pub fn draw(
    frame: &mut Frame<'_>,
    state: &mut ResultsState,
    status: &QueryStatus,
    focused: bool,
    area: Rect,
) {
    // 2 border rows + 1 header row = 3 non-data rows.
    state.visible_rows = area.height.saturating_sub(3) as usize;
    let title = match (status, &state.current) {
        (QueryStatus::Running { started_at, .. }, _) => {
            format!(
                " Results — running… ({:.1}s, Esc to cancel) ",
                started_at.elapsed().as_secs_f32()
            )
        }
        (QueryStatus::Failed(_), _) => " Results — error ".into(),
        (QueryStatus::Cancelled, _) => " Results — cancelled ".into(),
        (_, Some(set)) => {
            let tag = set.command_tag.as_deref().unwrap_or("");
            format!(" Results — {} ({}ms) ", tag, set.elapsed_ms)
        }
        _ => " Results ".into(),
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(focus_style(focused));

    match (status, &state.current) {
        (QueryStatus::Failed(msg), _) => {
            // Structured Postgres errors are multi-line (DETAIL /
            // HINT / POSITION on their own lines). Split explicitly
            // so Paragraph doesn't join.
            let lines: Vec<Line> = msg
                .split('\n')
                .map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(Color::Red))))
                .collect();
            let p = Paragraph::new(lines)
                .block(block)
                .wrap(ratatui::widgets::Wrap { trim: false });
            frame.render_widget(p, area);
        }
        (QueryStatus::Running { .. }, _) => {
            let p = Paragraph::new("executing…")
                .block(block)
                .style(Style::default().fg(Color::Yellow));
            frame.render_widget(p, area);
        }
        (_, None) => {
            let p = Paragraph::new("(no results yet — press F5 to execute)")
                .block(block)
                .style(Style::default().fg(Color::DarkGray));
            frame.render_widget(p, area);
        }
        (_, Some(set)) => {
            if let Some(lines) = state.explain_lines.as_ref() {
                draw_explain(frame, lines, state.selected_row, block, area);
            } else {
                draw_table(frame, set, state, block, area);
            }
        }
    }
}

fn draw_table(
    frame: &mut Frame<'_>,
    set: &ResultSet,
    state: &ResultsState,
    block: Block<'_>,
    area: Rect,
) {
    if set.columns.is_empty() {
        let tag = set.command_tag.clone().unwrap_or_default();
        let p = Paragraph::new(format!("{} (no rows)", tag))
            .block(block)
            .style(Style::default().fg(Color::Green));
        frame.render_widget(p, area);
        return;
    }

    let visible_cols = visible_columns(&set.columns, state.x_offset);
    let widths = compute_widths(&set.columns, &set.rows, state.x_offset, visible_cols);

    let header = Row::new(
        set.columns[state.x_offset..state.x_offset + visible_cols]
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let abs = state.x_offset + i;
                let arrow = match state.sort {
                    Some(SortState {
                        col,
                        dir: SortDir::Asc,
                    }) if col == abs => " \u{2191}",
                    Some(SortState {
                        col,
                        dir: SortDir::Desc,
                    }) if col == abs => " \u{2193}",
                    _ => "",
                };
                Cell::from(Line::from(vec![
                    Span::styled(
                        c.name.clone(),
                        Style::default()
                            .fg(Color::Cyan)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        arrow.to_string(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {}", c.type_name),
                        Style::default().fg(Color::DarkGray),
                    ),
                ]))
            })
            .collect::<Vec<_>>(),
    )
    .height(1);

    let rows: Vec<Row> = set
        .rows
        .iter()
        .map(|r| {
            let slice = &r[state.x_offset..state.x_offset + visible_cols];
            Row::new(
                slice
                    .iter()
                    .map(|v| Cell::from(truncate_for_cell(&v.to_string()))),
            )
        })
        .collect();

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut table_state = TableState::default();
    if !set.rows.is_empty() {
        table_state.select(Some(state.selected_row.min(set.rows.len() - 1)));
    }

    frame.render_stateful_widget(table, area, &mut table_state);
}

fn visible_columns(cols: &[ColumnMeta], offset: usize) -> usize {
    cols.len().saturating_sub(offset).max(1)
}

pub fn compute_widths(
    columns: &[ColumnMeta],
    rows: &[Vec<CellValue>],
    offset: usize,
    count: usize,
) -> Vec<Constraint> {
    let end = (offset + count).min(columns.len());
    columns[offset..end]
        .iter()
        .enumerate()
        .map(|(i, col)| {
            let real_i = offset + i;
            let mut w = UnicodeWidthStr::width(col.name.as_str()) as u16;
            for row in rows.iter().take(256) {
                if let Some(v) = row.get(real_i) {
                    let text = v.to_string();
                    let tw = UnicodeWidthStr::width(text.as_str()) as u16;
                    if tw > w {
                        w = tw;
                    }
                }
            }
            let w = w.clamp(MIN_CELL_WIDTH, MAX_CELL_WIDTH);
            Constraint::Min(w)
        })
        .collect()
}

pub(super) fn truncate_for_cell(s: &str) -> String {
    let max = MAX_CELL_WIDTH as usize;
    if UnicodeWidthStr::width(s) <= max {
        return s.replace('\n', "⏎ ");
    }
    let mut out = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw + 1 > max {
            out.push('…');
            break;
        }
        out.push(if ch == '\n' { '⏎' } else { ch });
        w += cw;
    }
    out
}
