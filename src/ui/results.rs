use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use crate::app::QueryStatus;
use crate::types::{CellValue, ColumnMeta, ResultSet};

use super::focus_style;

const MAX_CELL_WIDTH: u16 = 40;
const MIN_CELL_WIDTH: u16 = 4;

#[derive(Default)]
pub struct ResultsState {
    pub current: Option<ResultSet>,
    pub selected_row: usize,
    pub x_offset: usize,
    /// Last rendered visible data-row count. Updated each frame so PageUp/
    /// PageDown can step by a screenful instead of a fixed 20.
    pub visible_rows: usize,
    /// Active client-side sort, if any. Pressing `s` cycles through
    /// Asc → Desc → off for the leftmost visible column.
    pub sort: Option<SortState>,
    /// Snapshot of `current.rows` taken before the first sort, used to
    /// restore original order when the sort cycles back to off.
    original_rows: Option<Vec<Vec<crate::types::CellValue>>>,
    /// When the result is an EXPLAIN-style "QUERY PLAN" output, this
    /// holds each plan line for the dedicated explain renderer to
    /// pretty-print. `None` for normal SELECTs.
    pub explain_lines: Option<Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortState {
    pub col: usize,
    pub dir: SortDir,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

impl ResultsState {
    pub fn set_result(&mut self, result: ResultSet) {
        self.selected_row = 0;
        self.x_offset = 0;
        self.sort = None;
        self.original_rows = None;
        self.explain_lines = extract_explain_lines(&result);
        self.current = Some(result);
    }

    pub fn clear(&mut self) {
        self.current = None;
        self.selected_row = 0;
        self.x_offset = 0;
        self.sort = None;
        self.original_rows = None;
        self.explain_lines = None;
    }

    /// Cycles the sort on the leftmost visible column: off → Asc → Desc
    /// → off. Sorting is client-side; the underlying ResultSet's row
    /// vector is mutated, with the pre-sort order remembered so the
    /// "off" state can restore it.
    pub fn cycle_sort_on_current_column(&mut self) {
        let Some(set) = self.current.as_mut() else {
            return;
        };
        if set.columns.is_empty() {
            return;
        }
        let col = self.x_offset.min(set.columns.len() - 1);
        let next = match self.sort {
            None => Some(SortState {
                col,
                dir: SortDir::Asc,
            }),
            Some(s) if s.col != col => Some(SortState {
                col,
                dir: SortDir::Asc,
            }),
            Some(SortState {
                dir: SortDir::Asc, ..
            }) => Some(SortState {
                col,
                dir: SortDir::Desc,
            }),
            Some(SortState {
                dir: SortDir::Desc, ..
            }) => None,
        };
        if self.original_rows.is_none() {
            self.original_rows = Some(set.rows.clone());
        }
        match next {
            Some(s) => {
                set.rows.sort_by(|a, b| {
                    let cmp = compare_cells(a.get(s.col), b.get(s.col));
                    match s.dir {
                        SortDir::Asc => cmp,
                        SortDir::Desc => cmp.reverse(),
                    }
                });
                self.sort = Some(s);
            }
            None => {
                if let Some(orig) = self.original_rows.clone() {
                    set.rows = orig;
                }
                self.sort = None;
            }
        }
        self.selected_row = 0;
    }

    pub fn begin_running(&mut self) {
        self.current = None;
    }

    pub fn scroll_rows(&mut self, delta: i32) {
        let Some(set) = &self.current else { return };
        let max = set.rows.len().saturating_sub(1) as i32;
        let new = (self.selected_row as i32 + delta).clamp(0, max);
        self.selected_row = new as usize;
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        let Some(set) = &self.current else { return };
        let max = set.rows.len().saturating_sub(1);
        // Ctrl+Left / Ctrl+Right jump to the first / last column. Kept
        // out of the main match so the non-Ctrl arrows keep their
        // single-column-scroll behavior.
        if key
            .modifiers
            .contains(crossterm::event::KeyModifiers::CONTROL)
        {
            match key.code {
                KeyCode::Left => {
                    self.x_offset = 0;
                    return;
                }
                KeyCode::Right => {
                    self.x_offset = set.columns.len().saturating_sub(1);
                    return;
                }
                _ => {}
            }
        }
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
                let step = self.visible_rows.max(1);
                self.selected_row = self.selected_row.saturating_sub(step);
            }
            KeyCode::PageDown => {
                let step = self.visible_rows.max(1);
                self.selected_row = (self.selected_row + step).min(max);
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
            KeyCode::Char('s') => self.cycle_sort_on_current_column(),
            _ => {}
        }
    }
}

/// Detects EXPLAIN-shaped results: a single column named "QUERY PLAN"
/// where each row holds one line of plan text. Returns the lines if
/// detected, `None` otherwise.
fn extract_explain_lines(set: &ResultSet) -> Option<Vec<String>> {
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
/// (actual time over a millisecond threshold) get a red accent so the
/// hot spot is easy to find.
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
fn parse_actual_total_ms(tail: &str) -> Option<f64> {
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

/// Type-aware ordering for `CellValue` so client-side sort behaves
/// sensibly. Same-type variants get their natural ordering; mixed
/// numeric (Int vs Float) gets coerced to f64; nulls sort last; any
/// other mix falls back to the textual `Display` form.
fn compare_cells(a: Option<&CellValue>, b: Option<&CellValue>) -> std::cmp::Ordering {
    use std::cmp::Ordering::*;
    use CellValue::*;
    match (a, b) {
        (None, None) => Equal,
        (None, _) => Greater,
        (_, None) => Less,
        (Some(Null), Some(Null)) => Equal,
        (Some(Null), _) => Greater,
        (_, Some(Null)) => Less,
        (Some(Bool(x)), Some(Bool(y))) => x.cmp(y),
        (Some(Int(x)), Some(Int(y))) => x.cmp(y),
        (Some(Float(x)), Some(Float(y))) => x.partial_cmp(y).unwrap_or(Equal),
        (Some(Numeric(x)), Some(Numeric(y))) => x.cmp(y),
        (Some(Int(x)), Some(Float(y))) => (*x as f64).partial_cmp(y).unwrap_or(Equal),
        (Some(Float(x)), Some(Int(y))) => x.partial_cmp(&(*y as f64)).unwrap_or(Equal),
        (Some(Date(x)), Some(Date(y))) => x.cmp(y),
        (Some(Time(x)), Some(Time(y))) => x.cmp(y),
        (Some(Timestamp(x)), Some(Timestamp(y))) => x.cmp(y),
        (Some(TimestampTz(x)), Some(TimestampTz(y))) => x.cmp(y),
        (Some(Text(x)), Some(Text(y))) => x.cmp(y),
        (Some(Json(x)), Some(Json(y))) => x.cmp(y),
        (Some(Bytes(x)), Some(Bytes(y))) => x.cmp(y),
        (Some(x), Some(y)) => x.to_string().cmp(&y.to_string()),
    }
}

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
            // Structured Postgres errors are multi-line (DETAIL/HINT/POSITION
            // on their own lines). Split explicitly so Paragraph doesn't join.
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

fn draw_explain(
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
        // PageUp/PageDown now step by visible_rows, which is set by draw.
        // Simulate a "screenful" large enough to overshoot the 3-row sample.
        s.visible_rows = 20;

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

    fn result_with_rows(n: usize) -> ResultSet {
        let rows: Vec<Vec<CellValue>> = (0..n).map(|i| vec![CellValue::Int(i as i64)]).collect();
        ResultSet {
            columns: vec![ColumnMeta {
                name: "a".into(),
                type_name: "int4".into(),
            }],
            rows,
            truncated_at: None,
            command_tag: Some(format!("{n} rows")),
            elapsed_ms: 1,
        }
    }

    #[test]
    fn page_down_uses_visible_rows_step() {
        let mut s = ResultsState::default();
        s.set_result(result_with_rows(100));
        s.visible_rows = 10;
        s.handle_key(key(KeyCode::PageDown));
        assert_eq!(s.selected_row, 10);
        s.handle_key(key(KeyCode::PageDown));
        assert_eq!(s.selected_row, 20);
    }

    #[test]
    fn page_up_clamps_to_zero() {
        let mut s = ResultsState::default();
        s.set_result(result_with_rows(100));
        s.visible_rows = 25;
        s.selected_row = 5;
        s.handle_key(key(KeyCode::PageUp));
        assert_eq!(s.selected_row, 0);
    }

    #[test]
    fn extract_explain_detects_query_plan_column() {
        let set = ResultSet {
            columns: vec![ColumnMeta {
                name: "QUERY PLAN".into(),
                type_name: "text".into(),
            }],
            rows: vec![
                vec![CellValue::Text(
                    "Seq Scan on t  (cost=0.00..1.00 rows=1)".into(),
                )],
                vec![CellValue::Text("Planning Time: 0.1 ms".into())],
            ],
            truncated_at: None,
            command_tag: Some("2 rows".into()),
            elapsed_ms: 1,
        };
        let mut s = ResultsState::default();
        s.set_result(set);
        let lines = s.explain_lines.as_ref().expect("explain detected");
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("Seq Scan"));
    }

    #[test]
    fn extract_explain_ignores_normal_results() {
        let mut s = ResultsState::default();
        s.set_result(sample_result());
        assert!(s.explain_lines.is_none());
    }

    #[test]
    fn parse_actual_total_ms_extracts_upper_bound() {
        assert_eq!(
            parse_actual_total_ms("  (actual time=0.013..0.014 rows=3 loops=1)"),
            Some(0.014)
        );
        assert_eq!(
            parse_actual_total_ms("  (actual time=10..123.4 rows=99)"),
            Some(123.4)
        );
        assert_eq!(parse_actual_total_ms("  (cost=0..1 rows=10)"), None);
    }

    #[test]
    fn sort_cycles_asc_desc_off() {
        let mut s = ResultsState::default();
        s.set_result(sample_result());
        // Three rows: ints 1, 2, 3 in column a.
        s.cycle_sort_on_current_column();
        let cur = s.current.as_ref().unwrap();
        assert!(matches!(cur.rows[0][0], CellValue::Int(1)));
        assert_eq!(s.sort.unwrap().dir, SortDir::Asc);
        s.cycle_sort_on_current_column();
        let cur = s.current.as_ref().unwrap();
        assert!(matches!(cur.rows[0][0], CellValue::Int(3)));
        assert_eq!(s.sort.unwrap().dir, SortDir::Desc);
        s.cycle_sort_on_current_column();
        let cur = s.current.as_ref().unwrap();
        // Off → restored to original (1, 2, 3).
        assert!(matches!(cur.rows[0][0], CellValue::Int(1)));
        assert!(s.sort.is_none());
    }

    #[test]
    fn sort_on_different_column_starts_asc() {
        let mut s = ResultsState::default();
        s.set_result(sample_result());
        s.cycle_sort_on_current_column();
        s.cycle_sort_on_current_column(); // Desc on col 0
        s.x_offset = 1;
        s.cycle_sort_on_current_column(); // Asc on col 1
        let st = s.sort.unwrap();
        assert_eq!(st.col, 1);
        assert_eq!(st.dir, SortDir::Asc);
    }

    #[test]
    fn ctrl_left_jumps_to_first_column() {
        use crossterm::event::KeyModifiers;
        let mut s = ResultsState::default();
        s.set_result(sample_result());
        s.x_offset = 1;
        s.handle_key(KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL));
        assert_eq!(s.x_offset, 0);
    }

    #[test]
    fn ctrl_right_jumps_to_last_column() {
        use crossterm::event::KeyModifiers;
        let mut s = ResultsState::default();
        s.set_result(sample_result());
        s.x_offset = 0;
        s.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::CONTROL));
        // sample_result has 2 columns; last index is 1.
        assert_eq!(s.x_offset, 1);
    }

    #[test]
    fn page_down_falls_back_to_single_step_when_visible_rows_unset() {
        let mut s = ResultsState::default();
        s.set_result(result_with_rows(5));
        // visible_rows defaults to 0 — step is clamped to 1.
        s.handle_key(key(KeyCode::PageDown));
        assert_eq!(s.selected_row, 1);
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
