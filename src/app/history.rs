use super::App;

const HISTORY_MAX: usize = 50;

impl App {
    /// Pushes a query onto the in-session history, de-duplicating the most
    /// recent entry so repeated F5 presses don't spam the buffer. Resets
    /// the recall cursor.
    pub(super) fn push_history(&mut self, sql: &str) {
        let trimmed = sql.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.front().map(|s| s.as_str()) == Some(trimmed) {
            self.history_cursor = None;
            return;
        }
        self.history.push_front(trimmed.to_string());
        while self.history.len() > HISTORY_MAX {
            self.history.pop_back();
        }
        self.history_cursor = None;
    }

    /// Recalls an earlier query into the editor (Ctrl+Up). No-op at the
    /// oldest entry.
    pub(super) fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.history_cursor {
            None => 0,
            Some(i) => (i + 1).min(self.history.len() - 1),
        };
        if let Some(entry) = self.history.get(next).cloned() {
            self.editor_mut().set_text(&entry);
            self.mark_active_dirty();
            self.history_cursor = Some(next);
        }
    }

    /// Steps back toward the present (Ctrl+Down). At the newest entry it
    /// clears the editor, matching shell history feel.
    pub(super) fn history_next(&mut self) {
        let Some(i) = self.history_cursor else { return };
        if i == 0 {
            self.editor_mut().set_text("");
            self.mark_active_dirty();
            self.history_cursor = None;
        } else {
            let new = i - 1;
            if let Some(entry) = self.history.get(new).cloned() {
                self.editor_mut().set_text(&entry);
                self.mark_active_dirty();
                self.history_cursor = Some(new);
            }
        }
    }
}
