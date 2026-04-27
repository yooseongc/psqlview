//! Directory listing dropdown shown above the file prompt and `:`
//! command line when the user is typing a path. Re-reads the parent
//! directory on every keystroke (no caching) so the listing always
//! reflects the live filesystem — fits the zero-footprint promise.
//!
//! Hidden files (those starting with `.`) are shown only when the
//! typed basename prefix also starts with `.`, matching vim's wildmenu
//! and most shells.

use std::path::{Path, PathBuf};

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};
use ratatui::Frame;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HintEntry {
    pub name: String,
    pub is_dir: bool,
}

#[derive(Debug, Default)]
pub struct DirHint {
    /// Parent chunk of the typed input (everything before the last
    /// `/` or `\`). Kept so `commit_selection` can rebuild the input
    /// with only the basename portion replaced.
    parent: String,
    /// Filtered + sorted entries.
    entries: Vec<HintEntry>,
    /// `Some(idx)` once the user has pressed Up/Down — drives both
    /// the highlight and Tab/Enter commit behavior.
    selected: Option<usize>,
}

impl DirHint {
    /// Lists `parent` of `input` (relative paths resolved against
    /// `cwd`) and filters entries by basename prefix. Resets the
    /// selection because the candidate set just changed.
    pub fn recompute(&mut self, input: &str, cwd: &Path) {
        let (parent_str, prefix) = split_parent_basename(input);
        self.parent = parent_str.to_string();
        self.selected = None;

        let parent_path = if parent_str.is_empty() {
            cwd.to_path_buf()
        } else {
            let p = PathBuf::from(parent_str);
            if p.is_absolute() {
                p
            } else {
                cwd.join(p)
            }
        };

        let Ok(read) = std::fs::read_dir(&parent_path) else {
            self.entries.clear();
            return;
        };

        let show_hidden = prefix.starts_with('.');
        let mut entries: Vec<HintEntry> = read
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                if !name.starts_with(prefix) {
                    return None;
                }
                if !show_hidden && name.starts_with('.') {
                    return None;
                }
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                Some(HintEntry { name, is_dir })
            })
            .collect();

        // Directories first, then alphabetical within each group.
        entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.cmp(&b.name)));
        self.entries = entries;
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn select_next(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        self.selected = Some(match self.selected {
            None => 0,
            Some(i) => (i + 1).min(self.entries.len() - 1),
        });
    }

    pub fn select_prev(&mut self) {
        if self.entries.is_empty() {
            return;
        }
        self.selected = Some(match self.selected {
            None => 0,
            Some(0) => 0,
            Some(i) => i - 1,
        });
    }

    pub fn selected_entry(&self) -> Option<&HintEntry> {
        self.entries.get(self.selected?)
    }

    pub fn entries(&self) -> &[HintEntry] {
        &self.entries
    }

    /// Returns the new input string with the basename portion replaced
    /// by the currently selected entry's name. Returns `None` when no
    /// selection is active. Directory entries get a trailing `/` so
    /// the next keystroke descends into them naturally.
    pub fn commit_selection(&self) -> Option<String> {
        let entry = self.selected_entry()?;
        let mut name = entry.name.clone();
        if entry.is_dir {
            name.push('/');
        }
        Some(if self.parent.is_empty() {
            name
        } else {
            format!("{}/{}", self.parent, name)
        })
    }
}

fn split_parent_basename(input: &str) -> (&str, &str) {
    match input.rfind(['/', '\\']) {
        Some(idx) => (&input[..idx], &input[idx + 1..]),
        None => ("", input),
    }
}

/// Draws the hint dropdown directly above a `prompt_rows`-tall prompt
/// pinned to the bottom of `editor_area`. Caller passes the editor's
/// full rect; this widget chooses its own slot inside it.
pub fn draw_above_prompt(
    frame: &mut Frame<'_>,
    hint: &DirHint,
    editor_area: Rect,
    prompt_rows: u16,
) {
    if hint.is_empty() {
        return;
    }
    const MAX_HINT_ROWS: u16 = 8;

    let available = editor_area.height.saturating_sub(prompt_rows);
    if available < 3 {
        return;
    }
    // +2 for borders.
    let rows = (hint.len() as u16 + 2).min(MAX_HINT_ROWS).min(available);

    let area = Rect {
        x: editor_area.x,
        y: editor_area.y + editor_area.height - prompt_rows - rows,
        width: editor_area.width,
        height: rows,
    };

    let items: Vec<ListItem> = hint
        .entries
        .iter()
        .map(|e| {
            let mut name = e.name.clone();
            if e.is_dir {
                name.push('/');
            }
            let style = if e.is_dir {
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };
            ListItem::new(Line::from(Span::styled(name, style)))
        })
        .collect();

    let title = format!(
        " {} entries  [\u{2191}/\u{2193} select \u{00b7} Tab commit] ",
        hint.len()
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Yellow));

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let mut state = ListState::default();
    state.select(hint.selected);

    frame.render_widget(Clear, area);
    frame.render_stateful_widget(list, area, &mut state);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_dir() -> PathBuf {
        let id = uuid::Uuid::new_v4();
        let p = std::env::temp_dir().join(format!("psqlview_dh_{id}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn recompute_returns_prefix_matches_only() {
        let dir = fresh_dir();
        std::fs::write(dir.join("alpha.sql"), "").unwrap();
        std::fs::write(dir.join("beta.sql"), "").unwrap();
        std::fs::write(dir.join("gamma.sql"), "").unwrap();
        let mut h = DirHint::default();
        h.recompute("a", &dir);
        assert_eq!(h.entries().len(), 1);
        assert_eq!(h.entries()[0].name, "alpha.sql");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recompute_sorts_directories_first_then_alphabetical() {
        let dir = fresh_dir();
        std::fs::write(dir.join("zebra.sql"), "").unwrap();
        std::fs::create_dir(dir.join("apple_dir")).unwrap();
        std::fs::write(dir.join("banana.sql"), "").unwrap();
        std::fs::create_dir(dir.join("zoo_dir")).unwrap();
        let mut h = DirHint::default();
        h.recompute("", &dir);
        let names: Vec<_> = h.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["apple_dir", "zoo_dir", "banana.sql", "zebra.sql"]
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recompute_hides_dotfiles_unless_prefix_starts_with_dot() {
        let dir = fresh_dir();
        std::fs::write(dir.join(".bashrc"), "").unwrap();
        std::fs::write(dir.join("script.sh"), "").unwrap();
        let mut h = DirHint::default();
        h.recompute("", &dir);
        let names: Vec<_> = h.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["script.sh"]);
        h.recompute(".", &dir);
        let names: Vec<_> = h.entries().iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec![".bashrc"]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn select_next_and_prev_clamp_at_bounds() {
        let dir = fresh_dir();
        std::fs::write(dir.join("a"), "").unwrap();
        std::fs::write(dir.join("b"), "").unwrap();
        let mut h = DirHint::default();
        h.recompute("", &dir);
        assert_eq!(h.selected, None);
        h.select_next();
        assert_eq!(h.selected, Some(0));
        h.select_next();
        assert_eq!(h.selected, Some(1));
        h.select_next();
        assert_eq!(h.selected, Some(1));
        h.select_prev();
        assert_eq!(h.selected, Some(0));
        h.select_prev();
        assert_eq!(h.selected, Some(0));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn commit_selection_replaces_basename_only() {
        let dir = fresh_dir();
        let sub = dir.join("scripts");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("init.sql"), "").unwrap();
        std::fs::write(sub.join("seed.sql"), "").unwrap();
        let mut h = DirHint::default();
        h.recompute("scripts/i", &dir);
        h.select_next();
        let committed = h.commit_selection().expect("selection");
        assert_eq!(committed, "scripts/init.sql");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn commit_selection_appends_slash_for_dirs() {
        let dir = fresh_dir();
        std::fs::create_dir(dir.join("subdir")).unwrap();
        let mut h = DirHint::default();
        h.recompute("sub", &dir);
        h.select_next();
        let committed = h.commit_selection().expect("selection");
        assert_eq!(committed, "subdir/");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn commit_selection_returns_none_without_active_selection() {
        let dir = fresh_dir();
        std::fs::write(dir.join("a"), "").unwrap();
        let mut h = DirHint::default();
        h.recompute("", &dir);
        assert!(h.commit_selection().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn recompute_empty_for_missing_directory() {
        let mut h = DirHint::default();
        let missing = std::env::temp_dir().join("psqlview_dh_missing_xyz_12345");
        h.recompute("foo", &missing);
        assert!(h.is_empty());
    }

    #[test]
    fn recompute_resets_selection() {
        let dir = fresh_dir();
        std::fs::write(dir.join("a"), "").unwrap();
        std::fs::write(dir.join("b"), "").unwrap();
        let mut h = DirHint::default();
        h.recompute("", &dir);
        h.select_next();
        assert_eq!(h.selected, Some(0));
        h.recompute("a", &dir);
        assert_eq!(h.selected, None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
