//! `Ctrl+F` incremental find — single-line literal needle.
//!
//! Owned by `App::find`. Its lifecycle:
//!
//! 1. `Ctrl+F` opens an empty `FindState` (case-insensitive by default).
//! 2. Each keystroke either edits the needle (mutating `matches`),
//!    advances the active match (Enter / F3), goes back (Shift+F3),
//!    toggles case sensitivity (`Alt+C`), or closes the overlay (Esc).
//! 3. When the overlay closes, the needle is stashed onto the active
//!    `TabSlot::last_search` so `n` / `N` can repeat without retyping.
//!
//! Multi-line needles are unsupported on purpose — every match starts
//! and ends on the same row.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::ui::editor::buffer::Cursor;

/// Whether the overlay is in plain Find mode (`Ctrl+F`) or Find/Replace
/// mode (`Ctrl+H`).
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

    /// Rescans `lines` for occurrences of the current needle. Resets
    /// `active_idx` to `Some(0)` if there's at least one match, else
    /// `None`. O(N×M) — fine for SQL-buffer sizes.
    pub fn recompute(&mut self, lines: &[String]) {
        self.matches.clear();
        if self.needle.is_empty() {
            self.active_idx = None;
            return;
        }
        for (row, line) in lines.iter().enumerate() {
            for (start, end) in find_in_line(line, &self.needle, !self.case_sensitive) {
                self.matches
                    .push((Cursor::new(row, start), Cursor::new(row, end)));
            }
        }
        self.active_idx = if self.matches.is_empty() {
            None
        } else {
            Some(0)
        };
    }

    /// Advances `active_idx` and returns the new active match's start
    /// cursor (so the caller can scroll the editor to it). Wraps.
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

    /// Status line shown in the overlay title — `[3/12]` style. Empty
    /// string when no needle has been typed yet.
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
    /// Jump the editor caret to this cursor (the start of the active
    /// match) and keep the overlay open.
    JumpTo(Cursor),
    /// Replace one range with `text` (caller updates the buffer and
    /// recomputes matches). Used by Enter on the Replacement field in
    /// `FindMode::Replace`.
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

/// Routes a key into a Find overlay. The needle / matches mutate
/// in-place; the caller refreshes `recompute(lines)` whenever the
/// needle changes (handled inside this function for char/backspace,
/// callers don't have to repeat).
pub fn handle_key(state: &mut FindState, key: KeyEvent, lines: &[String]) -> FindOutcome {
    // Replace-mode-only keys come first.
    if state.mode == FindMode::Replace {
        match key.code {
            KeyCode::Tab => {
                state.focus = match state.focus {
                    ReplaceFocus::Needle => ReplaceFocus::Replacement,
                    ReplaceFocus::Replacement => ReplaceFocus::Needle,
                };
                return FindOutcome::Stay;
            }
            KeyCode::Char('a') | KeyCode::Char('A')
                if key.modifiers.contains(KeyModifiers::ALT) =>
            {
                if state.matches.is_empty() {
                    return FindOutcome::Stay;
                }
                let ranges = state.matches.clone();
                let text = state.replacement.clone();
                return FindOutcome::ReplaceAll { ranges, text };
            }
            KeyCode::Enter if state.focus == ReplaceFocus::Replacement => {
                let Some(idx) = state.active_idx else {
                    return FindOutcome::Stay;
                };
                let range = state.matches[idx];
                let text = state.replacement.clone();
                return FindOutcome::ReplaceOne { range, text };
            }
            _ => {}
        }
    }

    let mutating_replacement =
        state.mode == FindMode::Replace && state.focus == ReplaceFocus::Replacement;

    match key.code {
        KeyCode::Esc => FindOutcome::Cancel,
        // F3 / Enter advance; Shift+F3 retreats. In Replace mode the
        // Enter key on the Needle field also advances; Enter on the
        // Replacement field is handled above.
        KeyCode::F(3) | KeyCode::Enter if !key.modifiers.contains(KeyModifiers::SHIFT) => {
            match state.advance() {
                Some(c) => FindOutcome::JumpTo(c),
                None => FindOutcome::Stay,
            }
        }
        KeyCode::F(3) if key.modifiers.contains(KeyModifiers::SHIFT) => match state.retreat() {
            Some(c) => FindOutcome::JumpTo(c),
            None => FindOutcome::Stay,
        },
        KeyCode::Backspace => {
            if mutating_replacement {
                state.replacement.pop();
                FindOutcome::Stay
            } else {
                state.needle.pop();
                state.recompute(lines);
                if let Some(c) = state.matches.first().map(|(s, _)| *s) {
                    state.active_idx = Some(0);
                    FindOutcome::JumpTo(c)
                } else {
                    FindOutcome::Stay
                }
            }
        }
        KeyCode::Char('c') | KeyCode::Char('C')
            if key.modifiers.contains(KeyModifiers::ALT) && !mutating_replacement =>
        {
            state.case_sensitive = !state.case_sensitive;
            state.recompute(lines);
            FindOutcome::Stay
        }
        KeyCode::Char(c) if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT => {
            if mutating_replacement {
                state.replacement.push(c);
                FindOutcome::Stay
            } else {
                state.needle.push(c);
                state.recompute(lines);
                if let Some(c0) = state.matches.first().map(|(s, _)| *s) {
                    state.active_idx = Some(0);
                    FindOutcome::JumpTo(c0)
                } else {
                    FindOutcome::Stay
                }
            }
        }
        _ => FindOutcome::Stay,
    }
}

/// Finds every char-position occurrence of `needle` in `line`. Returns
/// `(start_col, end_col)` pairs in char units. Matches don't overlap —
/// once a match lands, scanning resumes at `start + needle.chars()`.
fn find_in_line(line: &str, needle: &str, case_insensitive: bool) -> Vec<(usize, usize)> {
    let line_chars: Vec<char> = line.chars().collect();
    let needle_chars: Vec<char> = needle.chars().collect();
    let mut out = Vec::new();
    let m = needle_chars.len();
    let n = line_chars.len();
    if m == 0 || m > n {
        return out;
    }
    let cmp = |a: char, b: char| -> bool {
        if case_insensitive {
            a.eq_ignore_ascii_case(&b)
        } else {
            a == b
        }
    };
    let mut i = 0;
    while i + m <= n {
        let mut j = 0;
        while j < m && cmp(line_chars[i + j], needle_chars[j]) {
            j += 1;
        }
        if j == m {
            out.push((i, i + m));
            i += m;
        } else {
            i += 1;
        }
    }
    out
}

pub fn draw(frame: &mut Frame<'_>, state: &FindState, editor_area: Rect) {
    let needs_height: u16 = if state.mode == FindMode::Replace {
        4
    } else {
        3
    };
    if editor_area.height < needs_height {
        return;
    }
    let area = Rect {
        x: editor_area.x,
        y: editor_area.y + editor_area.height - needs_height,
        width: editor_area.width,
        height: needs_height,
    };
    let case_label = if state.case_sensitive { "Aa" } else { "aA" };
    let status = state.status_label();
    let title = match state.mode {
        FindMode::Find => format!(
            " Find {case_label}  {status}  [Enter / F3 next \u{00b7} Shift+F3 prev \u{00b7} Alt+C case \u{00b7} Esc] "
        ),
        FindMode::Replace => format!(
            " Find/Replace {case_label}  {status}  [Tab field \u{00b7} Enter replace \u{00b7} Alt+A all \u{00b7} Esc] "
        ),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Yellow));

    let needle_active = state.mode == FindMode::Find || state.focus == ReplaceFocus::Needle;
    let replacement_active =
        state.mode == FindMode::Replace && state.focus == ReplaceFocus::Replacement;
    let active_caret = Span::styled("\u{2588}", Style::default().fg(Color::Yellow));

    let mut content_lines: Vec<Line<'static>> = Vec::with_capacity(2);
    let needle_label_style = if needle_active {
        Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let mut needle_spans: Vec<Span<'static>> = vec![
        Span::styled(" Find: ", needle_label_style),
        Span::styled(
            state.needle.clone(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ];
    if needle_active {
        needle_spans.push(active_caret.clone());
    }
    content_lines.push(Line::from(needle_spans));

    if state.mode == FindMode::Replace {
        let label_style = if replacement_active {
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray)
        };
        let mut spans: Vec<Span<'static>> = vec![
            Span::styled(" Repl: ", label_style),
            Span::styled(
                state.replacement.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
        ];
        if replacement_active {
            spans.push(active_caret);
        }
        content_lines.push(Line::from(spans));
    }

    let paragraph = Paragraph::new(content_lines).block(block);
    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn find_in_line_returns_each_occurrence() {
        let m = find_in_line("select * from t; SELECT 1; SeLeCt 2", "select", true);
        assert_eq!(m, vec![(0, 6), (17, 23), (27, 33)]);
    }

    #[test]
    fn find_in_line_case_sensitive_filters_out_mismatched_case() {
        let m = find_in_line("select Select SELECT", "Select", false);
        assert_eq!(m, vec![(7, 13)]);
    }

    #[test]
    fn find_in_line_does_not_overlap_matches() {
        let m = find_in_line("aaaa", "aa", true);
        // Non-overlapping: 0..2, 2..4. NOT 0,1,2.
        assert_eq!(m, vec![(0, 2), (2, 4)]);
    }

    #[test]
    fn recompute_populates_matches_for_each_line() {
        let mut s = FindState::with_needle("from".into(), false);
        let lines: Vec<String> = vec!["from a".into(), "no m here".into(), "and from b".into()];
        s.recompute(&lines);
        assert_eq!(
            s.matches,
            vec![
                (Cursor::new(0, 0), Cursor::new(0, 4)),
                (Cursor::new(2, 4), Cursor::new(2, 8)),
            ]
        );
        assert_eq!(s.active_idx, Some(0));
    }

    #[test]
    fn recompute_clears_when_needle_empty() {
        let mut s = FindState::new();
        s.recompute(&["any text".to_string()]);
        assert!(s.matches.is_empty());
        assert!(s.active_idx.is_none());
    }

    #[test]
    fn advance_wraps_around() {
        let mut s = FindState::with_needle("a".into(), false);
        s.recompute(&["aaa".to_string()]);
        assert_eq!(s.matches.len(), 3);
        s.active_idx = Some(2);
        assert_eq!(s.advance(), Some(Cursor::new(0, 0)));
    }

    #[test]
    fn retreat_wraps_to_last() {
        let mut s = FindState::with_needle("a".into(), false);
        s.recompute(&["aaa".to_string()]);
        s.active_idx = Some(0);
        assert_eq!(s.retreat(), Some(Cursor::new(0, 2)));
    }

    #[test]
    fn typing_a_char_extends_needle_and_jumps_to_first_match() {
        let lines = vec!["select".to_string()];
        let mut s = FindState::new();
        let out = handle_key(&mut s, k(KeyCode::Char('s'), KeyModifiers::NONE), &lines);
        assert_eq!(out, FindOutcome::JumpTo(Cursor::new(0, 0)));
        assert_eq!(s.needle, "s");
        assert_eq!(s.matches.len(), 1);
    }

    #[test]
    fn enter_advances_through_matches() {
        let lines = vec!["a a a".to_string()];
        let mut s = FindState::with_needle("a".into(), false);
        s.recompute(&lines);
        let out = handle_key(&mut s, k(KeyCode::Enter, KeyModifiers::NONE), &lines);
        assert_eq!(out, FindOutcome::JumpTo(Cursor::new(0, 2)));
    }

    #[test]
    fn esc_cancels() {
        let lines = vec!["x".to_string()];
        let mut s = FindState::with_needle("x".into(), false);
        s.recompute(&lines);
        assert_eq!(
            handle_key(&mut s, k(KeyCode::Esc, KeyModifiers::NONE), &lines),
            FindOutcome::Cancel
        );
    }

    #[test]
    fn alt_c_toggles_case_sensitivity() {
        let lines = vec!["Select select".to_string()];
        let mut s = FindState::with_needle("select".into(), false);
        s.recompute(&lines);
        assert_eq!(s.matches.len(), 2); // case-insensitive: both
        handle_key(&mut s, k(KeyCode::Char('c'), KeyModifiers::ALT), &lines);
        assert!(s.case_sensitive);
        assert_eq!(s.matches.len(), 1); // now only the lowercase one
    }

    #[test]
    fn status_label_formats_index_and_total() {
        let lines = vec!["abab".to_string()];
        let mut s = FindState::with_needle("ab".into(), false);
        s.recompute(&lines);
        assert_eq!(s.status_label(), "[1/2]");
        s.active_idx = Some(1);
        assert_eq!(s.status_label(), "[2/2]");
    }
}
