//! Vim-style ex command line. Single-line input pinned to the bottom
//! of the editor area; absorbs every keystroke until Enter / Esc.
//!
//! Parsing is split out from execution so the App side can dispatch
//! into existing primitives (`EditorState::goto_line`,
//! `EditorState::replace_all`, file-prompt machinery, tab management,
//! quit). Unsupported subset is intentional — vim's full ex-command
//! grammar is way out of scope for an SQL editor.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

#[derive(Debug, Default)]
pub struct CommandLineState {
    pub input: String,
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

pub fn handle_key(state: &mut CommandLineState, key: KeyEvent) -> CommandLineOutcome {
    match key.code {
        KeyCode::Esc => CommandLineOutcome::Cancel,
        KeyCode::Enter => CommandLineOutcome::Submit,
        KeyCode::Backspace => {
            state.pop_char();
            CommandLineOutcome::Stay
        }
        KeyCode::Char(c) if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
            state.push_char(c);
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

    #[test]
    fn handle_key_appends_chars_into_input() {
        let mut s = CommandLineState::new();
        let out = handle_key(
            &mut s,
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE),
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
        );
        assert_eq!(s.input, "ab");
    }

    #[test]
    fn handle_key_esc_cancels() {
        let mut s = CommandLineState::new();
        let out = handle_key(&mut s, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert_eq!(out, CommandLineOutcome::Cancel);
    }

    #[test]
    fn handle_key_enter_submits() {
        let mut s = CommandLineState::new();
        s.input = "42".into();
        let out = handle_key(&mut s, KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
        assert_eq!(out, CommandLineOutcome::Submit);
    }
}
