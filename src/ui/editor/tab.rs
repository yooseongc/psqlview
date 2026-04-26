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
use std::time::{Duration, Instant};

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

/// Container that owns every editor buffer the user has open, plus
/// the `active` index pointer and the two-strike close-confirmation
/// state. `App` holds a single `Tabs` instead of independently
/// tracking `tabs` + `active_tab` + `pending_tab_close`.
pub struct Tabs {
    pub list: Vec<TabSlot>,
    pub active: usize,
    /// Set by the first `Ctrl+W` on a dirty tab; cleared when the
    /// confirmation succeeds, the user changes tab, or any other key
    /// is pressed (the reset lives in `App::on_key_workspace`). The
    /// instant is consulted by `try_close_active` to enforce a 3-second
    /// confirm window.
    pub pending_close: Option<(usize, Instant)>,
}

impl Default for Tabs {
    fn default() -> Self {
        Self::new()
    }
}

/// Result of [`Tabs::try_close_active`]. `Closed` means the tab was
/// removed (or a new empty tab was put in its place when it was the
/// last). `PendingDirty` means the buffer was dirty and the close was
/// armed for confirmation — the caller should toast the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseOutcome {
    Closed,
    PendingDirty,
}

impl Tabs {
    pub fn new() -> Self {
        Self {
            list: vec![TabSlot::new()],
            active: 0,
            pending_close: None,
        }
    }

    pub fn active(&self) -> &TabSlot {
        &self.list[self.active]
    }

    pub fn active_mut(&mut self) -> &mut TabSlot {
        &mut self.list[self.active]
    }

    pub fn mark_active_dirty(&mut self) {
        self.list[self.active].dirty = true;
    }

    pub fn open_new(&mut self) {
        self.pending_close = None;
        self.list.push(TabSlot::new());
        self.active = self.list.len() - 1;
    }

    /// Advances `active` by `delta` (wrapping). Returns the previous
    /// active index so the caller can decide whether per-tab modal
    /// state needs clearing.
    pub fn cycle(&mut self, delta: isize) -> usize {
        let prev = self.active;
        let n = self.list.len() as isize;
        if n <= 1 {
            return prev;
        }
        self.pending_close = None;
        self.active = ((self.active as isize + delta).rem_euclid(n)) as usize;
        prev
    }

    /// Sets `active = idx` if it's a valid index different from the
    /// current one. Returns the previous index (== `idx` when no-op).
    pub fn jump(&mut self, idx: usize) -> usize {
        let prev = self.active;
        if idx >= self.list.len() || idx == self.active {
            return prev;
        }
        self.pending_close = None;
        self.active = idx;
        prev
    }

    /// Tries to close the active tab. Clean buffers close immediately
    /// (`Closed`); dirty buffers arm `pending_close` and return
    /// `PendingDirty` so the caller can show a confirmation toast.
    /// A second call within 3 s while the same tab is still active
    /// closes it.
    pub fn try_close_active(&mut self) -> CloseOutcome {
        let idx = self.active;
        if !self.list[idx].dirty {
            self.do_close(idx);
            return CloseOutcome::Closed;
        }
        let now = Instant::now();
        if let Some((pending_idx, ts)) = self.pending_close {
            if pending_idx == idx && now.duration_since(ts) < Duration::from_secs(3) {
                self.pending_close = None;
                self.do_close(idx);
                return CloseOutcome::Closed;
            }
        }
        self.pending_close = Some((idx, now));
        CloseOutcome::PendingDirty
    }

    fn do_close(&mut self, idx: usize) {
        self.list.remove(idx);
        if self.list.is_empty() {
            self.list.push(TabSlot::new());
            self.active = 0;
        } else if self.active >= self.list.len() {
            self.active = self.list.len() - 1;
        }
        self.pending_close = None;
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

    #[test]
    fn tabs_starts_with_one_active_slot() {
        let t = Tabs::new();
        assert_eq!(t.list.len(), 1);
        assert_eq!(t.active, 0);
        assert!(t.pending_close.is_none());
    }

    #[test]
    fn open_new_appends_and_activates() {
        let mut t = Tabs::new();
        t.open_new();
        assert_eq!(t.list.len(), 2);
        assert_eq!(t.active, 1);
    }

    #[test]
    fn cycle_wraps_in_either_direction() {
        let mut t = Tabs::new();
        t.open_new();
        t.open_new();
        // active = 2, len = 3
        let prev = t.cycle(1);
        assert_eq!(prev, 2);
        assert_eq!(t.active, 0);
        let prev = t.cycle(-1);
        assert_eq!(prev, 0);
        assert_eq!(t.active, 2);
    }

    #[test]
    fn cycle_with_one_tab_is_noop() {
        let mut t = Tabs::new();
        let prev = t.cycle(1);
        assert_eq!(prev, 0);
        assert_eq!(t.active, 0);
    }

    #[test]
    fn jump_ignores_invalid_index() {
        let mut t = Tabs::new();
        t.open_new();
        let prev = t.jump(99);
        assert_eq!(prev, 1);
        assert_eq!(t.active, 1);
    }

    #[test]
    fn try_close_active_closes_clean_tab_immediately() {
        let mut t = Tabs::new();
        t.open_new();
        t.list[t.active].dirty = false;
        assert_eq!(t.try_close_active(), CloseOutcome::Closed);
        assert_eq!(t.list.len(), 1);
        assert!(t.pending_close.is_none());
    }

    #[test]
    fn try_close_active_arms_then_closes_dirty_tab() {
        let mut t = Tabs::new();
        t.open_new();
        t.list[t.active].dirty = true;
        assert_eq!(t.try_close_active(), CloseOutcome::PendingDirty);
        assert_eq!(t.list.len(), 2);
        assert!(t.pending_close.is_some());
        // Second strike closes.
        assert_eq!(t.try_close_active(), CloseOutcome::Closed);
        assert_eq!(t.list.len(), 1);
    }

    #[test]
    fn try_close_active_resets_pending_after_window() {
        let mut t = Tabs::new();
        t.open_new();
        t.list[t.active].dirty = true;
        t.try_close_active();
        // Backdate the pending instant past the 3-second window.
        let (idx, _) = t.pending_close.unwrap();
        t.pending_close = Some((idx, Instant::now() - Duration::from_secs(10)));
        // Second strike past window restarts the confirm flow.
        assert_eq!(t.try_close_active(), CloseOutcome::PendingDirty);
        assert_eq!(t.list.len(), 2);
    }

    #[test]
    fn closing_only_tab_replaces_with_empty() {
        let mut t = Tabs::new();
        t.list[0].editor.set_text("scratch");
        t.list[0].dirty = false;
        t.try_close_active();
        assert_eq!(t.list.len(), 1);
        assert_eq!(t.list[0].editor.text(), "");
        assert_eq!(t.active, 0);
    }
}
