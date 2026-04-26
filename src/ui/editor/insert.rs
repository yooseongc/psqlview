//! Insert-mode key handler.

use crossterm::event::KeyEvent;

use super::edit::{self, EditOutcome};
use super::EditorState;

impl EditorState {
    pub(super) fn handle_insert_key(&mut self, key: KeyEvent) -> bool {
        let pre = self.buf.clone();
        let outcome = edit::handle_key(&mut self.buf, key);
        if outcome == EditOutcome::Changed {
            self.undo.record(&pre);
            true
        } else {
            false
        }
    }
}
