//! Find / Replace overlay key handler + vim `/`/`?`/`n`/`N` entry
//! and repeat helpers.

use crossterm::event::KeyEvent;

use super::App;
use crate::ui::editor::buffer::Cursor;
use crate::ui::find::{self, FindOutcome, FindState};

impl App {
    /// Routes a keystroke into the Find overlay. Edits the needle,
    /// jumps the caret to matches, and on Esc / Enter stashes the
    /// needle (and search direction, for vim mode) onto the active
    /// tab's `last_search` so `n` / `N` can repeat after close.
    pub(super) fn handle_find_key(&mut self, key: KeyEvent) {
        let lines: Vec<String> = self.editor().lines().to_vec();
        let outcome = match self.find.as_mut() {
            Some(s) => find::handle_key(s, key, &lines),
            None => return,
        };
        match outcome {
            FindOutcome::Stay => {}
            FindOutcome::Cancel => {
                self.close_find_and_stash_needle();
            }
            FindOutcome::JumpTo(c) => {
                self.editor_mut().jump_caret(c);
            }
            FindOutcome::JumpAndClose(c) => {
                self.close_find_and_stash_needle();
                self.editor_mut().jump_caret(c);
            }
            FindOutcome::ReplaceOne {
                range: (start, end),
                text,
            } => {
                self.editor_mut().replace_range(start, end, &text);
                self.mark_active_dirty();
                // Recompute matches against the post-replacement
                // buffer so the overlay's count and active_idx stay
                // accurate.
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

    /// Closes the Find overlay and stashes its needle + direction
    /// onto the active tab so subsequent `n` / `N` can repeat. An
    /// empty needle clears the slot so `n` doesn't surface a stale
    /// search.
    fn close_find_and_stash_needle(&mut self) {
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

    /// Vim `/` (forward) / `?` (backward) entry. Opens a fresh Find
    /// overlay anchored at the cursor; the anchor steers `recompute`
    /// to the nearest match in the search direction so each typed
    /// char highlights the right match. Enter closes (vim semantics).
    pub(super) fn open_vim_search(&mut self, backward: bool) {
        let (r, c) = self.editor().cursor_pos();
        let anchor = Cursor::new(r, c);
        let mut state = FindState::new_vim_search(backward, anchor);
        state.recompute(self.editor().lines());
        self.find = Some(state);
        self.autocomplete = None;
    }

    /// Vim `n` (`reverse=false`) / `N` (`reverse=true`) repeat. Looks
    /// up the active tab's `last_search` + `last_search_backward`
    /// and jumps to the next/prev match strictly past the cursor.
    /// Toasts when no last search or no match is found.
    pub(super) fn repeat_vim_search(&mut self, reverse: bool) {
        let active = self.tabs.active();
        let Some(needle) = active.last_search.clone() else {
            self.toast_info("no previous search".into());
            return;
        };
        let backward = active.last_search_backward ^ reverse;
        let (cur_row, cur_col) = self.editor().cursor_pos();

        let mut state = FindState::with_needle(needle.clone(), false);
        state.recompute(self.editor().lines());
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
