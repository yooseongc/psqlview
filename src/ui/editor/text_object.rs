//! Vim-style text objects for operator-pending mode.
//!
//! Each function takes the buffer + cursor position + scope (Inner /
//! Around) and returns the half-open `[start, end)` range of chars
//! the operator should consume, or `None` when the cursor isn't
//! sitting inside (or adjacent to) a usable target.
//!
//! Word vs WORD follows vim's distinction:
//! - **word (small)**: same `class()` partition the motion module
//!   uses (whitespace / identifier / punct). Two adjacent runs of
//!   different non-whitespace classes are separate words.
//! - **WORD (big)**: whitespace-bounded. `schema.table` is one WORD
//!   but two small words; useful in SQL contexts where dotted
//!   identifiers should travel together.

use super::buffer::{Cursor, TextBuffer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `iw`-style — content only.
    Inner,
    /// `aw`-style — content + one side of surrounding whitespace /
    /// delimiter, matching vim's "around" semantics.
    Around,
}

/// Word object. `big = true` means WORD (whitespace-bounded); otherwise
/// the small-word class partition applies. `Around` includes one trailing
/// whitespace run (or the leading run if there's no trailing one).
pub fn word(lines: &[String], cursor: Cursor, scope: Scope, big: bool) -> Option<(Cursor, Cursor)> {
    let line = lines.get(cursor.row)?;
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let n = chars.len();
    let col = cursor.col.min(n.saturating_sub(1));
    let class_at = |i: usize| -> u8 { class(chars[i], big) };
    if class_at(col) == 0 {
        // Cursor on whitespace: vim still selects an "around-whitespace"
        // span, but for simplicity we treat this as no-op.
        return None;
    }
    let cur_class = class_at(col);
    // Walk left while same class.
    let mut start = col;
    while start > 0 && class_at(start - 1) == cur_class {
        start -= 1;
    }
    // Walk right while same class.
    let mut end_inclusive = col;
    while end_inclusive + 1 < n && class_at(end_inclusive + 1) == cur_class {
        end_inclusive += 1;
    }
    let mut end = end_inclusive + 1; // exclusive
    if matches!(scope, Scope::Around) {
        // Try trailing whitespace first; if none, take leading.
        let trailing_start = end;
        let mut trailing_end = trailing_start;
        while trailing_end < n && chars[trailing_end].is_whitespace() {
            trailing_end += 1;
        }
        if trailing_end > trailing_start {
            end = trailing_end;
        } else {
            // No trailing — extend `start` left across whitespace.
            while start > 0 && chars[start - 1].is_whitespace() {
                start -= 1;
            }
        }
    }
    Some((Cursor::new(cursor.row, start), Cursor::new(cursor.row, end)))
}

/// Quoted-string object. Looks for the nearest pair of `quote_char`
/// that *bracket* the cursor on the current line. Inner = chars
/// strictly between the quotes; Around = inclusive of the quotes.
/// Multi-line strings are not supported (matches vim's `i"`).
pub fn quote(
    lines: &[String],
    cursor: Cursor,
    scope: Scope,
    quote_char: char,
) -> Option<(Cursor, Cursor)> {
    let line = lines.get(cursor.row)?;
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let col = cursor.col.min(chars.len().saturating_sub(1));
    // If cursor sits on a quote, treat the *first* pair as
    // (cursor, next quote on the line).
    let positions: Vec<usize> = chars
        .iter()
        .enumerate()
        .filter_map(|(i, c)| (*c == quote_char).then_some(i))
        .collect();
    // Pair quotes left-to-right (1st with 2nd, 3rd with 4th, ...) and
    // pick the pair that brackets the cursor.
    let (lo, hi) = positions.chunks_exact(2).find_map(|pair| {
        let (lo, hi) = (pair[0], pair[1]);
        (lo <= col && col <= hi).then_some((lo, hi))
    })?;
    let (s, e) = match scope {
        Scope::Inner => (lo + 1, hi),
        Scope::Around => (lo, hi + 1),
    };
    Some((Cursor::new(cursor.row, s), Cursor::new(cursor.row, e)))
}

/// Paren object — the smallest pair of `(`/`)` that brackets the
/// cursor (line-local for simplicity; vim allows multi-line, but the
/// common SQL case is on one line).
pub fn paren(lines: &[String], cursor: Cursor, scope: Scope) -> Option<(Cursor, Cursor)> {
    bracket_pair(lines, cursor, scope, '(', ')')
}

fn bracket_pair(
    lines: &[String],
    cursor: Cursor,
    scope: Scope,
    open: char,
    close: char,
) -> Option<(Cursor, Cursor)> {
    let line = lines.get(cursor.row)?;
    let chars: Vec<char> = line.chars().collect();
    if chars.is_empty() {
        return None;
    }
    let col = cursor.col.min(chars.len().saturating_sub(1));
    // Walk left to find the matching open paren (track depth).
    let mut depth = 0i32;
    let mut open_idx: Option<usize> = None;
    for i in (0..=col).rev() {
        if chars[i] == close {
            depth += 1;
        } else if chars[i] == open {
            if depth == 0 {
                open_idx = Some(i);
                break;
            }
            depth -= 1;
        }
    }
    let open_idx = open_idx?;
    // Walk right to find the matching close.
    let mut depth = 0i32;
    let mut close_idx: Option<usize> = None;
    for (i, ch) in chars.iter().enumerate().skip(open_idx + 1) {
        if *ch == open {
            depth += 1;
        } else if *ch == close {
            if depth == 0 {
                close_idx = Some(i);
                break;
            }
            depth -= 1;
        }
    }
    let close_idx = close_idx?;
    let (s, e) = match scope {
        Scope::Inner => (open_idx + 1, close_idx),
        Scope::Around => (open_idx, close_idx + 1),
    };
    Some((Cursor::new(cursor.row, s), Cursor::new(cursor.row, e)))
}

fn class(c: char, big: bool) -> u8 {
    if c.is_whitespace() {
        0
    } else if big {
        // WORD: anything non-whitespace is class 1.
        1
    } else if c.is_alphanumeric() || c == '_' {
        1
    } else {
        2
    }
}

/// Convenience wrapper used by the operator-pending dispatcher.
pub fn resolve(buf: &TextBuffer, scope: Scope, obj_char: char) -> Option<(Cursor, Cursor)> {
    let cursor = buf.cursor();
    let lines = buf.lines();
    match obj_char {
        'w' => word(lines, cursor, scope, false),
        'W' => word(lines, cursor, scope, true),
        '"' => quote(lines, cursor, scope, '"'),
        '\'' => quote(lines, cursor, scope, '\''),
        '(' | ')' => paren(lines, cursor, scope),
        _ => None,
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
    fn iw_selects_current_small_word() {
        let b = buf_at("foo bar baz", 0, 5);
        let r = word(b.lines(), b.cursor(), Scope::Inner, false).unwrap();
        assert_eq!(r, (Cursor::new(0, 4), Cursor::new(0, 7)));
    }

    #[test]
    fn iw_treats_dot_as_word_boundary() {
        let b = buf_at("schema.table", 0, 2);
        let r = word(b.lines(), b.cursor(), Scope::Inner, false).unwrap();
        // Small-word: just "schema".
        assert_eq!(r, (Cursor::new(0, 0), Cursor::new(0, 6)));
    }

    #[test]
    fn iw_big_word_includes_dotted_identifier() {
        let b = buf_at("schema.table", 0, 2);
        let r = word(b.lines(), b.cursor(), Scope::Inner, true).unwrap();
        assert_eq!(r, (Cursor::new(0, 0), Cursor::new(0, 12)));
    }

    #[test]
    fn aw_includes_trailing_whitespace() {
        let b = buf_at("foo bar baz", 0, 0);
        let r = word(b.lines(), b.cursor(), Scope::Around, false).unwrap();
        assert_eq!(r, (Cursor::new(0, 0), Cursor::new(0, 4)));
    }

    #[test]
    fn aw_falls_back_to_leading_whitespace_at_eol() {
        let b = buf_at("foo bar", 0, 5);
        let r = word(b.lines(), b.cursor(), Scope::Around, false).unwrap();
        // No trailing ws — extends start across leading space.
        assert_eq!(r, (Cursor::new(0, 3), Cursor::new(0, 7)));
    }

    #[test]
    fn iw_returns_none_on_whitespace() {
        let b = buf_at("foo  bar", 0, 3);
        assert!(word(b.lines(), b.cursor(), Scope::Inner, false).is_none());
    }

    #[test]
    fn quote_inner_excludes_delimiters() {
        let b = buf_at("a \"foo\" b", 0, 4);
        let r = quote(b.lines(), b.cursor(), Scope::Inner, '"').unwrap();
        assert_eq!(r, (Cursor::new(0, 3), Cursor::new(0, 6)));
    }

    #[test]
    fn quote_around_includes_delimiters() {
        let b = buf_at("a \"foo\" b", 0, 4);
        let r = quote(b.lines(), b.cursor(), Scope::Around, '"').unwrap();
        assert_eq!(r, (Cursor::new(0, 2), Cursor::new(0, 7)));
    }

    #[test]
    fn quote_returns_none_when_no_pair() {
        let b = buf_at("no quotes here", 0, 0);
        assert!(quote(b.lines(), b.cursor(), Scope::Inner, '"').is_none());
    }

    #[test]
    fn paren_inner_finds_chars_inside_parens() {
        let b = buf_at("foo(bar)baz", 0, 5);
        let r = paren(b.lines(), b.cursor(), Scope::Inner).unwrap();
        assert_eq!(r, (Cursor::new(0, 4), Cursor::new(0, 7)));
    }

    #[test]
    fn paren_around_includes_parens() {
        let b = buf_at("foo(bar)baz", 0, 5);
        let r = paren(b.lines(), b.cursor(), Scope::Around).unwrap();
        assert_eq!(r, (Cursor::new(0, 3), Cursor::new(0, 8)));
    }

    #[test]
    fn paren_handles_nested_inner() {
        let b = buf_at("a(b(c)d)e", 0, 4);
        // Cursor on inner 'c'; smallest enclosing pair = (3, 5).
        let r = paren(b.lines(), b.cursor(), Scope::Inner).unwrap();
        assert_eq!(r, (Cursor::new(0, 4), Cursor::new(0, 5)));
    }

    #[test]
    fn paren_returns_none_when_unmatched() {
        let b = buf_at("no parens here", 0, 4);
        assert!(paren(b.lines(), b.cursor(), Scope::Inner).is_none());
    }
}
