//! Interactive substitute confirm modal — vim's `:s/pat/repl/c`.
//!
//! Walks forward through the buffer applying `pattern → replacement`
//! one match at a time. Each match prompts the user with
//! `(y)es / (n)o / (a)ll-rest / (q)uit`. Cursor jumps to the active
//! match so the user can see what's about to change. The replacement
//! itself doesn't get re-scanned (vim semantics: `s/foo/foofoo/c`
//! doesn't loop forever).

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::ui::editor::buffer::Cursor;
use crate::ui::find::match_engine::find_in_line;

#[derive(Debug)]
pub struct SubstituteState {
    pub pattern: String,
    pub replacement: String,
    pub case_sensitive: bool,
    /// Lower-bound cursor for the next-match search. Walks forward as
    /// the user accepts/skips. Always points just past the last
    /// confirmed range (or the original cursor at start).
    pub from: Cursor,
    /// `Some(row)` when the substitute is current-line scoped
    /// (`:s/...`); `None` for whole-buffer (`:%s/...`).
    pub restrict_row: Option<usize>,
    /// Cached "current match" — recomputed from `from` each time the
    /// state advances. `None` means we've reached the end.
    current: Option<(Cursor, Cursor)>,
    pub replaced: usize,
    pub skipped: usize,
}

impl SubstituteState {
    pub fn new(
        pattern: String,
        replacement: String,
        case_sensitive: bool,
        cursor: Cursor,
        restrict_row: Option<usize>,
        lines: &[String],
    ) -> Self {
        let from = match restrict_row {
            // Current-line scope: walk from start of the cursor's line
            // (matching vim's `:s` semantics — replaces all matches on
            // the current line, not just from the cursor onward).
            Some(r) => Cursor::new(r, 0),
            None => cursor,
        };
        let mut s = Self {
            pattern,
            replacement,
            case_sensitive,
            from,
            restrict_row,
            current: None,
            replaced: 0,
            skipped: 0,
        };
        s.refresh(lines);
        s
    }

    pub fn current(&self) -> Option<(Cursor, Cursor)> {
        self.current
    }

    pub fn done(&self) -> bool {
        self.current.is_none()
    }

    /// Caller has just applied the replacement to the buffer. Walk
    /// `from` past the replacement and re-scan for the next match.
    pub fn after_accept(&mut self, lines: &[String]) {
        if let Some((start, _)) = self.current {
            let new_col = start.col + self.replacement.chars().count();
            self.from = Cursor::new(start.row, new_col);
            self.replaced += 1;
            self.refresh(lines);
        }
    }

    /// Skip the active match — advance `from` past the match end and
    /// re-scan.
    pub fn after_skip(&mut self, lines: &[String]) {
        if let Some((_, end)) = self.current {
            self.from = end;
            self.skipped += 1;
            self.refresh(lines);
        }
    }

    fn refresh(&mut self, lines: &[String]) {
        self.current = find_next(
            lines,
            &self.pattern,
            self.case_sensitive,
            self.from,
            self.restrict_row,
        );
    }
}

fn find_next(
    lines: &[String],
    pattern: &str,
    case_sensitive: bool,
    from: Cursor,
    restrict_row: Option<usize>,
) -> Option<(Cursor, Cursor)> {
    if let Some(r) = restrict_row {
        if from.row > r {
            return None;
        }
        let line = lines.get(r)?;
        for (s, e) in find_in_line(line, pattern, !case_sensitive) {
            if s >= from.col {
                return Some((Cursor::new(r, s), Cursor::new(r, e)));
            }
        }
        None
    } else {
        for row in from.row..lines.len() {
            let line = lines.get(row)?;
            for (s, e) in find_in_line(line, pattern, !case_sensitive) {
                if row > from.row || s >= from.col {
                    return Some((Cursor::new(row, s), Cursor::new(row, e)));
                }
            }
        }
        None
    }
}

/// Renders the prompt at the bottom of the editor area. Single line:
/// `replace 'pat' \u{2192} 'repl'? (y/n/a/q)  [N replaced, M skipped]`.
pub fn draw(frame: &mut Frame<'_>, state: &SubstituteState, editor_area: Rect) {
    if editor_area.height < 3 {
        return;
    }
    let area = Rect {
        x: editor_area.x,
        y: editor_area.y + editor_area.height - 3,
        width: editor_area.width,
        height: 3,
    };
    let body = format!(
        " replace '{}' \u{2192} '{}'? (y/n/a/q)  [{} replaced, {} skipped] ",
        state.pattern, state.replacement, state.replaced, state.skipped,
    );
    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Substitute confirm ")
        .border_style(Style::default().fg(Color::Yellow));
    let line = Line::from(Span::styled(
        body,
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ));
    let p = Paragraph::new(line).block(block);
    frame.render_widget(Clear, area);
    frame.render_widget(p, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_match_starts_from_cursor_in_all_lines_scope() {
        let lines = vec!["foo bar foo".to_string(), "baz foo".to_string()];
        let s = SubstituteState::new(
            "foo".into(),
            "BAR".into(),
            false,
            Cursor::new(0, 5),
            None,
            &lines,
        );
        // First match >= (0, 5) is at (0, 8).
        assert_eq!(s.current(), Some((Cursor::new(0, 8), Cursor::new(0, 11))));
    }

    #[test]
    fn current_line_scope_starts_at_col_zero_of_cursor_row() {
        let lines = vec!["foo".to_string(), "bar foo".to_string()];
        let s = SubstituteState::new(
            "foo".into(),
            "X".into(),
            false,
            Cursor::new(1, 5),
            Some(1),
            &lines,
        );
        // Current-line scope: walk from (1, 0). First foo on row 1 is at col 4.
        assert_eq!(s.current(), Some((Cursor::new(1, 4), Cursor::new(1, 7))));
    }

    #[test]
    fn after_accept_walks_past_replacement_text() {
        // Buffer pre-replacement.
        let mut lines = vec!["foo foo foo".to_string()];
        let mut s = SubstituteState::new(
            "foo".into(),
            "BAR".into(),
            false,
            Cursor::new(0, 0),
            None,
            &lines,
        );
        // First match: (0,0)..(0,3). Apply.
        let (start, end) = s.current().unwrap();
        let mut row = lines[0].clone();
        row.replace_range(start.col..end.col, &s.replacement);
        lines[0] = row;
        // After accept: from should advance past replacement (col 3
        // since "BAR".len() == "foo".len() in chars). Next match is
        // at (0, 4).
        s.after_accept(&lines);
        assert_eq!(s.current(), Some((Cursor::new(0, 4), Cursor::new(0, 7))));
        assert_eq!(s.replaced, 1);
    }

    #[test]
    fn after_skip_advances_past_match_without_replacing() {
        let lines = vec!["foo foo".to_string()];
        let mut s = SubstituteState::new(
            "foo".into(),
            "X".into(),
            false,
            Cursor::new(0, 0),
            None,
            &lines,
        );
        // Skip first foo: from advances to col 3, next match at col 4.
        s.after_skip(&lines);
        assert_eq!(s.current(), Some((Cursor::new(0, 4), Cursor::new(0, 7))));
        assert_eq!(s.skipped, 1);
        assert_eq!(s.replaced, 0);
    }

    #[test]
    fn done_when_no_more_matches() {
        let lines = vec!["alpha beta".to_string()];
        let mut s = SubstituteState::new(
            "zzz".into(),
            "X".into(),
            false,
            Cursor::new(0, 0),
            None,
            &lines,
        );
        assert!(s.done());
        // After-accept on empty current is a no-op.
        s.after_accept(&lines);
        assert_eq!(s.replaced, 0);
    }

    #[test]
    fn replacement_containing_pattern_does_not_loop() {
        // "foo" → "foofoo" — naive recompute would re-find the new
        // "foo" at col 0 of the replacement and loop forever.
        // after_accept walks `from` past the inserted "foofoo", so the
        // next match search starts after the replacement.
        let mut lines = vec!["foo bar".to_string()];
        let mut s = SubstituteState::new(
            "foo".into(),
            "foofoo".into(),
            false,
            Cursor::new(0, 0),
            None,
            &lines,
        );
        // Accept the first foo.
        let (start, end) = s.current().unwrap();
        let mut row = lines[0].clone();
        row.replace_range(start.col..end.col, &s.replacement);
        lines[0] = row;
        s.after_accept(&lines);
        // No more "foo" past col 6 in "foofoo bar" → done.
        assert!(s.done());
        assert_eq!(s.replaced, 1);
    }

    #[test]
    fn current_line_scope_ignores_other_rows() {
        let lines = vec!["foo".to_string(), "foo".to_string(), "foo".to_string()];
        let mut s = SubstituteState::new(
            "foo".into(),
            "X".into(),
            false,
            Cursor::new(1, 0),
            Some(1),
            &lines,
        );
        // First (and only) match is (1, 0).
        assert_eq!(s.current(), Some((Cursor::new(1, 0), Cursor::new(1, 3))));
        // Skip it — no more on row 1.
        s.after_skip(&lines);
        assert!(s.done());
    }
}
