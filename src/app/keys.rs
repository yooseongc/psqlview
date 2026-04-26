use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, FocusPane, QueryStatus, Screen};
use crate::event::AppEvent;
use crate::ui::command_line::{self, Command, CommandLineOutcome, CommandLineState};
use crate::ui::editor::buffer::Cursor;
use crate::ui::editor::mode::Mode;
use crate::ui::file_prompt::FilePromptMode;
use crate::ui::find::{self, FindOutcome, FindState};

impl App {
    pub(super) fn on_key(&mut self, key: KeyEvent) {
        // Global hotkeys first. Ctrl+C / Ctrl+Q quit unconditionally.
        if is_ctrl_c(&key) || is_ctrl_q(&key) {
            self.should_quit = true;
            return;
        }

        // The file prompt is the most aggressive modal: while it's open,
        // every key (except quit, handled above) goes to it. We don't want
        // a stray F1 or `?` to dismiss the dialog mid-typing.
        if self.file_prompt.is_some() {
            self.handle_file_prompt_key(key);
            return;
        }

        // `:` command line — single-line ex prompt, modal at the same
        // priority slot the v0.4 goto-line overlay used. Absorbs every
        // key until Enter / Esc.
        if self.command_line.is_some() {
            self.handle_command_line_key(key);
            return;
        }

        // Find / Find-Replace overlay — absorbs all keys (printable,
        // Backspace, Enter / F3 / Shift+F3 advance, Alt+C toggle,
        // Esc closes).
        if self.find.is_some() {
            self.handle_find_key(key);
            return;
        }

        // Modal overlays capture keys before any pane does. Cheatsheet
        // is scrollable now, so the handler also routes Up/Down/PageUp/
        // PageDown into the scroll position.
        if self.cheatsheet.open {
            match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') | KeyCode::Char('q') => {
                    self.cheatsheet.close();
                }
                KeyCode::Up | KeyCode::Char('k') => self.cheatsheet.scroll_up(1),
                KeyCode::Down | KeyCode::Char('j') => self.cheatsheet.scroll_down(1),
                KeyCode::PageUp => self.cheatsheet.scroll_up(10),
                KeyCode::PageDown => self.cheatsheet.scroll_down(10),
                KeyCode::Home => self.cheatsheet.scroll = 0,
                KeyCode::End => self.cheatsheet.scroll_down(u16::MAX / 2),
                _ => {}
            }
            return;
        }
        if self.row_detail.open {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.row_detail.close(),
                KeyCode::Up | KeyCode::Char('k') => self.row_detail.scroll_up(1),
                KeyCode::Down | KeyCode::Char('j') => self.row_detail.scroll_down(1),
                KeyCode::PageUp => self.row_detail.scroll_up(10),
                KeyCode::PageDown => self.row_detail.scroll_down(10),
                _ => {}
            }
            return;
        }
        // F1 anywhere opens the cheatsheet. `?` also opens it, but only
        // outside contexts that swallow the character (editor, search,
        // autocomplete — those treat it as typed input).
        let help_via_slash = matches!(key.code, KeyCode::Char('?'))
            && !self.connecting
            && !matches!(self.query_status, QueryStatus::Running { .. })
            && self.tree.search.is_none()
            && self.autocomplete.is_none()
            && !(self.focus == FocusPane::Editor && self.screen == Screen::Workspace);
        let help_via_f1 = matches!(key.code, KeyCode::F(1));
        if help_via_slash || help_via_f1 {
            self.cheatsheet.open();
            return;
        }
        // Esc dismisses a visible toast immediately before anything else
        // reads Esc. Skipped while a modal sub-state owns Esc: connecting,
        // running query, active autocomplete, or tree incremental search.
        if matches!(key.code, KeyCode::Esc)
            && self.toast.is_some()
            && !self.connecting
            && !matches!(self.query_status, QueryStatus::Running { .. })
            && self.autocomplete.is_none()
            && self.tree.search.is_none()
        {
            self.toast = None;
            return;
        }
        // Direct pane switches (Workspace only).
        // Primary: F2/F3/F4 — chosen because they don't clash with common
        // terminal shortcuts (Tabby's Alt+digit hijacks tab switching).
        // Alt+1/2/3 is kept as a backup for users whose terminals pass it.
        if self.screen == Screen::Workspace {
            let target = match key.code {
                KeyCode::F(2) => Some(FocusPane::Tree),
                KeyCode::F(3) => Some(FocusPane::Editor),
                KeyCode::F(4) => Some(FocusPane::Results),
                KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::ALT) => match c {
                    '1' => Some(FocusPane::Tree),
                    '2' => Some(FocusPane::Editor),
                    '3' => Some(FocusPane::Results),
                    _ => None,
                },
                _ => None,
            };
            if let Some(pane) = target {
                self.set_focus(pane);
                return;
            }
        }

        match self.screen {
            Screen::Connect => self.on_key_connect(key),
            Screen::Workspace => self.on_key_workspace(key),
        }
    }

    fn on_key_connect(&mut self, key: KeyEvent) {
        if self.connecting {
            if matches!(key.code, KeyCode::Esc) {
                self.connecting = false;
                self.toast_info("connect cancelled".into());
            }
            return;
        }
        // Esc on an idle connect dialog is a no-op now; Ctrl+Q quits.
        if matches!(key.code, KeyCode::Esc) {
            return;
        }
        let submit = self.connect_dialog.handle_key(key);
        if submit {
            self.begin_connect();
        }
    }

    fn on_key_workspace(&mut self, key: KeyEvent) {
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
        // the user commits (Enter) or cancels (Esc). Otherwise Tab would
        // cycle focus out mid-search, F5 would run a query, etc.
        if self.focus == FocusPane::Tree && self.tree.search.is_some() {
            self.on_key_tree(key);
            return;
        }

        // Ctrl+Enter runs the current query regardless of focus. Some
        // terminals deliver this as Ctrl+J (the literal LF character)
        // because the standard VT protocol can't distinguish Ctrl+Enter
        // from Enter — we accept both so the shortcut works without
        // requiring kitty keyboard protocol support.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && (matches!(key.code, KeyCode::Enter)
                || matches!(key.code, KeyCode::Char('j') | KeyCode::Char('J')))
        {
            self.run_current_query();
            return;
        }

        // Ctrl+E exports the current result set to a CSV file. Pane-
        // independent: works whether you're focused on the tree, editor,
        // or results — the prompt cares about results.current, not focus.
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
                // Ctrl+PageUp → previous. Ctrl+[ is the same byte as
                // Esc on standard VT; the kitty-keyboard-protocol push
                // in main.rs disambiguates on supported terminals.
                // Ctrl+PageUp/Down is the universal fallback.
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

        // Ctrl+Up/Down in the editor recalls past queries from session
        // history. Ignored outside the editor so the tree/results panes
        // keep their scroll semantics. Ctrl+O / Ctrl+S open the file
        // prompt for read / write.
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
                    // Ctrl+G is preserved as a goto-line shortcut for
                    // muscle memory — it now opens the `:` command
                    // line, where bare digits jump to that line.
                    self.command_line = Some(CommandLineState::new());
                    self.autocomplete = None;
                    return;
                }
                KeyCode::Char('f') | KeyCode::Char('F') => {
                    // Reuse last_search if any — opening Find with the
                    // previous needle pre-typed is the natural flow.
                    let initial = self.tabs.active().last_search.clone().unwrap_or_default();
                    let mut state = FindState::with_needle(initial, false);
                    state.recompute(self.editor().lines());
                    self.find = Some(state);
                    self.autocomplete = None;
                    return;
                }
                KeyCode::Char('h') | KeyCode::Char('H') => {
                    // Same prefill rule as Ctrl+F — but the overlay
                    // opens in Replace mode with the Replacement field
                    // initially empty.
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

        // Vim-style search + `:` command line — only meaningful when
        // the editor pane is focused and the editor is in Normal mode.
        // The cheatsheet `?` shortcut is already gated off for
        // editor-focused workspace, so `?` here doesn't clash.
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

        // While the autocomplete popup is open it consumes most keys first.
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
                    // Any direct edit invalidates an in-progress history
                    // walk; user is no longer just browsing.
                    self.history_cursor = None;
                }
                FocusPane::Tree => self.on_key_tree(key),
                FocusPane::Results => {
                    // y / Y copy via OSC 52 to the host terminal's clipboard.
                    // Routed before results.handle_key so the keys don't fall
                    // through to a future scroll binding.
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
                    // refreshing a long-running result without going back
                    // to the editor.
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

    fn on_key_tree(&mut self, key: KeyEvent) {
        // Incremental-search mode: characters extend the needle and
        // rejump; Enter commits; Esc cancels (last committed needle
        // preserved so `n`/`N` still work).
        if self.tree.search.is_some() {
            match key.code {
                KeyCode::Char(c) => {
                    if let Some(needle) = self.tree.search.as_mut() {
                        needle.push(c);
                    }
                    if let Some(needle) = self.tree.search.clone() {
                        if let Some(idx) = self.tree.find_next(&needle, self.tree.selected) {
                            self.tree.selected = idx;
                        }
                    }
                }
                KeyCode::Backspace => {
                    if let Some(needle) = self.tree.search.as_mut() {
                        needle.pop();
                    }
                }
                KeyCode::Enter => {
                    self.tree.last_search = self.tree.search.take();
                }
                KeyCode::Esc => {
                    self.tree.search = None;
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Char('/') => {
                self.tree.search = Some(String::new());
            }
            KeyCode::Char('n') => {
                if let Some(needle) = self.tree.last_search.clone() {
                    if let Some(idx) = self.tree.find_next(&needle, self.tree.selected) {
                        self.tree.selected = idx;
                    }
                }
            }
            KeyCode::Char('N') => {
                if let Some(needle) = self.tree.last_search.clone() {
                    if let Some(idx) = self.tree.find_prev(&needle, self.tree.selected) {
                        self.tree.selected = idx;
                    }
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.tree.move_up(),
            KeyCode::Down | KeyCode::Char('j') => self.tree.move_down(),
            KeyCode::PageUp => self.tree.page_up(),
            KeyCode::PageDown => self.tree.page_down(),
            KeyCode::Home => self.tree.jump_to_start(),
            KeyCode::End => self.tree.jump_to_end(),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => self.expand_current_tree_node(),
            KeyCode::Left | KeyCode::Char('h') => self.tree.collapse_current(),
            KeyCode::Char('p') | KeyCode::Char(' ') => self.run_preview_for_selected_relation(),
            KeyCode::Char('D') => self.show_ddl_for_selected_relation(),
            _ => {}
        }
    }

    fn expand_current_tree_node(&mut self) {
        let Some(node) = self.tree.current_node() else {
            return;
        };
        let Some(session) = &self.session else {
            return;
        };
        let client = session.client();

        match node {
            crate::ui::schema_tree::NodeRef::Schema { name, loaded } => {
                if loaded {
                    self.tree.toggle_current();
                } else {
                    self.tree.mark_loading_current();
                    let tx = self.tx.clone();
                    let schema = name.clone();
                    tokio::spawn(async move {
                        let r = crate::db::catalog::list_relations(&client, &schema).await;
                        let _ = tx.send(AppEvent::RelationsLoaded { schema, result: r });
                    });
                }
            }
            crate::ui::schema_tree::NodeRef::Relation {
                schema,
                name,
                loaded,
                ..
            } => {
                if loaded {
                    self.tree.toggle_current();
                } else {
                    self.tree.mark_loading_current();
                    let tx = self.tx.clone();
                    let s = schema.clone();
                    let t = name.clone();
                    tokio::spawn(async move {
                        let r = crate::db::catalog::list_columns(&client, &s, &t).await;
                        let _ = tx.send(AppEvent::ColumnsLoaded {
                            schema: s,
                            table: t,
                            result: r,
                        });
                    });
                }
            }
            crate::ui::schema_tree::NodeRef::Column { .. } => {}
        }
    }

    /// Routes a keystroke into the `:` command line. On Submit the
    /// input is parsed and dispatched via `execute_command`; on
    /// Cancel the overlay closes without side effects.
    fn handle_command_line_key(&mut self, key: KeyEvent) {
        let outcome = match self.command_line.as_mut() {
            Some(s) => command_line::handle_key(s, key),
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
        let lines = self.editor().lines().to_vec();
        let mut state = FindState::with_needle(pattern.to_string(), false);
        state.recompute(&lines);
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
        if let Some(arg) = path_arg {
            let cwd = std::env::current_dir().unwrap_or_default();
            let path = crate::ui::file_prompt::resolve(&arg, &cwd);
            match std::fs::write(&path, self.editor().text()) {
                Ok(()) => {
                    let active = self.tabs.active_mut();
                    active.path = Some(path.clone());
                    active.dirty = false;
                    self.toast_info(format!("saved: {}", path.display()));
                }
                Err(e) => self.toast_error(format!("save failed: {e}")),
            }
            return;
        }
        // Bare `:w` — silent save when path known, else open Save prompt.
        if let Some(path) = self.tabs.active().path.clone() {
            match std::fs::write(&path, self.editor().text()) {
                Ok(()) => {
                    self.tabs.active_mut().dirty = false;
                    self.toast_info(format!("saved: {}", path.display()));
                }
                Err(e) => self.toast_error(format!("save failed: {e}")),
            }
        } else {
            self.open_file_prompt(FilePromptMode::Save);
        }
    }

    fn execute_edit(&mut self, arg: &str) {
        let cwd = std::env::current_dir().unwrap_or_default();
        let path = crate::ui::file_prompt::resolve(arg, &cwd);
        match std::fs::read_to_string(&path) {
            Ok(text) => {
                let normalized = text.replace("\r\n", "\n");
                self.editor_mut().set_text(&normalized);
                let active = self.tabs.active_mut();
                active.path = Some(path.clone());
                active.dirty = false;
                self.toast_info(format!("opened: {}", path.display()));
            }
            Err(e) => self.toast_error(format!("open failed: {e}")),
        }
    }

    /// Routes a keystroke into the Find overlay. Edits the needle,
    /// jumps the caret to matches, and on Esc / Enter stashes the
    /// needle (and search direction, for vim mode) onto the active
    /// tab's `last_search` so `n` / `N` can repeat after close.
    fn handle_find_key(&mut self, key: KeyEvent) {
        let lines: Vec<String> = self.editor().lines().to_vec();
        let outcome = match self.find.as_mut() {
            Some(s) => find::handle_key(s, key, &lines),
            None => return,
        };
        match outcome {
            FindOutcome::Stay => {}
            FindOutcome::Cancel => {
                let (needle, backward) = self
                    .find
                    .as_ref()
                    .map(|s| (s.needle.clone(), s.backward))
                    .unwrap_or_default();
                self.find = None;
                let active = self.tabs.active_mut();
                if needle.is_empty() {
                    active.last_search = None;
                } else {
                    active.last_search = Some(needle);
                    active.last_search_backward = backward;
                }
            }
            FindOutcome::JumpTo(c) => {
                self.editor_mut().jump_caret(c);
            }
            FindOutcome::JumpAndClose(c) => {
                let (needle, backward) = self
                    .find
                    .as_ref()
                    .map(|s| (s.needle.clone(), s.backward))
                    .unwrap_or_default();
                self.find = None;
                self.editor_mut().jump_caret(c);
                let active = self.tabs.active_mut();
                if !needle.is_empty() {
                    active.last_search = Some(needle);
                    active.last_search_backward = backward;
                }
            }
            FindOutcome::ReplaceOne {
                range: (start, end),
                text,
            } => {
                self.editor_mut().replace_range(start, end, &text);
                self.mark_active_dirty();
                // Recompute matches against the post-replacement buffer
                // so the overlay's count and active_idx stay accurate.
                let lines: Vec<String> = self.editor().lines().to_vec();
                if let Some(s) = self.find.as_mut() {
                    s.recompute(&lines);
                }
            }
            FindOutcome::ReplaceAll { ranges, text } => {
                let count = ranges.len();
                self.editor_mut().replace_all(&ranges, &text);
                self.mark_active_dirty();
                let lines: Vec<String> = self.editor().lines().to_vec();
                if let Some(s) = self.find.as_mut() {
                    s.recompute(&lines);
                }
                self.toast_info(format!("replaced {count} occurrences"));
            }
        }
    }

    /// Vim `/` (forward) / `?` (backward) entry. Opens a fresh Find
    /// overlay anchored at the cursor; the anchor steers `recompute`
    /// to the nearest match in the search direction so each typed
    /// char highlights the right match. Enter closes (vim semantics).
    fn open_vim_search(&mut self, backward: bool) {
        let (r, c) = self.editor().cursor_pos();
        let anchor = Cursor::new(r, c);
        let mut state = FindState::new_vim_search(backward, anchor);
        state.recompute(self.editor().lines());
        self.find = Some(state);
        self.autocomplete = None;
    }

    /// Vim `n` (`reverse=false`) / `N` (`reverse=true`) repeat. Looks
    /// up the active tab's `last_search` + `last_search_backward` and
    /// jumps to the next/prev match strictly past the cursor. Toasts
    /// when no last search or no match is found.
    fn repeat_vim_search(&mut self, reverse: bool) {
        let active = self.tabs.active();
        let Some(needle) = active.last_search.clone() else {
            self.toast_info("no previous search".into());
            return;
        };
        let backward = active.last_search_backward ^ reverse;
        let lines = self.editor().lines().to_vec();
        let (cur_row, cur_col) = self.editor().cursor_pos();

        let mut state = FindState::with_needle(needle.clone(), false);
        state.recompute(&lines);
        if state.matches.is_empty() {
            self.toast_info(format!("no match for {needle}"));
            return;
        }
        let target = if backward {
            state
                .matches
                .iter()
                .rev()
                .find(|(s, _)| (s.row, s.col) < (cur_row, cur_col))
                .or_else(|| state.matches.last())
                .map(|(s, _)| *s)
        } else {
            state
                .matches
                .iter()
                .find(|(s, _)| (s.row, s.col) > (cur_row, cur_col))
                .or_else(|| state.matches.first())
                .map(|(s, _)| *s)
        };
        if let Some(c) = target {
            self.editor_mut().jump_caret(c);
        }
    }
}

fn is_ctrl_c(k: &KeyEvent) -> bool {
    k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('c' | 'C'))
}

fn is_ctrl_q(k: &KeyEvent) -> bool {
    k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('q' | 'Q'))
}
