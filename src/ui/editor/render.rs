//! Renders the editor pane: optional Block frame, line numbers in the
//! gutter, cursor-line underline, selection highlight, and a real
//! terminal caret via `Frame::set_cursor_position`.
//!
//! R1 ships single-color text. R2 will replace `style_for_line` with
//! token-styled spans from the SQL lexer.

use ratatui::layout::{Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Paragraph};
use ratatui::Frame;
use unicode_width::UnicodeWidthStr;

use super::buffer::{Cursor, TextBuffer};

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
    for vis_row in 0..(inner.height as usize) {
        let row = view.scroll_top + vis_row;
        if row >= lines.len() {
            frame_lines.push(Line::from(""));
            continue;
        }
        let raw = &lines[row];
        let mut spans: Vec<Span<'static>> = Vec::new();

        // Gutter: line number, dimmed.
        spans.push(Span::styled(
            format!("{:>width$} ", row + 1, width = (gutter_width - 1) as usize),
            Style::default().fg(Color::DarkGray),
        ));

        // Body content with selection overlay.
        push_styled_line(&mut spans, raw, row, selection);

        let mut line = Line::from(spans);
        if focused && row == cursor.row {
            line.style = Style::default().add_modifier(Modifier::UNDERLINED);
        }
        frame_lines.push(line);
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

fn push_styled_line(
    spans: &mut Vec<Span<'static>>,
    line: &str,
    row: usize,
    selection: Option<(Cursor, Cursor)>,
) {
    // Selection overlay: split the line into pre / sel / post slices.
    let (sel_lo, sel_hi) = match selection {
        Some((s, e)) => row_selection_range(row, s, e, line.chars().count()),
        None => (None, None),
    };

    let chars: Vec<char> = line.chars().collect();
    let render_slice = |start: usize, end: usize, hi: bool, out: &mut Vec<Span<'static>>| {
        if start >= end {
            return;
        }
        let text: String = chars[start..end].iter().collect();
        let mut style = Style::default().fg(Color::White);
        if hi {
            style = style.bg(Color::DarkGray);
        }
        out.push(Span::styled(text, style));
    };

    let total = chars.len();
    match (sel_lo, sel_hi) {
        (Some(lo), Some(hi)) => {
            render_slice(0, lo, false, spans);
            render_slice(lo, hi, true, spans);
            render_slice(hi, total, false, spans);
        }
        (Some(lo), None) => {
            render_slice(0, lo, false, spans);
            render_slice(lo, total, true, spans);
        }
        (None, Some(hi)) => {
            render_slice(0, hi, true, spans);
            render_slice(hi, total, false, spans);
        }
        (None, None) => {
            render_slice(0, total, false, spans);
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
}
