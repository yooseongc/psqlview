//! Inline filename prompt used by `Ctrl+O` (open) and `Ctrl+S` (save).
//!
//! Rendered as a single bordered row pinned to the bottom of the editor
//! pane while [`FilePromptState`] is active. While active the prompt is
//! modal at the application level — it absorbs Esc / Enter / printable
//! characters / Backspace and lets nothing else through.
//!
//! Path handling is intentionally minimal: the input is a single string,
//! resolved against the process's current working directory if relative.
//! No shell-style `~` expansion, no globbing, no file picker.

use std::path::{Path, PathBuf};

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilePromptMode {
    Open,
    Save,
}

impl FilePromptMode {
    fn label(&self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Save => "Save",
        }
    }
}

#[derive(Debug, Clone)]
pub struct FilePromptState {
    pub mode: FilePromptMode,
    pub input: String,
}

impl FilePromptState {
    pub fn new(mode: FilePromptMode) -> Self {
        Self {
            mode,
            input: String::new(),
        }
    }

    pub fn push_char(&mut self, c: char) {
        self.input.push(c);
    }

    pub fn pop_char(&mut self) {
        self.input.pop();
    }

    pub fn input(&self) -> &str {
        &self.input
    }
}

/// Resolves the user-typed `input` to an absolute path. Absolute inputs
/// are passed through; relative inputs are joined onto `cwd`. No shell
/// expansion or symlink resolution.
pub fn resolve(input: &str, cwd: &Path) -> PathBuf {
    let p = PathBuf::from(input);
    if p.is_absolute() {
        p
    } else {
        cwd.join(p)
    }
}

/// Renders the prompt at the bottom of the editor pane, replacing the
/// last 3 rows (so the editor still shows surrounding context). Caller
/// passes the editor's full area rect; this widget chooses its own
/// vertical slot inside it.
pub fn draw(frame: &mut Frame<'_>, state: &FilePromptState, editor_area: Rect) {
    if editor_area.height < 3 {
        return;
    }
    let h: u16 = 3; // top + content + bottom border
    let area = Rect {
        x: editor_area.x,
        y: editor_area.y + editor_area.height - h,
        width: editor_area.width,
        height: h,
    };

    let title = format!(" {}  [Enter \u{00b7} Esc cancel] ", state.mode.label());
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Yellow));

    // Caret indicator: a trailing block char so the user can see the
    // insertion point even though we don't move the terminal cursor.
    let line = Line::from(vec![
        Span::styled(
            state.input.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("\u{2588}", Style::default().fg(Color::Yellow)),
    ]);

    let paragraph = Paragraph::new(line).block(block);
    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_and_pop_chars() {
        let mut s = FilePromptState::new(FilePromptMode::Save);
        s.push_char('a');
        s.push_char('b');
        s.push_char('c');
        assert_eq!(s.input(), "abc");
        s.pop_char();
        assert_eq!(s.input(), "ab");
    }

    #[test]
    fn resolve_passes_absolute_paths_through() {
        let abs = if cfg!(windows) {
            PathBuf::from("C:\\tmp\\foo.sql")
        } else {
            PathBuf::from("/tmp/foo.sql")
        };
        let cwd = PathBuf::from("/anywhere");
        assert_eq!(resolve(abs.to_str().unwrap(), &cwd), abs);
    }

    #[test]
    fn resolve_joins_relative_paths_to_cwd() {
        let cwd = PathBuf::from(if cfg!(windows) { "C:\\work" } else { "/work" });
        let resolved = resolve("queries/a.sql", &cwd);
        assert_eq!(resolved, cwd.join("queries/a.sql"));
    }

    #[test]
    fn mode_label_is_human_readable() {
        assert_eq!(FilePromptMode::Open.label(), "Open");
        assert_eq!(FilePromptMode::Save.label(), "Save");
    }
}
