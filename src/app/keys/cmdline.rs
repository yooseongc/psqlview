//! `:` command-line key handler + parsed-command dispatcher.

use crossterm::event::{KeyCode, KeyEvent};

use super::{App, FocusPane};
use crate::ui::command_line::{self, Command, CommandLineOutcome};
use crate::ui::editor::buffer::Cursor;
use crate::ui::file_prompt::FilePromptMode;
use crate::ui::find::FindState;
use crate::ui::substitute_confirm::SubstituteState;

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
                confirm,
            } => {
                if confirm {
                    self.open_subst_confirm(all_lines, pattern, replacement);
                } else {
                    self.execute_substitute(all_lines, &pattern, &replacement, global);
                }
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

    /// Spawns the interactive substitute confirm modal. Cursor jumps
    /// to the first match so the user sees what's about to change;
    /// when there are no matches we toast and stay where we were.
    fn open_subst_confirm(&mut self, all_lines: bool, pattern: String, replacement: String) {
        let lines: Vec<String> = self.editor().lines().to_vec();
        let (cur_row, cur_col) = self.editor().cursor_pos();
        let cursor = Cursor::new(cur_row, cur_col);
        let restrict_row = if all_lines { None } else { Some(cur_row) };
        let state = SubstituteState::new(
            pattern.clone(),
            replacement,
            false,
            cursor,
            restrict_row,
            &lines,
        );
        if state.done() {
            self.toast_error(format!("no match for {pattern}"));
            return;
        }
        if let Some((start, _)) = state.current() {
            self.editor_mut().jump_caret(start);
        }
        self.subst_confirm = Some(state);
    }

    /// Routes y/n/a/q/Esc into the active substitute confirm modal.
    /// Other keys are silently swallowed so global hotkeys can't
    /// accidentally cancel a confirm half-way through.
    pub(super) fn handle_subst_confirm_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char('Q') => {
                self.close_subst_confirm();
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.subst_confirm_accept_one();
            }
            KeyCode::Char('n') | KeyCode::Char('N') => {
                self.subst_confirm_skip_one();
            }
            KeyCode::Char('a') | KeyCode::Char('A') => {
                self.subst_confirm_accept_rest();
            }
            _ => {}
        }
    }

    fn subst_confirm_accept_one(&mut self) {
        let Some(state) = self.subst_confirm.as_ref() else {
            return;
        };
        let Some((start, end)) = state.current() else {
            self.close_subst_confirm();
            return;
        };
        let replacement = state.replacement.clone();
        self.editor_mut().replace_range(start, end, &replacement);
        self.mark_active_dirty();
        let lines: Vec<String> = self.editor().lines().to_vec();
        if let Some(s) = self.subst_confirm.as_mut() {
            s.after_accept(&lines);
        }
        self.subst_confirm_advance_or_close();
    }

    fn subst_confirm_skip_one(&mut self) {
        let lines: Vec<String> = self.editor().lines().to_vec();
        if let Some(s) = self.subst_confirm.as_mut() {
            s.after_skip(&lines);
        }
        self.subst_confirm_advance_or_close();
    }

    fn subst_confirm_accept_rest(&mut self) {
        loop {
            let Some(state) = self.subst_confirm.as_ref() else {
                return;
            };
            let Some((start, end)) = state.current() else {
                break;
            };
            let replacement = state.replacement.clone();
            self.editor_mut().replace_range(start, end, &replacement);
            let lines: Vec<String> = self.editor().lines().to_vec();
            if let Some(s) = self.subst_confirm.as_mut() {
                s.after_accept(&lines);
            }
        }
        self.mark_active_dirty();
        self.close_subst_confirm();
    }

    fn subst_confirm_advance_or_close(&mut self) {
        let next_pos = self
            .subst_confirm
            .as_ref()
            .and_then(|s| s.current())
            .map(|(start, _)| start);
        match next_pos {
            Some(p) => {
                self.editor_mut().jump_caret(p);
            }
            None => {
                self.close_subst_confirm();
            }
        }
    }

    fn close_subst_confirm(&mut self) {
        let (replaced, skipped) = self
            .subst_confirm
            .as_ref()
            .map(|s| (s.replaced, s.skipped))
            .unwrap_or((0, 0));
        self.subst_confirm = None;
        let plural = if replaced == 1 { "" } else { "s" };
        self.toast_info(format!(
            "replaced {replaced} occurrence{plural} ({skipped} skipped)"
        ));
    }
}
