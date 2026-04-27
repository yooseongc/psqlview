//! Key handlers for the cell-edit + UPDATE-confirm modal pair, plus
//! the `e` shortcut on the Results pane that opens cell-edit.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::App;
use crate::app::PendingCellPatch;
use crate::ui::cell_edit::CellEditState;
use crate::ui::confirm_update::ConfirmUpdateState;
use crate::ui::sql_format;

impl App {
    /// Eligibility-checks the current Results-pane cell and opens the
    /// cell-edit modal if everything passes. Toasts the specific
    /// failure reason otherwise so the user understands why `e` did
    /// nothing.
    pub(super) fn try_open_cell_edit(&mut self) {
        let Some(rs) = self.results.current.as_ref() else {
            return;
        };
        let Some(_source) = rs.source.as_ref() else {
            self.toast_error("cell edit only works on tree-preview results".into());
            return;
        };
        if rs.pk_columns.len() != 1 {
            if rs.pk_columns.is_empty() {
                self.toast_error("table has no primary key".into());
            } else {
                self.toast_error("composite primary key not supported".into());
            }
            return;
        }
        let pk_name = &rs.pk_columns[0];
        let row_idx = self.results.selected_row;
        let col_idx = self.results.x_offset;
        let Some(row) = rs.rows.get(row_idx) else {
            return;
        };
        let Some(col_meta) = rs.columns.get(col_idx) else {
            return;
        };
        if &col_meta.name == pk_name {
            self.toast_error("cannot edit primary key column".into());
            return;
        }
        let Some(cell) = row.get(col_idx) else {
            return;
        };
        if !sql_format::is_editable(cell) {
            self.toast_error("cell type not editable".into());
            return;
        }
        // Look up PK position in this row to confirm we have a value
        // for the WHERE clause. Required because the user could be
        // looking at a query that omitted the PK column from the
        // SELECT — but tree-preview always issues `SELECT *` so this
        // is mostly defensive.
        let pk_idx = rs.columns.iter().position(|c| &c.name == pk_name);
        if pk_idx.is_none() {
            self.toast_error("PK column missing from this result".into());
            return;
        }
        self.cell_edit = Some(CellEditState::new(
            row_idx,
            col_idx,
            col_meta.name.clone(),
            cell.clone(),
        ));
    }

    /// Routes printable / Backspace / Enter / Esc into the cell-edit
    /// modal. Enter parses the input and (on success) opens the
    /// UPDATE confirm modal. Parse failures stay in cell-edit with a
    /// toast.
    pub(super) fn handle_cell_edit_key(&mut self, key: KeyEvent) {
        let Some(state) = self.cell_edit.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.cell_edit = None;
            }
            KeyCode::Enter => self.commit_cell_edit_to_confirm(),
            KeyCode::Backspace => {
                state.pop_char();
            }
            // Ctrl+U clears the input — quick way to set NULL.
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                state.clear_input();
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                state.push_char(c);
            }
            _ => {}
        }
    }

    /// Parses the cell-edit input, builds the UPDATE SQL, and opens
    /// the confirm modal. Stays in cell-edit on parse failure.
    fn commit_cell_edit_to_confirm(&mut self) {
        let Some(edit) = self.cell_edit.as_ref() else {
            return;
        };
        let new_value = match sql_format::parse_cell_input(&edit.original, &edit.input) {
            Ok(v) => v,
            Err(msg) => {
                self.toast_error(msg);
                return;
            }
        };
        // Rebuild the UPDATE from result-set + cursor coords. We
        // re-resolve `source`, PK column, and PK row value here rather
        // than caching them on CellEditState — the result set hasn't
        // changed since the modal opened, so this is just the
        // simplest data flow.
        let Some(rs) = self.results.current.as_ref() else {
            self.cell_edit = None;
            return;
        };
        let Some(source) = rs.source.as_ref() else {
            self.cell_edit = None;
            return;
        };
        let Some(pk_name) = rs.pk_columns.first().cloned() else {
            self.cell_edit = None;
            return;
        };
        let Some(pk_idx) = rs.columns.iter().position(|c| c.name == pk_name) else {
            self.cell_edit = None;
            return;
        };
        let Some(row) = rs.rows.get(edit.row) else {
            self.cell_edit = None;
            return;
        };
        let Some(pk_val) = row.get(pk_idx).cloned() else {
            self.cell_edit = None;
            return;
        };
        let target = format!("{}.{}", source.schema, source.name);
        let sql =
            sql_format::format_update_one(&target, &pk_name, &pk_val, &edit.col_name, &new_value);
        let row_idx = edit.row;
        let col_idx = edit.col;
        self.cell_edit = None;
        self.confirm_update = Some(ConfirmUpdateState {
            sql,
            row: row_idx,
            col: col_idx,
            new_value,
        });
    }

    /// Routes y/n/Esc into the UPDATE confirm modal. On `y` the SQL
    /// dispatches via the existing query path and the cell is patched
    /// in-place once we know the server accepted it.
    pub(super) fn handle_confirm_update_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('n') | KeyCode::Char('N') => {
                self.confirm_update = None;
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.execute_confirm_update();
            }
            _ => {}
        }
    }

    /// Dispatches the confirmed UPDATE SQL and stages the in-place
    /// cell patch so `on_query_result` can apply it once the server
    /// confirms success.
    fn execute_confirm_update(&mut self) {
        let Some(state) = self.confirm_update.take() else {
            return;
        };
        // Stash the patch so on_query_result can apply it post-success.
        self.pending_cell_patch = Some(PendingCellPatch {
            row: state.row,
            col: state.col,
            new_value: state.new_value,
        });
        self.dispatch_sql(state.sql);
    }
}
