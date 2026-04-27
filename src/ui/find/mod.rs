//! `Ctrl+F` incremental find — single-line literal needle. Same
//! state powers vim's `/` / `?` / `n` / `N`; the only difference is
//! `enter_closes` (vim Enter closes the overlay; Ctrl+F Enter just
//! advances) and the `anchor` cursor (vim positions the active
//! match relative to it).
//!
//! Owned by `App::find`. Lifecycle: open → edit needle → repeat
//! advance / retreat → close (Esc / vim-style Enter). On close, the
//! needle is stashed onto `TabSlot::last_search` so `n` / `N` can
//! repeat without retyping. Multi-line needles are unsupported —
//! every match starts and ends on the same row.

mod key;
pub(crate) mod match_engine;
mod render;

#[cfg(test)]
mod tests;

pub use key::handle_key;
pub use render::draw;

use crate::ui::editor::buffer::Cursor;

/// Whether the overlay is in plain Find mode (`Ctrl+F`) or
/// Find/Replace mode (`Ctrl+H`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FindMode {
    Find,
    Replace,
}

/// Which input field the keystrokes mutate — only meaningful in
/// `FindMode::Replace`. `Tab` toggles between them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplaceFocus {
    Needle,
    Replacement,
}

#[derive(Debug)]
pub struct FindState {
    pub needle: String,
    pub case_sensitive: bool,
    pub matches: Vec<(Cursor, Cursor)>,
    pub active_idx: Option<usize>,
    pub mode: FindMode,
    pub replacement: String,
    pub focus: ReplaceFocus,
    /// `true` when this overlay was opened with vim's `?` (backward).
    /// Affects which match is initially active and how Enter
    /// advances.
    pub backward: bool,
    /// When `true`, `Enter` jumps to the active match *and* closes
    /// the overlay (vim `/foo<Enter>` semantics). When `false`,
    /// Enter just advances and keeps the overlay open (Ctrl+F
    /// semantics).
    pub enter_closes: bool,
    /// Cursor position at the moment the overlay opened. When set,
    /// `recompute` positions `active_idx` at the nearest match in
    /// the search direction relative to this anchor — matching
    /// vim's "search starts from cursor" behavior.
    pub anchor: Option<Cursor>,
    /// Cursor position at the moment the overlay opened *from
    /// Visual mode*. Two effects: (1) every match jump uses
    /// `jump_caret_keep_selection` so the Visual selection extends
    /// instead of collapsing, and (2) Esc restores the cursor here
    /// so cancelling search doesn't leave the selection mid-flight.
    pub pre_find_cursor: Option<Cursor>,
}

impl Default for FindState {
    fn default() -> Self {
        Self::new()
    }
}

impl FindState {
    pub fn new() -> Self {
        Self {
            needle: String::new(),
            case_sensitive: false,
            matches: Vec::new(),
            active_idx: None,
            mode: FindMode::Find,
            replacement: String::new(),
            focus: ReplaceFocus::Needle,
            backward: false,
            enter_closes: false,
            anchor: None,
            pre_find_cursor: None,
        }
    }

    pub fn new_replace() -> Self {
        Self {
            mode: FindMode::Replace,
            ..Self::new()
        }
    }

    pub fn with_needle(needle: String, case_sensitive: bool) -> Self {
        Self {
            needle,
            case_sensitive,
            ..Self::new()
        }
    }

    /// Vim-style `/` (forward) / `?` (backward) entry. Anchor
    /// records the cursor position at open time so each `recompute`
    /// activates the nearest match in the search direction. Enter
    /// closes the overlay after jumping.
    pub fn new_vim_search(backward: bool, anchor: Cursor) -> Self {
        Self {
            backward,
            enter_closes: true,
            anchor: Some(anchor),
            ..Self::new()
        }
    }

    /// Vim-style search opened from Visual mode. Behaves like
    /// `new_vim_search` but also records `pre_find_cursor` so the
    /// caller (a) keeps the active selection while jumping to
    /// matches and (b) restores the cursor on Esc.
    pub fn new_vim_search_from_visual(backward: bool, cursor: Cursor) -> Self {
        Self {
            backward,
            enter_closes: true,
            anchor: Some(cursor),
            pre_find_cursor: Some(cursor),
            ..Self::new()
        }
    }

    /// Rescans `lines` for occurrences of the current needle. With
    /// an `anchor` set, `active_idx` lands on the nearest match in
    /// the search direction; otherwise it falls back to the first
    /// match.
    pub fn recompute(&mut self, lines: &[String]) {
        self.matches.clear();
        if self.needle.is_empty() {
            self.active_idx = None;
            return;
        }
        for (row, line) in lines.iter().enumerate() {
            for (start, end) in match_engine::find_in_line(line, &self.needle, !self.case_sensitive)
            {
                self.matches
                    .push((Cursor::new(row, start), Cursor::new(row, end)));
            }
        }
        if self.matches.is_empty() {
            self.active_idx = None;
            return;
        }
        self.active_idx = match self.anchor {
            Some(anchor) if self.backward => Some(
                self.matches
                    .iter()
                    .rposition(|(s, _)| (s.row, s.col) < (anchor.row, anchor.col))
                    .unwrap_or(self.matches.len() - 1),
            ),
            Some(anchor) => Some(
                self.matches
                    .iter()
                    .position(|(s, _)| (s.row, s.col) >= (anchor.row, anchor.col))
                    .unwrap_or(0),
            ),
            None => Some(0),
        };
    }

    /// Advances `active_idx` and returns the new active match's
    /// start cursor (so the caller can scroll the editor to it).
    /// Wraps.
    pub fn advance(&mut self) -> Option<Cursor> {
        if self.matches.is_empty() {
            return None;
        }
        let i = match self.active_idx {
            Some(i) => (i + 1) % self.matches.len(),
            None => 0,
        };
        self.active_idx = Some(i);
        Some(self.matches[i].0)
    }

    /// Steps `active_idx` backward and returns the new start cursor.
    pub fn retreat(&mut self) -> Option<Cursor> {
        if self.matches.is_empty() {
            return None;
        }
        let n = self.matches.len();
        let i = match self.active_idx {
            Some(i) => (i + n - 1) % n,
            None => n - 1,
        };
        self.active_idx = Some(i);
        Some(self.matches[i].0)
    }

    /// Status line shown in the overlay title — `[3/12]` style.
    /// Empty string when no needle has been typed yet.
    pub fn status_label(&self) -> String {
        if self.needle.is_empty() {
            return String::new();
        }
        match self.active_idx {
            Some(i) => format!("[{}/{}]", i + 1, self.matches.len()),
            None => "[no match]".to_string(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum FindOutcome {
    /// Stay open with current state.
    Stay,
    /// Close the overlay; caller may stash `state.needle` onto the
    /// active tab so `n` / `N` can repeat afterwards.
    Cancel,
    /// Jump the editor caret to this cursor (the start of the
    /// active match) and keep the overlay open.
    JumpTo(Cursor),
    /// Vim-style `/foo<Enter>` — jump to this cursor and *close*
    /// the overlay. Caller stashes the needle and direction onto
    /// the active tab so subsequent `n` / `N` can repeat the
    /// search.
    JumpAndClose(Cursor),
    /// Replace one range with `text` (caller updates the buffer
    /// and recomputes matches). Used by Enter on the Replacement
    /// field in `FindMode::Replace`.
    ReplaceOne {
        range: (Cursor, Cursor),
        text: String,
    },
    /// Replace every range in one undo step. Used by `Alt+A` in
    /// `FindMode::Replace`.
    ReplaceAll {
        ranges: Vec<(Cursor, Cursor)>,
        text: String,
    },
}
