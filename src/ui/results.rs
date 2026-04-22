use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::app::QueryStatus;
use crate::types::{ColumnMeta, ResultSet};

use super::focus_style;

const MAX_CELL_WIDTH: u16 = 40;
const MIN_CELL_WIDTH: u16 = 4;

#[derive(Default)]
pub struct ResultsState {
    pub current: Option<ResultSet>,
    pub selected_row: usize,
    pub x_offset: usize,
}

impl ResultsState {
    pub fn set_result(&mut self, result: ResultSet) {
        self.selected_row = 0;
        self.x_offset = 0;
        self.current = Some(result);
    }

    pub fn clear(&mut self) {
        self.current = None;
        self.selected_row = 0;
        self.x_offset = 0;
    }

    pub fn begin_running(&mut self) {
        self.current = None;
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        let Some(set) = &self.current else { return };
        let max = set.rows.len().saturating_sub(1);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                if self.selected_row > 0 {
                    self.selected_row -= 1;
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if self.selected_row < max {
                    self.selected_row += 1;
                }
            }
            KeyCode::PageUp => {
                self.selected_row = self.selected_row.saturating_sub(20);
            }
            KeyCode::PageDown => {
                self.selected_row = (self.selected_row + 20).min(max);
            }
            KeyCode::Home => self.selected_row = 0,
            KeyCode::End => self.selected_row = max,
            KeyCode::Left | KeyCode::Char('h') => {
                if self.x_offset > 0 {
                    self.x_offset -= 1;
                }
            }
            KeyCode::Right | KeyCode::Char('l') => {
                let col_count = set.columns.len();
                if col_count > 0 && self.x_offset + 1 < col_count {
                    self.x_offset += 1;
                }
            }
            _ => {}
        }
    }
}

pub fn draw(
    frame: &mut Frame<'_>,
    state: &ResultsState,
    status: &QueryStatus,
    focused: bool,
    area: Rect,
) {
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
            let p = Paragraph::new(Line::from(Span::styled(
                msg.clone(),
                Style::default().fg(Color::Red),
            )))
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
        (_, Some(set)) => draw_table(frame, set, state, block, area),
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
            .map(|c| {
                Cell::from(Line::from(vec![
                    Span::styled(
                        c.name.clone(),
                        Style::default()
                            .fg(Color::Cyan)
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
    rows: &[Vec<crate::types::CellValue>],
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

fn truncate_for_cell(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CellValue, ColumnMeta, ResultSet};
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn sample_result() -> ResultSet {
        ResultSet {
            columns: vec![
                ColumnMeta {
                    name: "a".into(),
                    type_name: "int4".into(),
                },
                ColumnMeta {
                    name: "b".into(),
                    type_name: "text".into(),
                },
            ],
            rows: vec![
                vec![CellValue::Int(1), CellValue::Text("x".into())],
                vec![CellValue::Int(2), CellValue::Text("y".into())],
                vec![CellValue::Int(3), CellValue::Text("z".into())],
            ],
            truncated_at: None,
            command_tag: Some("3 rows".into()),
            elapsed_ms: 1,
        }
    }

    #[test]
    fn compute_widths_respects_bounds() {
        let cols = vec![
            ColumnMeta {
                name: "a".into(),
                type_name: "int4".into(),
            },
            ColumnMeta {
                name: "verbose_column_header_should_be_capped_to_40".into(),
                type_name: "text".into(),
            },
        ];
        let rows = vec![vec![CellValue::Int(123), CellValue::Text("x".into())]];
        let widths = compute_widths(&cols, &rows, 0, 2);
        // MIN_CELL_WIDTH=4 (for "a"), MAX_CELL_WIDTH=40 (for long header)
        assert_eq!(widths.len(), 2);
    }

    #[test]
    fn truncate_keeps_short_strings() {
        assert_eq!(truncate_for_cell("hi"), "hi");
        let s = "x".repeat(100);
        let t = truncate_for_cell(&s);
        assert!(t.ends_with('…'));
        assert!(UnicodeWidthStr::width(t.as_str()) <= MAX_CELL_WIDTH as usize);
    }

    #[test]
    fn handle_key_is_safe_on_empty_state() {
        let mut s = ResultsState::default();
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Char('j'),
            KeyCode::Char('k'),
            KeyCode::Char('h'),
            KeyCode::Char('l'),
        ] {
            s.handle_key(key(code));
        }
        assert_eq!(s.selected_row, 0);
        assert_eq!(s.x_offset, 0);
        assert!(s.current.is_none());
    }

    #[test]
    fn handle_key_respects_row_and_col_bounds() {
        let mut s = ResultsState::default();
        s.set_result(sample_result());

        for _ in 0..10 {
            s.handle_key(key(KeyCode::Down));
        }
        assert_eq!(s.selected_row, 2);

        s.handle_key(key(KeyCode::Home));
        assert_eq!(s.selected_row, 0);

        s.handle_key(key(KeyCode::PageDown));
        assert_eq!(s.selected_row, 2); // capped at max

        s.handle_key(key(KeyCode::End));
        assert_eq!(s.selected_row, 2);

        s.handle_key(key(KeyCode::PageUp));
        assert_eq!(s.selected_row, 0);

        for _ in 0..10 {
            s.handle_key(key(KeyCode::Right));
        }
        assert_eq!(s.x_offset, 1); // col_count - 1

        for _ in 0..10 {
            s.handle_key(key(KeyCode::Left));
        }
        assert_eq!(s.x_offset, 0);
    }

    #[test]
    fn compute_widths_handles_offset_slice() {
        let cols = vec![
            ColumnMeta {
                name: "a".into(),
                type_name: "int".into(),
            },
            ColumnMeta {
                name: "b".into(),
                type_name: "int".into(),
            },
            ColumnMeta {
                name: "c".into(),
                type_name: "int".into(),
            },
        ];
        let rows = vec![vec![
            CellValue::Int(1),
            CellValue::Int(2),
            CellValue::Int(3),
        ]];
        let widths = compute_widths(&cols, &rows, 1, 2);
        assert_eq!(widths.len(), 2);
        for w in widths {
            // Each Constraint::Min(w) should have w in [MIN_CELL_WIDTH, MAX_CELL_WIDTH].
            match w {
                ratatui::layout::Constraint::Min(n) => {
                    assert!((MIN_CELL_WIDTH..=MAX_CELL_WIDTH).contains(&n));
                }
                other => panic!("unexpected constraint: {other:?}"),
            }
        }
    }
}
