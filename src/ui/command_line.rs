//! Vim-style ex command line. Single-line input pinned to the bottom
//! of the editor area; absorbs every keystroke until Enter / Esc.
//!
//! Parsing is split out from execution so the App side can dispatch
//! into existing primitives (`EditorState::goto_line`,
//! `EditorState::replace_all`, file-prompt machinery, tab management,
//! quit). Unsupported subset is intentional — vim's full ex-command
//! grammar is way out of scope for an SQL editor.

use std::path::Path;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use super::path_hint::{self, DirHint};

#[derive(Debug, Default)]
pub struct CommandLineState {
    pub input: String,
    /// Directory hint dropdown shown when the input is in
    /// `e <path>` / `w <path>` form. Empty otherwise.
    pub path_hint: DirHint,
}

impl CommandLineState {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_char(&mut self, c: char) {
        self.input.push(c);
    }

    pub fn pop_char(&mut self) {
        self.input.pop();
    }

    /// Re-populates the path hint when the input is currently typing
    /// the argument to `:e` or `:w`. Clears the hint otherwise so
    /// non-path commands like `:s/...` don't get a stale dropdown.
    pub fn refresh_hint(&mut self, cwd: &Path) {
        match parse_path_arg(&self.input) {
            Some(arg) => self.path_hint.recompute(arg, cwd),
            None => self.path_hint = DirHint::default(),
        }
    }

    /// If the dropdown has an active selection, replaces the path
    /// argument portion of the input with the selected entry. Returns
    /// whether a commit happened so callers can chain a re-render or
    /// skip a follow-up Tab fallback.
    pub fn commit_hint_if_active(&mut self) -> bool {
        let Some(committed) = self.path_hint.commit_selection() else {
            return false;
        };
        let Some(start) = path_arg_start(&self.input) else {
            return false;
        };
        self.input.truncate(start);
        self.input.push_str(&committed);
        true
    }
}

/// Returns the byte index in `input` where the path argument starts,
/// or `None` if the input isn't an `e <path>` / `w <path>` form.
/// Stable across leading whitespace so `"  e foo"` resolves correctly.
fn path_arg_start(input: &str) -> Option<usize> {
    let trimmed = input.trim_start();
    let leading = input.len() - trimmed.len();
    for prefix in ["e ", "w "] {
        if trimmed.starts_with(prefix) {
            return Some(leading + prefix.len());
        }
    }
    None
}

/// Extracts the path argument from `:e <path>` / `:w <path>`. Returns
/// `None` for any other command (or for `:e` / `:w` with no space yet).
fn parse_path_arg(input: &str) -> Option<&str> {
    let start = path_arg_start(input)?;
    Some(&input[start..])
}

/// Outcome of routing a key into the command line.
#[derive(Debug, PartialEq, Eq)]
pub enum CommandLineOutcome {
    /// Stay open with current state.
    Stay,
    /// User pressed Esc — close without executing.
    Cancel,
    /// User pressed Enter — caller should `parse(state.input)` and
    /// dispatch the command, then close the overlay.
    Submit,
}

pub fn handle_key(state: &mut CommandLineState, key: KeyEvent, cwd: &Path) -> CommandLineOutcome {
    match key.code {
        KeyCode::Esc => CommandLineOutcome::Cancel,
        KeyCode::Enter => {
            // Commit any highlighted hint into the input first so
            // submit lines up with what the user sees in the dropdown.
            state.commit_hint_if_active();
            CommandLineOutcome::Submit
        }
        KeyCode::Backspace => {
            state.pop_char();
            state.refresh_hint(cwd);
            CommandLineOutcome::Stay
        }
        KeyCode::Up => {
            state.path_hint.select_prev();
            CommandLineOutcome::Stay
        }
        KeyCode::Down => {
            state.path_hint.select_next();
            CommandLineOutcome::Stay
        }
        KeyCode::Tab => {
            if state.commit_hint_if_active() {
                state.refresh_hint(cwd);
            }
            CommandLineOutcome::Stay
        }
        KeyCode::Char(c) if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
            state.push_char(c);
            state.refresh_hint(cwd);
            CommandLineOutcome::Stay
        }
        _ => CommandLineOutcome::Stay,
    }
}

/// Parsed command tree. Variants map 1:1 onto an App-side dispatcher.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// Plain digits — `:42` jumps to line 42.
    GotoLine(usize),
    /// `:s/PAT/REPL/[g]` (current line) or `:%s/...` (whole buffer).
    Substitute {
        all_lines: bool,
        pattern: String,
        replacement: String,
        global: bool,
    },
    /// `:w` (no path → save in place if known, else open Save prompt)
    /// or `:w foo.sql` (save to argument).
    Write {
        path: Option<String>,
    },
    /// `:e foo.sql` — open file at path.
    Edit {
        path: String,
    },
    TabNew,
    TabNext,
    TabPrev,
    TabClose,
    Quit,
    Help,
}

/// Parses a single ex command (the text after `:`). Returns a
/// human-readable error string when the input doesn't match any
/// supported form — caller surfaces it as a toast.
pub fn parse(input: &str) -> Result<Command, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("empty command".into());
    }

    if let Ok(n) = trimmed.parse::<usize>() {
        return Ok(Command::GotoLine(n));
    }

    if let Some(rest) = trimmed.strip_prefix("%s/") {
        let (pattern, replacement, flags) = parse_subst_body(rest)?;
        return Ok(Command::Substitute {
            all_lines: true,
            pattern,
            replacement,
            global: flags.contains('g'),
        });
    }
    if let Some(rest) = trimmed.strip_prefix("s/") {
        let (pattern, replacement, flags) = parse_subst_body(rest)?;
        return Ok(Command::Substitute {
            all_lines: false,
            pattern,
            replacement,
            global: flags.contains('g'),
        });
    }

    match trimmed {
        "tabnew" => return Ok(Command::TabNew),
        "tabnext" | "tabn" => return Ok(Command::TabNext),
        "tabprev" | "tabp" => return Ok(Command::TabPrev),
        "tabclose" | "tabc" => return Ok(Command::TabClose),
        "q" | "q!" | "qa" | "qa!" => return Ok(Command::Quit),
        "help" | "h" => return Ok(Command::Help),
        "w" => return Ok(Command::Write { path: None }),
        _ => {}
    }

    if let Some(rest) = trimmed.strip_prefix("w ") {
        let path = rest.trim();
        if path.is_empty() {
            return Ok(Command::Write { path: None });
        }
        return Ok(Command::Write {
            path: Some(path.to_string()),
        });
    }
    if let Some(rest) = trimmed.strip_prefix("e ") {
        let path = rest.trim();
        if path.is_empty() {
            return Err("usage: e <path>".into());
        }
        return Ok(Command::Edit {
            path: path.to_string(),
        });
    }

    Err(format!("unknown command: {trimmed}"))
}

fn parse_subst_body(body: &str) -> Result<(String, String, String), String> {
    let mut parts = body.splitn(3, '/');
    let pattern = parts.next().unwrap_or("").to_string();
    let replacement = match parts.next() {
        Some(r) => r.to_string(),
        None => return Err("missing replacement (form: s/PAT/REPL[/FLAGS])".into()),
    };
    let flags = parts.next().unwrap_or("").to_string();
    if pattern.is_empty() {
        return Err("empty pattern".into());
    }
    Ok((pattern, replacement, flags))
}

pub fn draw(frame: &mut Frame<'_>, state: &CommandLineState, editor_area: Rect) {
    if editor_area.height < 3 {
        return;
    }
    let area = Rect {
        x: editor_area.x,
        y: editor_area.y + editor_area.height - 3,
        width: editor_area.width,
        height: 3,
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Command  [Enter run \u{00b7} Esc cancel] ")
        .border_style(Style::default().fg(Color::Yellow));
    let line = Line::from(vec![
        Span::styled(
            ":",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            state.input.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("\u{2588}", Style::default().fg(Color::Yellow)),
    ]);
    let p = Paragraph::new(line).block(block);
    frame.render_widget(Clear, area);
    frame.render_widget(p, area);

    // Path-arg hint dropdown sits directly above the command line —
    // shows up only when the user is typing the argument to `:e`/`:w`.
    path_hint::draw_above_prompt(frame, &state.path_hint, editor_area, 3);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_digit_goes_to_line() {
        assert_eq!(parse("42").unwrap(), Command::GotoLine(42));
        assert_eq!(parse("  7  ").unwrap(), Command::GotoLine(7));
    }

    #[test]
    fn parse_subst_current_line_no_global() {
        let cmd = parse("s/foo/bar/").unwrap();
        assert_eq!(
            cmd,
            Command::Substitute {
                all_lines: false,
                pattern: "foo".into(),
                replacement: "bar".into(),
                global: false,
            }
        );
    }

    #[test]
    fn parse_subst_current_line_with_global_flag() {
        let cmd = parse("s/foo/bar/g").unwrap();
        match cmd {
            Command::Substitute {
                all_lines,
                global,
                pattern,
                replacement,
            } => {
                assert!(!all_lines);
                assert!(global);
                assert_eq!(pattern, "foo");
                assert_eq!(replacement, "bar");
            }
            _ => panic!("expected Substitute"),
        }
    }

    #[test]
    fn parse_subst_all_lines_with_global_flag() {
        let cmd = parse("%s/foo/bar/g").unwrap();
        match cmd {
            Command::Substitute {
                all_lines, global, ..
            } => {
                assert!(all_lines);
                assert!(global);
            }
            _ => panic!("expected Substitute"),
        }
    }

    #[test]
    fn parse_subst_with_empty_replacement_deletes() {
        let cmd = parse("s/foo//").unwrap();
        match cmd {
            Command::Substitute { replacement, .. } => assert_eq!(replacement, ""),
            _ => panic!(),
        }
    }

    #[test]
    fn parse_subst_missing_replacement_errors() {
        assert!(parse("s/foo").is_err());
    }

    #[test]
    fn parse_w_without_path() {
        assert_eq!(parse("w").unwrap(), Command::Write { path: None });
    }

    #[test]
    fn parse_w_with_path() {
        assert_eq!(
            parse("w foo.sql").unwrap(),
            Command::Write {
                path: Some("foo.sql".into())
            }
        );
    }

    #[test]
    fn parse_e_with_path() {
        assert_eq!(
            parse("e bar.sql").unwrap(),
            Command::Edit {
                path: "bar.sql".into()
            }
        );
    }

    #[test]
    fn parse_e_without_path_errors() {
        assert!(parse("e").is_err());
    }

    #[test]
    fn parse_tab_aliases() {
        assert_eq!(parse("tabnew").unwrap(), Command::TabNew);
        assert_eq!(parse("tabn").unwrap(), Command::TabNext);
        assert_eq!(parse("tabnext").unwrap(), Command::TabNext);
        assert_eq!(parse("tabp").unwrap(), Command::TabPrev);
        assert_eq!(parse("tabprev").unwrap(), Command::TabPrev);
        assert_eq!(parse("tabc").unwrap(), Command::TabClose);
        assert_eq!(parse("tabclose").unwrap(), Command::TabClose);
    }

    #[test]
    fn parse_quit_aliases() {
        for s in ["q", "q!", "qa", "qa!"] {
            assert_eq!(parse(s).unwrap(), Command::Quit, "input was {s}");
        }
    }

    #[test]
    fn parse_help_aliases() {
        assert_eq!(parse("help").unwrap(), Command::Help);
        assert_eq!(parse("h").unwrap(), Command::Help);
    }

    #[test]
    fn parse_empty_returns_error() {
        assert!(parse("").is_err());
        assert!(parse("   ").is_err());
    }

    #[test]
    fn parse_unknown_returns_error_with_input() {
        let err = parse("foozle").unwrap_err();
        assert!(err.contains("foozle"));
    }

    fn dummy_cwd() -> std::path::PathBuf {
        std::env::temp_dir()
    }

    #[test]
    fn handle_key_appends_chars_into_input() {
        let mut s = CommandLineState::new();
        let out = handle_key(
            &mut s,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
            &dummy_cwd(),
        );
        assert_eq!(out, CommandLineOutcome::Stay);
        assert_eq!(s.input, "a");
    }

    #[test]
    fn handle_key_backspace_pops() {
        let mut s = CommandLineState::new();
        s.input = "abc".into();
        handle_key(
            &mut s,
            KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE),
            &dummy_cwd(),
        );
        assert_eq!(s.input, "ab");
    }

    #[test]
    fn handle_key_esc_cancels() {
        let mut s = CommandLineState::new();
        let out = handle_key(
            &mut s,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
            &dummy_cwd(),
        );
        assert_eq!(out, CommandLineOutcome::Cancel);
    }

    #[test]
    fn handle_key_enter_submits() {
        let mut s = CommandLineState::new();
        s.input = "42".into();
        let out = handle_key(
            &mut s,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
            &dummy_cwd(),
        );
        assert_eq!(out, CommandLineOutcome::Submit);
    }

    fn fresh_dir() -> std::path::PathBuf {
        let id = uuid::Uuid::new_v4();
        let p = std::env::temp_dir().join(format!("psqlview_cl_{id}"));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn parse_path_arg_extracts_e_and_w_arguments() {
        assert_eq!(parse_path_arg("e foo.sql"), Some("foo.sql"));
        assert_eq!(parse_path_arg("w foo.sql"), Some("foo.sql"));
        // Leading whitespace tolerated.
        assert_eq!(parse_path_arg("  e foo"), Some("foo"));
        // Empty arg right after the space — hints become cwd contents.
        assert_eq!(parse_path_arg("e "), Some(""));
        // Non-path commands return None so dropdown stays empty.
        assert_eq!(parse_path_arg("s/foo/bar"), None);
        assert_eq!(parse_path_arg("42"), None);
        assert_eq!(parse_path_arg("tabnew"), None);
        assert_eq!(parse_path_arg("w"), None);
    }

    #[test]
    fn refresh_hint_lists_cwd_for_w_with_empty_arg() {
        let dir = fresh_dir();
        std::fs::write(dir.join("alpha.sql"), "").unwrap();
        std::fs::write(dir.join("beta.sql"), "").unwrap();
        let mut s = CommandLineState::new();
        s.input = "w ".into();
        s.refresh_hint(&dir);
        assert_eq!(s.path_hint.len(), 2);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn refresh_hint_clears_when_input_is_not_path_arg() {
        let dir = fresh_dir();
        std::fs::write(dir.join("a"), "").unwrap();
        let mut s = CommandLineState::new();
        s.input = "w ".into();
        s.refresh_hint(&dir);
        assert!(!s.path_hint.is_empty());
        // Switch to a non-path command — dropdown must clear.
        s.input = "s/foo/bar/".into();
        s.refresh_hint(&dir);
        assert!(s.path_hint.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn commit_hint_replaces_only_path_arg_portion() {
        let dir = fresh_dir();
        std::fs::write(dir.join("alpha.sql"), "").unwrap();
        let mut s = CommandLineState::new();
        s.input = "w al".into();
        s.refresh_hint(&dir);
        s.path_hint.select_next();
        let committed = s.commit_hint_if_active();
        assert!(committed);
        assert_eq!(s.input, "w alpha.sql");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn commit_hint_returns_false_without_selection() {
        let mut s = CommandLineState::new();
        s.input = "w foo".into();
        // No refresh / no select — nothing to commit.
        assert!(!s.commit_hint_if_active());
        assert_eq!(s.input, "w foo");
    }
}
