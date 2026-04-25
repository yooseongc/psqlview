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
    ExportCsv,
}

impl FilePromptMode {
    fn label(&self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Save => "Save",
            Self::ExportCsv => "Export CSV",
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

/// Best-effort path completion for the file prompt. Splits `input` on
/// the last `/` (or `\\` on Windows) into a parent and a basename
/// prefix, lists the parent directory, and replaces the basename with
/// the longest common prefix shared by all matching entries. If
/// exactly one entry matches and it's a directory, a trailing
/// separator is appended so the next Tab descends into it.
///
/// Returns `None` when the directory can't be read or no entry matches
/// — the caller leaves the input unchanged in that case.
pub fn path_complete(input: &str, cwd: &Path) -> Option<String> {
    let (parent_str, prefix) = split_parent_basename(input);
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

    let entries = std::fs::read_dir(&parent_path).ok()?;
    let mut matches: Vec<(String, bool)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with(prefix) {
                let is_dir = e.file_type().map(|t| t.is_dir()).unwrap_or(false);
                Some((name, is_dir))
            } else {
                None
            }
        })
        .collect();
    if matches.is_empty() {
        return None;
    }
    matches.sort_by(|a, b| a.0.cmp(&b.0));

    let names: Vec<&str> = matches.iter().map(|(n, _)| n.as_str()).collect();
    let lcp = longest_common_prefix(&names);
    let new_basename = if lcp.len() > prefix.len() {
        lcp.to_string()
    } else {
        // No additional characters to commit — return as-is so the
        // user's input doesn't get mangled. Caller treats this as a
        // no-op completion.
        return None;
    };

    let mut out = if parent_str.is_empty() {
        new_basename
    } else {
        format!("{parent_str}/{new_basename}")
    };
    if matches.len() == 1 && matches[0].1 {
        out.push('/');
    }
    Some(out)
}

fn trim_path_for_title(path: &str, max: usize) -> String {
    let count = path.chars().count();
    if count <= max {
        return path.to_string();
    }
    let skip = count - max + 1;
    let tail: String = path.chars().skip(skip).collect();
    format!("\u{2026}{tail}")
}

fn split_parent_basename(input: &str) -> (&str, &str) {
    match input.rfind(['/', '\\']) {
        Some(idx) => (&input[..idx], &input[idx + 1..]),
        None => ("", input),
    }
}

fn longest_common_prefix<'a>(strs: &[&'a str]) -> &'a str {
    if strs.is_empty() {
        return "";
    }
    let first = strs[0];
    let mut end = first.len();
    for s in &strs[1..] {
        end = end.min(s.len());
        let f = first.as_bytes();
        let sb = s.as_bytes();
        let mut i = 0;
        while i < end && f[i] == sb[i] {
            i += 1;
        }
        end = i;
    }
    // Don't slice in the middle of a multi-byte UTF-8 sequence.
    while end > 0 && !first.is_char_boundary(end) {
        end -= 1;
    }
    &first[..end]
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

    // Show the cwd in the prompt title so the user knows what relative
    // paths resolve to. Truncated to the trailing 50 chars to avoid
    // overflowing the editor border on deep workdirs.
    let cwd_hint = std::env::current_dir()
        .map(|p| trim_path_for_title(&p.display().to_string(), 50))
        .unwrap_or_else(|_| "?".into());
    let title = format!(
        " {}  [Tab complete \u{00b7} Enter \u{00b7} Esc cancel]  cwd: {} ",
        state.mode.label(),
        cwd_hint
    );
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

    fn fresh_dir() -> PathBuf {
        let id = uuid::Uuid::new_v4();
        let p = std::env::temp_dir().join(format!("psqlview_pc_{id}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn path_complete_extends_unique_prefix_to_full_name() {
        let dir = fresh_dir();
        std::fs::write(dir.join("alpha.sql"), "").unwrap();
        std::fs::write(dir.join("beta.sql"), "").unwrap();
        let out = path_complete("al", &dir).expect("completion");
        assert_eq!(out, "alpha.sql");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn path_complete_extends_to_longest_common_prefix() {
        let dir = fresh_dir();
        std::fs::write(dir.join("queries_a.sql"), "").unwrap();
        std::fs::write(dir.join("queries_b.sql"), "").unwrap();
        let out = path_complete("q", &dir).expect("completion");
        assert_eq!(out, "queries_");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn path_complete_appends_separator_for_unique_directory_match() {
        let dir = fresh_dir();
        std::fs::create_dir(dir.join("subdir")).unwrap();
        let out = path_complete("sub", &dir).expect("completion");
        assert_eq!(out, "subdir/");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn path_complete_handles_subdirectory_input() {
        let dir = fresh_dir();
        let sub = dir.join("scripts");
        std::fs::create_dir(&sub).unwrap();
        std::fs::write(sub.join("init.sql"), "").unwrap();
        let out = path_complete("scripts/in", &dir).expect("completion");
        assert_eq!(out, "scripts/init.sql");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn path_complete_returns_none_when_no_extension_possible() {
        let dir = fresh_dir();
        std::fs::write(dir.join("alpha.sql"), "").unwrap();
        // The user already typed the full name — nothing more to commit.
        let out = path_complete("alpha.sql", &dir);
        assert!(out.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn path_complete_returns_none_when_no_match() {
        let dir = fresh_dir();
        std::fs::write(dir.join("alpha.sql"), "").unwrap();
        let out = path_complete("xyz", &dir);
        assert!(out.is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn longest_common_prefix_works_for_short_lists() {
        assert_eq!(longest_common_prefix(&["foo", "fool", "foobar"]), "foo");
        assert_eq!(longest_common_prefix(&["abc", "xyz"]), "");
        assert_eq!(longest_common_prefix(&["only"]), "only");
        assert_eq!(longest_common_prefix(&[]), "");
    }

    #[test]
    fn trim_path_for_title_keeps_short_paths() {
        assert_eq!(trim_path_for_title("/short", 50), "/short");
    }

    #[test]
    fn trim_path_for_title_truncates_with_ellipsis_marker() {
        let long = "/".repeat(60);
        let out = trim_path_for_title(&long, 20);
        assert!(out.starts_with('\u{2026}'));
        assert_eq!(out.chars().count(), 20);
    }
}
