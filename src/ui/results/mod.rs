//! Results pane — state, sort cycle, key handling. Rendering and
//! EXPLAIN-specific layout live in sibling modules.

mod explain;
mod render;

#[cfg(test)]
mod tests;

pub use render::{compute_widths, draw};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::types::{CellValue, ResultSet};

#[derive(Default)]
pub struct ResultsState {
    pub current: Option<ResultSet>,
    pub selected_row: usize,
    pub x_offset: usize,
    /// Last rendered visible data-row count. Updated each frame so
    /// PageUp/PageDown can step by a screenful instead of a fixed 20.
    pub visible_rows: usize,
    /// Active client-side sort, if any. Pressing `s` cycles through
    /// Asc → Desc → off for the leftmost visible column.
    pub sort: Option<SortState>,
    /// Snapshot of `current.rows` taken before the first sort, used
    /// to restore original order when the sort cycles back to off.
    original_rows: Option<Vec<Vec<CellValue>>>,
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
        self.explain_lines = explain::extract_explain_lines(&result);
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

    /// Cycles the sort on the leftmost visible column: off → Asc →
    /// Desc → off. Sorting is client-side; the underlying ResultSet's
    /// row vector is mutated, with the pre-sort order remembered so
    /// the "off" state can restore it.
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
        // Ctrl+Left / Ctrl+Right jump to the first / last column.
        // Kept out of the main match so the non-Ctrl arrows keep
        // their single-column-scroll behavior.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
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
