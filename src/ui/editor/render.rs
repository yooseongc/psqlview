//! Renders the editor pane: optional Block frame, line numbers in the
//! gutter, per-token syntax color, selection highlight, cursor-line
//! underline, bracket-pair reverse-video highlight, and a real terminal
//! caret via `Frame::set_cursor_position`.

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use super::bracket;
use super::buffer::{Cursor, TextBuffer};
use crate::ui::sql_lexer::{tokenize_line, LexState, TokenKind};

/// Pane-internal rendering state that survives across frames: the
/// vertical scroll offset (top visible row).
#[derive(Debug, Default, Clone, Copy)]
pub struct ViewState {
    pub scroll_top: usize,
}

impl ViewState {
    /// Adjusts `scroll_top` so the cursor is visible inside `inner_height`.
    /// Called from the draw pass before laying out lines.
    pub fn ensure_cursor_visible(&mut self, cursor: Cursor, inner_height: usize) {
        if inner_height == 0 {
            return;
        }
        if cursor.row < self.scroll_top {
            self.scroll_top = cursor.row;
        } else if cursor.row >= self.scroll_top + inner_height {
            self.scroll_top = cursor.row + 1 - inner_height;
        }
    }
}

pub fn draw(
    frame: &mut Frame<'_>,
    buf: &TextBuffer,
    view: &mut ViewState,
    block: Block<'_>,
    placeholder: Option<&str>,
    focused: bool,
    area: Rect,
) {
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    // Gutter width: enough digits for the largest line number.
    let total_lines = buf.line_count();
    let gutter_width = digits(total_lines.max(1)) + 1; // 1 space pad
    let gutter_width = gutter_width.min(inner.width as usize) as u16;
    let body_x = inner.x + gutter_width;
    let body_w = inner.width.saturating_sub(gutter_width);
    if body_w == 0 {
        return;
    }

    view.ensure_cursor_visible(buf.cursor(), inner.height as usize);

    let selection = buf.selection_range();
    let cursor = buf.cursor();
    // Bracket-pair highlight: only when the editor has focus, no selection
    // is active, and the cursor sits on a paired bracket. We require focus
    // because the highlight is anchored to the visible caret.
    let match_pair = if focused && selection.is_none() {
        bracket::find_match(buf, cursor).map(|m| (cursor, m))
    } else {
        None
    };

    // Empty + placeholder.
    if buf.is_empty() {
        if let Some(text) = placeholder {
            let p = Paragraph::new(Line::from(Span::styled(
                text.to_string(),
                Style::default().fg(Color::DarkGray),
            )));
            let phrect = Rect {
                x: body_x,
                y: inner.y,
                width: body_w,
                height: 1,
            };
            frame.render_widget(p, phrect);
        }
        if focused {
            frame.set_cursor_position(Position {
                x: body_x,
                y: inner.y,
            });
        }
        return;
    }

    let lines = buf.lines();
    let mut frame_lines: Vec<Line<'static>> = Vec::with_capacity(inner.height as usize);
    // Walk every line from the top so multi-line lex constructs (block
    // comments, strings spanning newlines) carry their state correctly
    // by the time we reach the scrolled-into-view region.
    let mut lex_state = LexState::default();
    for (row, raw) in lines.iter().enumerate() {
        if row < view.scroll_top {
            let _ = tokenize_line(raw, &mut lex_state);
            continue;
        }
        if row >= view.scroll_top + (inner.height as usize) {
            break;
        }
        let mut spans: Vec<Span<'static>> = Vec::new();

        // Gutter: line number, dimmed.
        spans.push(Span::styled(
            format!("{:>width$} ", row + 1, width = (gutter_width - 1) as usize),
            Style::default().fg(Color::DarkGray),
        ));

        // Body content: per-token color + selection bg + cursor-line underline
        // + bracket-pair highlight.
        push_styled_line(
            &mut spans,
            raw,
            row,
            &mut lex_state,
            selection,
            focused && row == cursor.row,
            match_pair,
        );

        frame_lines.push(Line::from(spans));
    }
    // If the buffer has fewer lines than the viewport, pad with blanks.
    while frame_lines.len() < inner.height as usize {
        frame_lines.push(Line::from(""));
    }

    let p = Paragraph::new(frame_lines);
    frame.render_widget(p, inner);

    // Real terminal caret at the cursor position. Skipped when not focused
    // so ratatui hides the cursor on the unfocused pane (matches the
    // pre-rewrite behavior with tui-textarea).
    if focused {
        if let Some((cx, cy)) = caret_screen_pos(buf, view, inner, gutter_width) {
            frame.set_cursor_position(Position { x: cx, y: cy });
        }
    }
}

/// Builds the content portion of a line: tokenizes for syntax color,
/// overlays the selection background, optionally adds underline for the
/// cursor line and reverse-video for matched brackets, and merges adjacent
/// same-style chars into Spans.
fn push_styled_line(
    spans: &mut Vec<Span<'static>>,
    line: &str,
    row: usize,
    lex_state: &mut LexState,
    selection: Option<(Cursor, Cursor)>,
    cursor_line: bool,
    match_pair: Option<(Cursor, Cursor)>,
) {
    let chars: Vec<char> = line.chars().collect();
    let styles = compute_line_styles(line, row, lex_state, selection, cursor_line, match_pair);
    let n = chars.len();
    let mut i = 0;
    while i < n {
        let s = styles[i];
        let start = i;
        while i < n && styles[i] == s {
            i += 1;
        }
        let text: String = chars[start..i].iter().collect();
        spans.push(Span::styled(text, s));
    }
}

/// Per-character style vector for one rendered line. Pure function so
/// it can be unit-tested without spinning up a ratatui frame.
fn compute_line_styles(
    line: &str,
    row: usize,
    lex_state: &mut LexState,
    selection: Option<(Cursor, Cursor)>,
    cursor_line: bool,
    match_pair: Option<(Cursor, Cursor)>,
) -> Vec<Style> {
    let n = line.chars().count();
    let default_style = Style::default().fg(Color::White);
    let mut styles: Vec<Style> = vec![default_style; n];

    // 1. Per-char foreground style from the tokenizer.
    let tokens = tokenize_line(line, lex_state);
    for tok in &tokens {
        let s = style_for_kind(tok.kind);
        let end = (tok.start_col + tok.len).min(n);
        for slot in &mut styles[tok.start_col..end] {
            *slot = s;
        }
    }

    // 2. Selection background.
    let (lo_opt, hi_opt) = match selection {
        Some((s, e)) => row_selection_range(row, s, e, n),
        None => (None, None),
    };
    if lo_opt.is_some() || hi_opt.is_some() {
        let lo = lo_opt.unwrap_or(0);
        let hi = hi_opt.unwrap_or(n);
        for slot in &mut styles[lo..hi.min(n)] {
            *slot = slot.bg(Color::DarkGray);
        }
    }

    // 3. Cursor-line underline (additive to whatever color the token had).
    if cursor_line {
        for slot in &mut styles {
            *slot = slot.add_modifier(Modifier::UNDERLINED);
        }
    }

    // 4. Bracket-pair highlight via reverse video — keeps the underlying
    // token color but flips fg/bg so the pair pops out regardless of theme.
    if let Some((a, b)) = match_pair {
        for pos in [a, b] {
            if pos.row == row && pos.col < n {
                styles[pos.col] = styles[pos.col].add_modifier(Modifier::REVERSED);
            }
        }
    }

    styles
}

fn style_for_kind(kind: TokenKind) -> Style {
    match kind {
        TokenKind::Keyword => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        TokenKind::StringLit => Style::default().fg(Color::Green),
        TokenKind::Number => Style::default().fg(Color::Yellow),
        TokenKind::LineComment | TokenKind::BlockComment => Style::default().fg(Color::DarkGray),
        TokenKind::QuotedIdent => Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::UNDERLINED),
        TokenKind::Identifier | TokenKind::Operator | TokenKind::Whitespace => {
            Style::default().fg(Color::White)
        }
    }
}

/// For a given visible row, returns the (lo, hi) char-column endpoints
/// of the selection portion that intersects this row.
/// `Some(col)` for a side that starts/ends exactly on this row, or
/// `None` if the selection extends past that side of the row.
/// Returns `(None, None)` if the row isn't touched by the selection.
fn row_selection_range(
    row: usize,
    start: Cursor,
    end: Cursor,
    line_chars: usize,
) -> (Option<usize>, Option<usize>) {
    if row < start.row || row > end.row {
        return (None, None);
    }
    let lo = if row == start.row {
        Some(start.col.min(line_chars))
    } else {
        Some(0)
    };
    let hi = if row == end.row {
        Some(end.col.min(line_chars))
    } else {
        // Past end of line — extend to logical EOL.
        Some(line_chars)
    };
    (lo, hi)
}

fn caret_screen_pos(
    buf: &TextBuffer,
    view: &ViewState,
    inner: Rect,
    gutter_width: u16,
) -> Option<(u16, u16)> {
    let cursor = buf.cursor();
    if cursor.row < view.scroll_top || cursor.row >= view.scroll_top + inner.height as usize {
        return None;
    }
    let line = buf.lines().get(cursor.row)?;
    let prefix: String = line.chars().take(cursor.col).collect();
    let col_width = UnicodeWidthStr::width(prefix.as_str()) as u16;
    let body_x = inner.x + gutter_width;
    let body_w = inner.width.saturating_sub(gutter_width);
    if body_w == 0 {
        return None;
    }
    let x = body_x + col_width.min(body_w.saturating_sub(1));
    let y = inner.y + (cursor.row - view.scroll_top) as u16;
    Some((x, y))
}

fn digits(mut n: usize) -> usize {
    let mut d = 0;
    while n > 0 {
        d += 1;
        n /= 10;
    }
    d.max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ensure_cursor_visible_scrolls_down() {
        let mut v = ViewState::default();
        v.ensure_cursor_visible(Cursor::new(20, 0), 10);
        assert_eq!(v.scroll_top, 11);
    }

    #[test]
    fn ensure_cursor_visible_scrolls_up() {
        let mut v = ViewState { scroll_top: 50 };
        v.ensure_cursor_visible(Cursor::new(40, 0), 10);
        assert_eq!(v.scroll_top, 40);
    }

    #[test]
    fn ensure_cursor_visible_noop_when_in_view() {
        let mut v = ViewState { scroll_top: 5 };
        v.ensure_cursor_visible(Cursor::new(7, 0), 10);
        assert_eq!(v.scroll_top, 5);
    }

    #[test]
    fn digits_handles_small_values() {
        assert_eq!(digits(0), 1);
        assert_eq!(digits(1), 1);
        assert_eq!(digits(9), 1);
        assert_eq!(digits(10), 2);
        assert_eq!(digits(999), 3);
    }

    #[test]
    fn row_selection_range_for_outside_row_is_empty() {
        let s = Cursor::new(2, 0);
        let e = Cursor::new(4, 1);
        assert_eq!(row_selection_range(0, s, e, 10), (None, None));
        assert_eq!(row_selection_range(5, s, e, 10), (None, None));
    }

    #[test]
    fn row_selection_range_inner_row_covers_full_line() {
        let s = Cursor::new(2, 1);
        let e = Cursor::new(4, 2);
        let (lo, hi) = row_selection_range(3, s, e, 8);
        assert_eq!((lo, hi), (Some(0), Some(8)));
    }

    #[test]
    fn bracket_match_marks_both_positions_reversed() {
        let line = "SELECT (a + b) FROM t";
        let mut lex = LexState::default();
        let pair = Some((Cursor::new(0, 7), Cursor::new(0, 13)));
        let styles = compute_line_styles(line, 0, &mut lex, None, false, pair);
        assert!(styles[7].add_modifier.contains(Modifier::REVERSED));
        assert!(styles[13].add_modifier.contains(Modifier::REVERSED));
        assert!(!styles[8].add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn bracket_match_only_applies_to_matching_row() {
        let line = "a)";
        let mut lex = LexState::default();
        // Pair spans rows 0 and 2; rendering row 2 should mark only col 0.
        let pair = Some((Cursor::new(0, 7), Cursor::new(2, 0)));
        let styles = compute_line_styles(line, 2, &mut lex, None, false, pair);
        assert!(styles[0].add_modifier.contains(Modifier::REVERSED));
        assert!(!styles[1].add_modifier.contains(Modifier::REVERSED));
    }

    #[test]
    fn bracket_match_none_leaves_styles_untouched() {
        let line = "SELECT 1";
        let mut lex = LexState::default();
        let with = compute_line_styles(line, 0, &mut lex, None, false, None);
        let mut lex2 = LexState::default();
        let pair = Some((Cursor::new(99, 0), Cursor::new(99, 1)));
        let without = compute_line_styles(line, 0, &mut lex2, None, false, pair);
        assert_eq!(with, without);
    }
}
