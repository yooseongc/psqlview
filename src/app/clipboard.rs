use super::App;

/// Renders a cell for clipboard / TSV copy. Mirrors the Display impl
/// of `CellValue` except NULL becomes the empty string (so a row with
/// nulls round-trips through a paste cleanly).
pub(super) fn format_cell_for_copy(v: &crate::types::CellValue) -> String {
    match v {
        crate::types::CellValue::Null => String::new(),
        other => other.to_string(),
    }
}

/// Truncates `s` to `max` chars + "…" so toast messages don't grow
/// unboundedly when the user copies a long cell.
pub(super) fn truncate_for_toast(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

impl App {
    /// Copies the cell at (`selected_row`, leftmost-visible-column) into
    /// the host terminal's clipboard via OSC 52.
    pub(super) fn copy_current_cell_to_clipboard(&mut self) {
        let Some(text) = self.format_current_cell() else {
            self.toast_info("no cell to copy".into());
            return;
        };
        match crate::ui::clipboard::copy(&text) {
            Ok(()) => self.toast_info(format!("copied: {}", truncate_for_toast(&text, 40))),
            Err(e) => self.toast_error(format!("copy failed: {e}")),
        }
    }

    /// Copies the entire selected row as TSV (cells joined by `\t`).
    pub(super) fn copy_current_row_to_clipboard(&mut self) {
        let Some(text) = self.format_current_row_as_tsv() else {
            self.toast_info("no row to copy".into());
            return;
        };
        match crate::ui::clipboard::copy(&text) {
            Ok(()) => self.toast_info("row copied".into()),
            Err(e) => self.toast_error(format!("copy failed: {e}")),
        }
    }

    pub(super) fn format_current_cell(&self) -> Option<String> {
        let rs = self.results.current.as_ref()?;
        if rs.rows.is_empty() || rs.columns.is_empty() {
            return None;
        }
        let row = rs.rows.get(self.results.selected_row)?;
        let col = self.results.x_offset.min(row.len().saturating_sub(1));
        Some(format_cell_for_copy(row.get(col)?))
    }

    pub(super) fn format_current_row_as_tsv(&self) -> Option<String> {
        let rs = self.results.current.as_ref()?;
        if rs.rows.is_empty() {
            return None;
        }
        let row = rs.rows.get(self.results.selected_row)?;
        Some(
            row.iter()
                .map(format_cell_for_copy)
                .collect::<Vec<_>>()
                .join("\t"),
        )
    }
}
