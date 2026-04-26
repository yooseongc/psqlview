//! Workspace-screen key handler. Largest of the cascade — routes
//! through cancel-running-query, dirty-tab close gating, focus
//! shortcuts, tab management, history recall, file/find/replace
//! shortcuts, vim search/`:` entry, autocomplete consumption, and
//! the per-pane fallback.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, FocusPane, QueryStatus};
use crate::ui::command_line::CommandLineState;
use crate::ui::editor::mode::Mode;
use crate::ui::file_prompt::FilePromptMode;
use crate::ui::find::FindState;

impl App {
    pub(super) fn on_key_workspace(&mut self, key: KeyEvent) {
        // Handle cancellation first when a query is running.
        if matches!(&self.query_status, QueryStatus::Running { .. }) {
            if matches!(key.code, KeyCode::Esc) {
                self.cancel_running_query();
            }
            return;
        }

        // Any keystroke other than Ctrl+W cancels a pending dirty-tab
        // close confirmation. The first-strike toast auto-expires.
        let is_ctrl_w = key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('w') | KeyCode::Char('W'));
        if !is_ctrl_w {
            self.tabs.pending_close = None;
        }

        // Incremental search in the tree pane absorbs every key until
        // the user commits (Enter) or cancels (Esc). Otherwise Tab
        // would cycle focus out mid-search, F5 would run a query,
        // etc.
        if self.focus == FocusPane::Tree && self.tree.search.is_some() {
            self.on_key_tree(key);
            return;
        }

        // Ctrl+Enter runs the current query regardless of focus.
        // Some terminals deliver this as Ctrl+J (the literal LF
        // character) because the standard VT protocol can't
        // distinguish Ctrl+Enter from Enter — we accept both so the
        // shortcut works without requiring kitty keyboard protocol
        // support.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && (matches!(key.code, KeyCode::Enter)
                || matches!(key.code, KeyCode::Char('j') | KeyCode::Char('J')))
        {
            self.run_current_query();
            return;
        }

        // Ctrl+E exports the current result set to a CSV file.
        // Pane-independent: works whether you're focused on the
        // tree, editor, or results — the prompt cares about
        // results.current, not focus.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('e') | KeyCode::Char('E'))
        {
            if self.results.current.is_some() {
                self.open_file_prompt(FilePromptMode::ExportCsv);
            } else {
                self.toast_info("no result set to export".into());
            }
            return;
        }

        // Editor-pane tab management. Pane-independent — the tab bar
        // belongs to the editor pane but we don't gate on focus so a
        // user driving the tree / results pane can still create a
        // scratch tab without first switching focus.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('t') | KeyCode::Char('T') => {
                    self.new_tab();
                    self.focus = FocusPane::Editor;
                    return;
                }
                KeyCode::Char('w') | KeyCode::Char('W') => {
                    self.close_active_tab();
                    return;
                }
                // Ctrl+] / Ctrl+PageDown → next tab, Ctrl+[ /
                // Ctrl+PageUp → previous. Ctrl+[ is the same byte
                // as Esc on standard VT; the kitty-keyboard-
                // protocol push in main.rs disambiguates on
                // supported terminals. Ctrl+PageUp/Down is the
                // universal fallback.
                KeyCode::Char(']') | KeyCode::PageDown => {
                    self.cycle_tab(1);
                    return;
                }
                KeyCode::Char('[') | KeyCode::PageUp => {
                    self.cycle_tab(-1);
                    return;
                }
                KeyCode::Char(c @ '1'..='9') => {
                    let idx = (c as u8 - b'1') as usize;
                    self.jump_tab(idx);
                    return;
                }
                _ => {}
            }
        }

        // Ctrl+Up/Down in the editor recalls past queries from
        // session history. Ignored outside the editor so the tree /
        // results panes keep their scroll semantics. Ctrl+O / Ctrl+S
        // open the file prompt for read / write.
        if self.focus == FocusPane::Editor && key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Up => {
                    self.history_prev();
                    return;
                }
                KeyCode::Down => {
                    self.history_next();
                    return;
                }
                KeyCode::Char('o') | KeyCode::Char('O') => {
                    self.open_file_prompt(FilePromptMode::Open);
                    return;
                }
                KeyCode::Char('s') | KeyCode::Char('S') => {
                    self.open_file_prompt(FilePromptMode::Save);
                    return;
                }
                KeyCode::Char('g') | KeyCode::Char('G') => {
                    // Ctrl+G is preserved as a goto-line shortcut
                    // for muscle memory — it now opens the `:`
                    // command line, where bare digits jump to that
                    // line.
                    self.command_line = Some(CommandLineState::new());
                    self.autocomplete = None;
                    return;
                }
                KeyCode::Char('f') | KeyCode::Char('F') => {
                    // Reuse last_search if any — opening Find with
                    // the previous needle pre-typed is the natural
                    // flow.
                    let initial = self.tabs.active().last_search.clone().unwrap_or_default();
                    let mut state = FindState::with_needle(initial, false);
                    state.recompute(self.editor().lines());
                    self.find = Some(state);
                    self.autocomplete = None;
                    return;
                }
                KeyCode::Char('h') | KeyCode::Char('H') => {
                    // Same prefill rule as Ctrl+F — but the overlay
                    // opens in Replace mode with the Replacement
                    // field initially empty.
                    let initial = self.tabs.active().last_search.clone().unwrap_or_default();
                    let mut state = FindState::new_replace();
                    state.needle = initial;
                    state.recompute(self.editor().lines());
                    self.find = Some(state);
                    self.autocomplete = None;
                    return;
                }
                _ => {}
            }
        }

        // Vim-style search + `:` command line — only meaningful
        // when the editor pane is focused and the editor is in
        // Normal mode. The cheatsheet `?` shortcut is already gated
        // off for editor-focused workspace, so `?` here doesn't
        // clash.
        if self.focus == FocusPane::Editor
            && matches!(self.editor().mode(), Mode::Normal)
            && (key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
        {
            match key.code {
                KeyCode::Char('/') if key.modifiers.is_empty() => {
                    self.open_vim_search(false);
                    return;
                }
                KeyCode::Char('?') => {
                    self.open_vim_search(true);
                    return;
                }
                KeyCode::Char('n') if key.modifiers.is_empty() => {
                    self.repeat_vim_search(false);
                    return;
                }
                KeyCode::Char('N') if key.modifiers == KeyModifiers::SHIFT => {
                    self.repeat_vim_search(true);
                    return;
                }
                KeyCode::Char(':') => {
                    self.command_line = Some(CommandLineState::new());
                    self.autocomplete = None;
                    return;
                }
                _ => {}
            }
        }

        // While the autocomplete popup is open it consumes most
        // keys first.
        if self.autocomplete.is_some()
            && self.focus == FocusPane::Editor
            && self.handle_autocomplete_key(key)
        {
            return;
        }

        match key.code {
            KeyCode::F(5) => self.run_current_query(),
            KeyCode::Tab if key.modifiers.is_empty() => {
                if self.focus == FocusPane::Editor {
                    self.handle_editor_tab();
                } else {
                    self.focus = self.focus.cycle();
                }
            }
            KeyCode::BackTab => {
                if self.focus == FocusPane::Editor {
                    if let Some((s, e)) = self.editor().selected_line_range() {
                        self.editor_mut().outdent_lines(s, e);
                    } else {
                        self.editor_mut().outdent_current_line();
                    }
                    self.mark_active_dirty();
                } else {
                    self.focus = match self.focus {
                        FocusPane::Tree => FocusPane::Results,
                        FocusPane::Editor => FocusPane::Tree,
                        FocusPane::Results => FocusPane::Editor,
                    };
                }
            }
            _ => match self.focus {
                FocusPane::Editor => {
                    if self.editor_mut().handle_key(key) {
                        self.mark_active_dirty();
                    }
                    // Any direct edit invalidates an in-progress
                    // history walk; user is no longer just browsing.
                    self.history_cursor = None;
                }
                FocusPane::Tree => self.on_key_tree(key),
                FocusPane::Results => {
                    // y / Y copy via OSC 52 to the host terminal's
                    // clipboard. Routed before results.handle_key so
                    // the keys don't fall through to a future scroll
                    // binding.
                    if key.modifiers.is_empty() && matches!(key.code, KeyCode::Char('y')) {
                        self.copy_current_cell_to_clipboard();
                        return;
                    }
                    if key.modifiers == KeyModifiers::SHIFT
                        && matches!(key.code, KeyCode::Char('Y'))
                    {
                        self.copy_current_row_to_clipboard();
                        return;
                    }
                    // R re-runs the most recent query. Useful for
                    // refreshing a long-running result without going
                    // back to the editor.
                    if key.modifiers == KeyModifiers::SHIFT
                        && matches!(key.code, KeyCode::Char('R'))
                    {
                        self.rerun_last_query();
                        return;
                    }
                    // Enter on a populated result opens the per-row
                    // detail modal, bypassing the Results handler.
                    if matches!(key.code, KeyCode::Enter)
                        && self
                            .results
                            .current
                            .as_ref()
                            .is_some_and(|s| !s.rows.is_empty())
                    {
                        self.row_detail.open();
                    } else {
                        self.results.handle_key(key);
                    }
                }
            },
        }
    }
}
