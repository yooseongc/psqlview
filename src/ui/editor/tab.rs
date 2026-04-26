//! `TabSlot` — sidecar struct that owns an `EditorState` plus the UI
//! metadata (filesystem path, dirty flag, last-search needle) the tab
//! bar and Find/Replace overlay need.
//!
//! `EditorState` itself stays focused on text + cursor + undo so its
//! `Clone for undo` invariant doesn't have to clone a `PathBuf` per
//! keystroke. R1 introduces this struct without wiring `path` /
//! `dirty` / `last_search` — those land in R2 (file-prompt commits)
//! and R4 (find-state retention).

use std::path::PathBuf;

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::EditorState;

#[derive(Default)]
pub struct TabSlot {
    pub editor: EditorState,
    /// Absolute path of the file most recently `Open`ed or `Save`d into
    /// this tab, or `None` for an "untitled" buffer. Wired in R2.
    pub path: Option<PathBuf>,
    /// `true` when the buffer has unsaved changes since the last
    /// successful Open / Save. Wired in R2 — UI marks the tab title
    /// with a trailing `*` and gates the close-confirmation flow on it.
    pub dirty: bool,
    /// Needle from the most recent `Ctrl+F` session; retained after
    /// the overlay closes so `n` / `N` can repeat without retyping.
    /// Wired in R4.
    pub last_search: Option<String>,
}

impl TabSlot {
    pub fn new() -> Self {
        Self::default()
    }

    /// Display title for the tab bar. Returns the file's basename when
    /// the tab is bound to a path, else `"untitled"`. Symlinks /
    /// `..` are not resolved — it's purely a render hint.
    pub fn title(&self) -> String {
        self.path
            .as_ref()
            .and_then(|p| p.file_name())
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "untitled".to_string())
    }
}

/// One-row tab strip rendered above the editor's outer block. Each
/// tab shows `"<n>: <title>[*]"` — `n` is 1-based, the asterisk
/// marks an unsaved buffer. The active tab is bold-cyan; inactive
/// tabs are dimmed. Long titles get clipped by the Paragraph itself
/// (no marquee scroll).
pub fn draw(frame: &mut Frame<'_>, tabs: &[TabSlot], active: usize, area: Rect) {
    if area.height == 0 || tabs.is_empty() {
        return;
    }
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(tabs.len() * 2);
    for (i, tab) in tabs.iter().enumerate() {
        let mark = if tab.dirty { "*" } else { "" };
        let label = format!(" {}: {}{} ", i + 1, tab.title(), mark);
        let style = if i == active {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        spans.push(Span::styled(label, style));
        if i + 1 < tabs.len() {
            spans.push(Span::raw(" "));
        }
    }
    let paragraph = Paragraph::new(Line::from(spans));
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_tab_is_untitled_clean_and_searchless() {
        let t = TabSlot::new();
        assert!(t.path.is_none());
        assert!(!t.dirty);
        assert!(t.last_search.is_none());
        assert_eq!(t.title(), "untitled");
    }

    #[test]
    fn title_uses_path_basename() {
        let mut t = TabSlot::new();
        t.path = Some(PathBuf::from("/tmp/queries/active_users.sql"));
        assert_eq!(t.title(), "active_users.sql");
    }

    #[test]
    fn title_falls_back_to_untitled_for_path_with_no_filename() {
        let mut t = TabSlot::new();
        t.path = Some(PathBuf::from("/"));
        // PathBuf::file_name on "/" returns None; we fall back.
        assert_eq!(t.title(), "untitled");
    }
}
