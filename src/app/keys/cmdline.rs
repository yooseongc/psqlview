//! `:` command-line key handler + parsed-command dispatcher.

use crossterm::event::KeyEvent;

use super::{App, FocusPane};
use crate::ui::command_line::{self, Command, CommandLineOutcome};
use crate::ui::file_prompt::FilePromptMode;
use crate::ui::find::FindState;

impl App {
    /// Routes a keystroke into the `:` command line. On Submit the
    /// input is parsed and dispatched via `execute_command`; on
    /// Cancel the overlay closes without side effects.
    pub(super) fn handle_command_line_key(&mut self, key: KeyEvent) {
        let cwd = std::env::current_dir().unwrap_or_default();
        let outcome = match self.command_line.as_mut() {
            Some(s) => command_line::handle_key(s, key, &cwd),
            None => return,
        };
        match outcome {
            CommandLineOutcome::Stay => {}
            CommandLineOutcome::Cancel => {
                self.command_line = None;
            }
            CommandLineOutcome::Submit => {
                let input = self
                    .command_line
                    .as_ref()
                    .map(|s| s.input.clone())
                    .unwrap_or_default();
                self.command_line = None;
                match command_line::parse(&input) {
                    Ok(cmd) => self.execute_command(cmd),
                    Err(msg) => self.toast_error(msg),
                }
            }
        }
    }

    /// Dispatches a parsed ex command into the existing primitives
    /// (goto_line / replace_all / file-prompt / tab management /
    /// quit). Errors surface as toasts; the buffer is unchanged on
    /// failure paths.
    fn execute_command(&mut self, cmd: Command) {
        match cmd {
            Command::GotoLine(n) => {
                self.editor_mut().goto_line(n);
            }
            Command::Substitute {
                all_lines,
                pattern,
                replacement,
                global,
            } => {
                self.execute_substitute(all_lines, &pattern, &replacement, global);
            }
            Command::Write { path } => self.execute_write(path),
            Command::Edit { path } => self.execute_edit(&path),
            Command::TabNew => {
                self.new_tab();
                self.focus = FocusPane::Editor;
            }
            Command::TabNext => self.cycle_tab(1),
            Command::TabPrev => self.cycle_tab(-1),
            Command::TabClose => self.close_active_tab(),
            Command::Quit => {
                self.should_quit = true;
            }
            Command::Help => {
                self.cheatsheet.open();
            }
        }
    }

    fn execute_substitute(
        &mut self,
        all_lines: bool,
        pattern: &str,
        replacement: &str,
        global: bool,
    ) {
        let mut state = FindState::with_needle(pattern.to_string(), false);
        state.recompute(self.editor().lines());
        let cur_row = self.editor().cursor_pos().0;
        let scoped: Vec<_> = state
            .matches
            .into_iter()
            .filter(|(s, _)| all_lines || s.row == cur_row)
            .collect();
        let ranges: Vec<_> = if global {
            scoped
        } else {
            // Without `g`, vim replaces only the first match per line.
            let mut seen_rows = std::collections::HashSet::new();
            scoped
                .into_iter()
                .filter(|(s, _)| seen_rows.insert(s.row))
                .collect()
        };
        if ranges.is_empty() {
            self.toast_error(format!("no match for {pattern}"));
            return;
        }
        let count = ranges.len();
        self.editor_mut().replace_all(&ranges, replacement);
        self.mark_active_dirty();
        self.toast_info(format!("replaced {count} occurrences"));
    }

    fn execute_write(&mut self, path_arg: Option<String>) {
        // Either an explicit `:w foo.sql` arg, the active tab's known
        // path, or fall back to the Save prompt.
        let path = path_arg
            .map(|arg| {
                let cwd = std::env::current_dir().unwrap_or_default();
                crate::ui::file_prompt::resolve(&arg, &cwd)
            })
            .or_else(|| self.tabs.active().path.clone());
        match path {
            Some(p) => self.commit_save(&p),
            None => self.open_file_prompt(FilePromptMode::Save),
        }
    }

    fn execute_edit(&mut self, arg: &str) {
        let cwd = std::env::current_dir().unwrap_or_default();
        let path = crate::ui::file_prompt::resolve(arg, &cwd);
        self.commit_open(&path);
    }
}
