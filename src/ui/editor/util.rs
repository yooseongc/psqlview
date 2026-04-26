//! Free helpers shared by the editor's modal handlers.

use crossterm::event::{KeyCode, KeyEvent};

use super::buffer::{Cursor, TextBuffer};
use super::motion::{self, Motion};

/// True when a previously-armed chord matches this key. Currently
/// only `g` + `g` is supported. Returns `false` (with the chord
/// already cleared by the caller's `take()`) when broken.
pub(super) fn matches_chord_resolution(prev_chord: Option<char>, key: KeyEvent) -> bool {
    matches!((prev_chord, key.code), (Some('g'), KeyCode::Char('g')))
}

/// Inserts a string at the buffer's cursor, preserving newlines and
/// dropping `\r` so CRLF clipboards don't produce blank lines. Used
/// by `insert_str` (bracketed paste), `paste` (vim register), and
/// `apply_replace` (find/replace replacement). Caller owns the undo
/// snapshot. Returns `true` if anything was inserted.
pub(super) fn insert_text(buf: &mut TextBuffer, s: &str) -> bool {
    let mut changed = false;
    for c in s.chars() {
        match c {
            '\r' => continue,
            '\n' => buf.insert_newline(),
            other => buf.insert_char(other),
        }
        changed = true;
    }
    changed
}

pub(super) fn motion_from_keycode(code: KeyCode) -> Option<Motion> {
    match code {
        KeyCode::Char('h') | KeyCode::Left => Some(Motion::Left),
        KeyCode::Char('j') | KeyCode::Down => Some(Motion::Down),
        KeyCode::Char('k') | KeyCode::Up => Some(Motion::Up),
        KeyCode::Char('l') | KeyCode::Right => Some(Motion::Right),
        KeyCode::Char('w') => Some(Motion::WordForward),
        KeyCode::Char('b') => Some(Motion::WordBackward),
        KeyCode::Char('e') => Some(Motion::WordEnd),
        KeyCode::Char('0') => Some(Motion::LineStart),
        KeyCode::Char('^') => Some(Motion::FirstNonBlank),
        KeyCode::Char('$') => Some(Motion::LineEnd),
        KeyCode::Char('%') => Some(Motion::MatchingBracket),
        _ => None,
    }
}

pub(super) fn motion_inclusive(motion: Motion) -> bool {
    matches!(
        motion,
        Motion::WordEnd | Motion::LineEnd | Motion::MatchingBracket
    )
}

pub(super) fn cursor_le(a: Cursor, b: Cursor) -> bool {
    (a.row, a.col) <= (b.row, b.col)
}

/// Bumps a cursor one logical position forward, saturating at end of
/// buffer. Used by operator dispatch to convert an inclusive motion
/// endpoint into an exclusive one for delete-range.
pub(super) fn step_forward_one(lines: &[String], c: Cursor) -> Cursor {
    motion::step_forward(lines, c).unwrap_or(c)
}
