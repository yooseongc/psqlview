//! Bracket-pair finder for the editor's match highlight.
//!
//! Given a cursor sitting on `(`, `)`, `[`, `]`, `{`, or `}`, walk the
//! buffer (skipping string and comment regions) to locate the matching
//! bracket. Returns `None` if the cursor isn't on a bracket or no match
//! exists in the visible buffer.

use super::buffer::{Cursor, TextBuffer};
use crate::ui::sql_lexer::{tokenize_line, LexState, TokenKind};

#[derive(Debug, Clone, Copy)]
struct Pair {
    open: char,
    close: char,
}

const PAIRS: &[Pair] = &[
    Pair {
        open: '(',
        close: ')',
    },
    Pair {
        open: '[',
        close: ']',
    },
    Pair {
        open: '{',
        close: '}',
    },
];

fn classify(c: char) -> Option<(Pair, bool)> {
    for p in PAIRS {
        if c == p.open {
            return Some((*p, true));
        }
        if c == p.close {
            return Some((*p, false));
        }
    }
    None
}

/// Locates the bracket that matches the one under the cursor. Returns
/// `None` if the cursor is not on a bracket or no match exists.
pub fn find_match(buf: &TextBuffer, cursor: Cursor) -> Option<Cursor> {
    let lines = buf.lines();
    let line = lines.get(cursor.row)?;
    let line_chars: Vec<char> = line.chars().collect();
    let c = *line_chars.get(cursor.col)?;
    let (pair, forward) = classify(c)?;

    let skip = build_skip_map(lines);
    if is_skipped(&skip, cursor.row, cursor.col) {
        // The cursor is sitting on a bracket inside a string or comment;
        // we don't try to pair it with anything outside that region.
        return None;
    }

    if forward {
        scan_forward(lines, &skip, cursor, pair)
    } else {
        scan_backward(lines, &skip, cursor, pair)
    }
}

fn scan_forward(lines: &[String], skip: &[Vec<bool>], start: Cursor, pair: Pair) -> Option<Cursor> {
    let mut depth = 1i32;
    let mut row = start.row;
    let mut col = start.col + 1;
    loop {
        let line = lines.get(row)?;
        let chars: Vec<char> = line.chars().collect();
        while col < chars.len() {
            if !is_skipped(skip, row, col) {
                let ch = chars[col];
                if ch == pair.open {
                    depth += 1;
                } else if ch == pair.close {
                    depth -= 1;
                    if depth == 0 {
                        return Some(Cursor::new(row, col));
                    }
                }
            }
            col += 1;
        }
        row += 1;
        col = 0;
        if row >= lines.len() {
            return None;
        }
    }
}

fn scan_backward(
    lines: &[String],
    skip: &[Vec<bool>],
    start: Cursor,
    pair: Pair,
) -> Option<Cursor> {
    let mut depth = 1i32;
    let mut row = start.row as isize;
    let mut col = start.col as isize - 1;
    loop {
        if row < 0 {
            return None;
        }
        let line = &lines[row as usize];
        let chars: Vec<char> = line.chars().collect();
        while col >= 0 {
            let c = col as usize;
            if !is_skipped(skip, row as usize, c) {
                let ch = chars[c];
                if ch == pair.close {
                    depth += 1;
                } else if ch == pair.open {
                    depth -= 1;
                    if depth == 0 {
                        return Some(Cursor::new(row as usize, c));
                    }
                }
            }
            col -= 1;
        }
        row -= 1;
        if row >= 0 {
            col = lines[row as usize].chars().count() as isize - 1;
        }
    }
}

/// Builds a per-line, per-char bitmap of "this position is inside a
/// string / comment". Walks the lexer once over the entire buffer.
fn build_skip_map(lines: &[String]) -> Vec<Vec<bool>> {
    let mut state = LexState::default();
    let mut out = Vec::with_capacity(lines.len());
    for line in lines {
        let n = line.chars().count();
        let toks = tokenize_line(line, &mut state);
        let mut row = vec![false; n];
        for t in &toks {
            if matches!(
                t.kind,
                TokenKind::StringLit | TokenKind::LineComment | TokenKind::BlockComment
            ) {
                let end = (t.start_col + t.len).min(n);
                for slot in &mut row[t.start_col..end] {
                    *slot = true;
                }
            }
        }
        out.push(row);
    }
    out
}

fn is_skipped(skip: &[Vec<bool>], row: usize, col: usize) -> bool {
    skip.get(row)
        .and_then(|r| r.get(col))
        .copied()
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn match_paren_same_line() {
        let buf = TextBuffer::from_text("SELECT (a + b) FROM t");
        let open = Cursor::new(0, 7); // '(' at col 7
        let close = find_match(&buf, open).expect("close found");
        assert_eq!(close, Cursor::new(0, 13));
        let back = find_match(&buf, close).expect("open found");
        assert_eq!(back, open);
    }

    #[test]
    fn match_nested_paren() {
        let buf = TextBuffer::from_text("((a))");
        let outer_open = Cursor::new(0, 0);
        assert_eq!(find_match(&buf, outer_open), Some(Cursor::new(0, 4)));
        let inner_open = Cursor::new(0, 1);
        assert_eq!(find_match(&buf, inner_open), Some(Cursor::new(0, 3)));
    }

    #[test]
    fn match_paren_across_lines() {
        let buf = TextBuffer::from_text("SELECT (\na\n)");
        let open = Cursor::new(0, 7);
        assert_eq!(find_match(&buf, open), Some(Cursor::new(2, 0)));
    }

    #[test]
    fn no_match_when_unbalanced() {
        let buf = TextBuffer::from_text("(((");
        assert_eq!(find_match(&buf, Cursor::new(0, 0)), None);
    }

    #[test]
    fn brackets_inside_strings_are_ignored() {
        // The '(' inside the string literal must not pair with the
        // outer ')'.
        let buf = TextBuffer::from_text("SELECT '(' || foo)");
        // Outer ')' at end.
        let close = Cursor::new(0, 17);
        // Looking back, there is no bare '(' to match.
        assert_eq!(find_match(&buf, close), None);
    }

    #[test]
    fn brackets_inside_line_comments_are_ignored() {
        let buf = TextBuffer::from_text("(a) -- ( inside comment");
        assert_eq!(find_match(&buf, Cursor::new(0, 0)), Some(Cursor::new(0, 2)));
    }

    #[test]
    fn cursor_not_on_bracket_returns_none() {
        let buf = TextBuffer::from_text("SELECT 1");
        assert_eq!(find_match(&buf, Cursor::new(0, 0)), None);
    }

    #[test]
    fn cursor_inside_string_on_bracket_returns_none() {
        // '(' is inside the string; we don't try to pair it.
        let buf = TextBuffer::from_text("'(' || ')'");
        assert_eq!(find_match(&buf, Cursor::new(0, 1)), None);
    }

    #[test]
    fn brackets_match_across_kinds_independently() {
        let buf = TextBuffer::from_text("(a [b] c)");
        // '(' at 0 must skip the '[]' inside and pair with ')' at 8.
        assert_eq!(find_match(&buf, Cursor::new(0, 0)), Some(Cursor::new(0, 8)));
        // '[' at 3 pairs with ']' at 5.
        assert_eq!(find_match(&buf, Cursor::new(0, 3)), Some(Cursor::new(0, 5)));
    }
}
