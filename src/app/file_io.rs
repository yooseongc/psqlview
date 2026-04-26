use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::App;
use crate::ui::file_prompt::{self, FilePromptMode, FilePromptState};
use crate::ui::{csv_export, json_export, sql_export};

impl App {
    /// Opens the inline filename prompt for the given mode. Closes any
    /// active autocomplete popup so the next keystroke is unambiguously
    /// routed to the prompt.
    pub(super) fn open_file_prompt(&mut self, mode: FilePromptMode) {
        self.autocomplete = None;
        self.file_prompt = Some(FilePromptState::new(mode));
    }

    /// Routes a keystroke to the file-prompt modal. Only Enter / Esc /
    /// printable characters / Backspace are meaningful; everything else
    /// is silently swallowed so global shortcuts like F-keys don't
    /// dismiss the prompt by accident.
    pub(super) fn handle_file_prompt_key(&mut self, key: KeyEvent) {
        let Some(state) = self.file_prompt.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.file_prompt = None;
            }
            KeyCode::Enter => {
                self.commit_file_prompt();
            }
            KeyCode::Backspace => {
                state.pop_char();
            }
            KeyCode::Tab => {
                // Best-effort path completion against the cwd. Quietly
                // no-ops when the parent directory can't be read or no
                // entry matches the typed prefix.
                let cwd = std::env::current_dir().unwrap_or_default();
                if let Some(completed) = file_prompt::path_complete(&state.input, &cwd) {
                    state.input = completed;
                }
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                state.push_char(c);
            }
            _ => {}
        }
    }

    /// Reads or writes the file the prompt names, then closes the prompt.
    /// Errors surface as toasts; the editor buffer is unchanged on Save
    /// failure and on Open failure (so a bad path doesn't blow away
    /// in-progress work).
    fn commit_file_prompt(&mut self) {
        let Some(state) = self.file_prompt.take() else {
            return;
        };
        let trimmed = state.input.trim();
        if trimmed.is_empty() {
            self.toast_error("file path is empty".into());
            return;
        }
        let cwd = std::env::current_dir().unwrap_or_default();
        let path = file_prompt::resolve(trimmed, &cwd);
        match state.mode {
            FilePromptMode::Open => self.commit_open(&path),
            FilePromptMode::Save => self.commit_save(&path),
            FilePromptMode::ExportCsv => self.commit_export(&path),
        }
    }

    pub(super) fn commit_open(&mut self, path: &std::path::Path) {
        match std::fs::read_to_string(path) {
            Ok(text) => {
                // CRLF normalized so Windows line endings don't render
                // as blank lines.
                let normalized = text.replace("\r\n", "\n");
                self.editor_mut().set_text(&normalized);
                let active = self.tabs.active_mut();
                active.path = Some(path.to_path_buf());
                active.dirty = false;
                self.toast_info(format!("opened: {}", path.display()));
            }
            Err(e) => {
                self.toast_error(format!("open failed: {e}"));
            }
        }
    }

    pub(super) fn commit_save(&mut self, path: &std::path::Path) {
        match std::fs::write(path, self.editor().text()) {
            Ok(()) => {
                let active = self.tabs.active_mut();
                active.path = Some(path.to_path_buf());
                active.dirty = false;
                self.toast_info(format!("saved: {}", path.display()));
            }
            Err(e) => {
                self.toast_error(format!("save failed: {e}"));
            }
        }
    }

    fn commit_export(&mut self, path: &std::path::Path) {
        let Some(rs) = self.results.current.as_ref() else {
            self.toast_error("no result set to export".into());
            return;
        };
        // Format follows the file extension. CSV is the default for
        // anything we don't recognize. SQL targets the `file_stem`
        // (e.g. `public.users.sql` → `INSERT INTO public.users …`).
        let format = ExportFormat::from_path(path);
        let res = std::fs::File::create(path).and_then(|mut f| match &format {
            ExportFormat::Csv => csv_export::write_csv(rs, &mut f),
            ExportFormat::JsonLines => json_export::write_json_lines(rs, &mut f),
            ExportFormat::JsonPretty => json_export::write_json_pretty(rs, &mut f),
            ExportFormat::SqlInsert { target } => sql_export::write_inserts(rs, target, &mut f),
        });
        match res {
            Ok(()) => self.toast_info(format!(
                "exported {} rows to {} ({})",
                rs.rows.len(),
                path.display(),
                format.label(),
            )),
            Err(e) => self.toast_error(format!("export failed: {e}")),
        }
    }
}

/// Output format chosen from the export-path's extension.
#[derive(Debug)]
enum ExportFormat {
    Csv,
    JsonLines,
    JsonPretty,
    SqlInsert { target: String },
}

impl ExportFormat {
    fn from_path(path: &std::path::Path) -> Self {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_ascii_lowercase);
        match ext.as_deref() {
            Some("jsonl") | Some("ndjson") => Self::JsonLines,
            Some("json") => Self::JsonPretty,
            Some("sql") => {
                let target = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .filter(|s| !s.is_empty())
                    .unwrap_or("exported_rows")
                    .to_string();
                Self::SqlInsert { target }
            }
            _ => Self::Csv,
        }
    }

    fn label(&self) -> String {
        match self {
            Self::Csv => "csv".into(),
            Self::JsonLines => "jsonl".into(),
            Self::JsonPretty => "json".into(),
            Self::SqlInsert { target } => format!("sql → {target}"),
        }
    }
}
