//! Vim-style motions, decoupled from the surrounding `EditorState`.
//!
//! Pure functions over a `TextBuffer` — no mutation, no side effects.
//! `apply` walks the buffer once and returns the new cursor position;
//! the caller is responsible for jumping the buffer caret there.
//!
//! Word semantics follow vim's lowercase-`w` definition: characters
//! are partitioned into three classes (whitespace, identifier
//! [alphanumeric + `_`], punct), and a word boundary is any class
//! transition where neither side is whitespace, plus every
//! whitespace/non-whitespace boundary. Newlines count as whitespace,
//! so `w` jumps across line boundaries naturally.

use super::bracket;
use super::buffer::{Cursor, TextBuffer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    WordForward,
    WordBackward,
    WordEnd,
    LineStart,
    FirstNonBlank,
    LineEnd,
    MatchingBracket,
}

/// Compute the cursor position that results from applying `motion`
/// `count` times. `count == 0` is treated as `count == 1` so callers
/// can pass through their accumulated count without an extra `.max(1)`.
pub fn apply(buf: &TextBuffer, motion: Motion, count: usize) -> Cursor {
    let n = count.max(1);
    let cursor = buf.cursor();
    let lines = buf.lines();

    match motion {
        Motion::Left => {
            let col = cursor.col.saturating_sub(n);
            Cursor::new(cursor.row, col)
        }
        Motion::Right => {
            let len = line_len(lines, cursor.row);
            let col = (cursor.col + n).min(len);
            Cursor::new(cursor.row, col)
        }
        Motion::Up => {
            let row = cursor.row.saturating_sub(n);
            let col = cursor.col.min(line_len(lines, row));
            Cursor::new(row, col)
        }
        Motion::Down => {
            let last = lines.len().saturating_sub(1);
            let row = (cursor.row + n).min(last);
            let col = cursor.col.min(line_len(lines, row));
            Cursor::new(row, col)
        }
        Motion::LineStart => Cursor::new(cursor.row, 0),
        Motion::LineEnd => Cursor::new(cursor.row, line_len(lines, cursor.row)),
        Motion::FirstNonBlank => {
            let line = lines.get(cursor.row).map(|s| s.as_str()).unwrap_or("");
            let col = line.chars().take_while(|c| c.is_whitespace()).count();
            Cursor::new(cursor.row, col)
        }
        Motion::WordForward => repeat(n, cursor, |c| word_forward_step(lines, c)),
        Motion::WordBackward => repeat(n, cursor, |c| word_backward_step(lines, c)),
        Motion::WordEnd => repeat(n, cursor, |c| word_end_step(lines, c)),
        Motion::MatchingBracket => bracket::find_match(buf, cursor).unwrap_or(cursor),
    }
}

fn repeat<F: Fn(Cursor) -> Cursor>(n: usize, init: Cursor, step: F) -> Cursor {
    let mut c = init;
    for _ in 0..n {
        let next = step(c);
        if next == c {
            return c;
        }
        c = next;
    }
    c
}

fn line_len(lines: &[String], row: usize) -> usize {
    lines.get(row).map(|l| l.chars().count()).unwrap_or(0)
}

fn class(c: char) -> u8 {
    if c.is_whitespace() {
        0
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// Class of the char at this cursor position. Positions past line
/// end (col == line_len) are treated as whitespace (representing the
/// implicit newline) so word motions cross line boundaries cleanly.
fn class_at(lines: &[String], c: Cursor) -> u8 {
    let len = line_len(lines, c.row);
    if c.col < len {
        let line = match lines.get(c.row) {
            Some(l) => l,
            None => return 0,
        };
        line.chars().nth(c.col).map(class).unwrap_or(0)
    } else {
        0
    }
}

/// Step one logical position forward; `Some(end-of-buffer)` returns `None`.
/// Position `(row, line_len)` is the "newline slot" — stepping from there
/// lands on `(row+1, 0)`.
fn step_forward(lines: &[String], c: Cursor) -> Option<Cursor> {
    let len = line_len(lines, c.row);
    if c.col < len {
        Some(Cursor::new(c.row, c.col + 1))
    } else if c.row + 1 < lines.len() {
        Some(Cursor::new(c.row + 1, 0))
    } else {
        None
    }
}

fn step_backward(lines: &[String], c: Cursor) -> Option<Cursor> {
    if c.col > 0 {
        Some(Cursor::new(c.row, c.col - 1))
    } else if c.row > 0 {
        let prev = c.row - 1;
        Some(Cursor::new(prev, line_len(lines, prev)))
    } else {
        None
    }
}

fn word_forward_step(lines: &[String], cursor: Cursor) -> Cursor {
    let mut c = cursor;
    let cur_class = class_at(lines, c);
    if cur_class != 0 {
        // Skip current run of same class.
        while let Some(next) = step_forward(lines, c) {
            if class_at(lines, next) != cur_class {
                c = next;
                break;
            }
            c = next;
        }
    }
    // Skip whitespace until a non-ws char (or end-of-buffer).
    while class_at(lines, c) == 0 {
        match step_forward(lines, c) {
            Some(next) => c = next,
            None => return c,
        }
    }
    c
}

fn word_backward_step(lines: &[String], cursor: Cursor) -> Cursor {
    let Some(mut c) = step_backward(lines, cursor) else {
        return cursor;
    };
    // Skip whitespace going back.
    while class_at(lines, c) == 0 {
        match step_backward(lines, c) {
            Some(prev) => c = prev,
            None => return c,
        }
    }
    let cur_class = class_at(lines, c);
    loop {
        let Some(prev) = step_backward(lines, c) else {
            return c;
        };
        if class_at(lines, prev) == cur_class {
            c = prev;
        } else {
            return c;
        }
    }
}

fn word_end_step(lines: &[String], cursor: Cursor) -> Cursor {
    let mut c = cursor;
    let cur_class = class_at(lines, c);
    let next_class = step_forward(lines, c)
        .map(|n| class_at(lines, n))
        .unwrap_or(0);
    // If the cursor sits on whitespace OR at the end of a run, advance
    // one slot first; otherwise stay so we walk to the run's end.
    if cur_class == 0 || cur_class != next_class {
        c = match step_forward(lines, c) {
            Some(next) => next,
            None => return c,
        };
    }
    // Skip whitespace.
    while class_at(lines, c) == 0 {
        c = match step_forward(lines, c) {
            Some(next) => next,
            None => return c,
        };
    }
    // Walk forward through the run.
    let cur_class = class_at(lines, c);
    loop {
        let Some(next) = step_forward(lines, c) else {
            return c;
        };
        if class_at(lines, next) == cur_class {
            c = next;
        } else {
            return c;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf_at(text: &str, row: usize, col: usize) -> TextBuffer {
        let mut b = TextBuffer::from_text(text);
        b.set_cursor(row, col);
        b
    }

    #[test]
    fn left_saturates_at_column_zero() {
        let b = buf_at("hello", 0, 2);
        assert_eq!(apply(&b, Motion::Left, 5), Cursor::new(0, 0));
    }

    #[test]
    fn right_saturates_at_line_end() {
        let b = buf_at("hello", 0, 0);
        assert_eq!(apply(&b, Motion::Right, 99), Cursor::new(0, 5));
    }

    #[test]
    fn down_clamps_column_when_target_is_shorter() {
        let b = buf_at("longer\nshort", 0, 5);
        assert_eq!(apply(&b, Motion::Down, 1), Cursor::new(1, 5));
    }

    #[test]
    fn up_with_count_jumps_multiple_rows() {
        let b = buf_at("a\nb\nc\nd", 3, 0);
        assert_eq!(apply(&b, Motion::Up, 2), Cursor::new(1, 0));
    }

    #[test]
    fn line_start_and_line_end_for_current_row() {
        let b = buf_at("  hello", 0, 4);
        assert_eq!(apply(&b, Motion::LineStart, 1), Cursor::new(0, 0));
        assert_eq!(apply(&b, Motion::LineEnd, 1), Cursor::new(0, 7));
    }

    #[test]
    fn first_non_blank_skips_leading_whitespace() {
        let b = buf_at("    hello", 0, 0);
        assert_eq!(apply(&b, Motion::FirstNonBlank, 1), Cursor::new(0, 4));
    }

    #[test]
    fn word_forward_lands_on_next_word_start() {
        let b = buf_at("foo bar baz", 0, 0);
        assert_eq!(apply(&b, Motion::WordForward, 1), Cursor::new(0, 4));
        assert_eq!(apply(&b, Motion::WordForward, 2), Cursor::new(0, 8));
    }

    #[test]
    fn word_forward_treats_punct_as_separate_word() {
        let b = buf_at("foo.bar", 0, 0);
        // 'foo' run → '.' is punct (class 2), separate word.
        assert_eq!(apply(&b, Motion::WordForward, 1), Cursor::new(0, 3));
        assert_eq!(apply(&b, Motion::WordForward, 2), Cursor::new(0, 4));
    }

    #[test]
    fn word_forward_crosses_newline() {
        let b = buf_at("foo\nbar", 0, 0);
        assert_eq!(apply(&b, Motion::WordForward, 1), Cursor::new(1, 0));
    }

    #[test]
    fn word_backward_from_word_middle_lands_on_word_start() {
        let b = buf_at("foo bar", 0, 5);
        assert_eq!(apply(&b, Motion::WordBackward, 1), Cursor::new(0, 4));
        assert_eq!(apply(&b, Motion::WordBackward, 2), Cursor::new(0, 0));
    }

    #[test]
    fn word_end_from_word_start_lands_on_word_end() {
        let b = buf_at("foo bar", 0, 0);
        assert_eq!(apply(&b, Motion::WordEnd, 1), Cursor::new(0, 2));
        assert_eq!(apply(&b, Motion::WordEnd, 2), Cursor::new(0, 6));
    }

    #[test]
    fn matching_bracket_finds_close_paren() {
        let b = buf_at("(foo)", 0, 0);
        assert_eq!(apply(&b, Motion::MatchingBracket, 1), Cursor::new(0, 4));
    }

    #[test]
    fn matching_bracket_returns_cursor_when_not_on_pair() {
        let b = buf_at("foo bar", 0, 1);
        assert_eq!(apply(&b, Motion::MatchingBracket, 1), Cursor::new(0, 1));
    }

    #[test]
    fn count_zero_treated_as_one() {
        let b = buf_at("foo bar", 0, 0);
        assert_eq!(
            apply(&b, Motion::WordForward, 0),
            apply(&b, Motion::WordForward, 1)
        );
    }

    #[test]
    fn count_overflow_saturates_to_buffer_bounds() {
        let b = buf_at("a b c", 0, 0);
        let last = apply(&b, Motion::WordForward, 999);
        // No matter the count, end-of-buffer cursor is the position past
        // the last char on the last line.
        assert_eq!(last.row, 0);
        assert!(last.col >= 4);
    }
}
