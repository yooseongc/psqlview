//! Top-level modal cascade. Routes a `KeyEvent` through the priority
//! chain (quit → file_prompt → command_line → find → cheatsheet → row
//! detail → F1/`?` → toast Esc → focus switches → screen-specific
//! handler) and delegates pane logic to the sub-modules.

mod cmdline;
mod connect;
mod find;
mod tree;
mod util;
mod workspace;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::{App, FocusPane, QueryStatus, Screen};

use util::{is_ctrl_c, is_ctrl_q};

impl App {
    pub(in crate::app) fn on_key(&mut self, key: KeyEvent) {
        // Global hotkeys first. Ctrl+C / Ctrl+Q quit unconditionally.
        if is_ctrl_c(&key) || is_ctrl_q(&key) {
            self.should_quit = true;
            return;
        }

        // The file prompt is the most aggressive modal: while it's open,
        // every key (except quit, handled above) goes to it. We don't
        // want a stray F1 or `?` to dismiss the dialog mid-typing.
        if self.file_prompt.is_some() {
            self.handle_file_prompt_key(key);
            return;
        }

        // `:` command line — single-line ex prompt. Absorbs every
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
        // is scrollable, so the handler also routes Up/Down/PageUp/
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
        // F1 anywhere opens the cheatsheet. `?` also opens it, but
        // only outside contexts that swallow the character (editor,
        // search, autocomplete — those treat it as typed input).
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
        // Esc dismisses a visible toast immediately before anything
        // else reads Esc. Skipped while a modal sub-state owns Esc:
        // connecting, running query, active autocomplete, or tree
        // incremental search.
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
        // Primary: F2/F3/F4 — chosen because they don't clash with
        // common terminal shortcuts (Tabby's Alt+digit hijacks tab
        // switching). Alt+1/2/3 is kept as a backup for users whose
        // terminals pass it.
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
}
