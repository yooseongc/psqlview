//! Higher-level edit operations layered on `TextBuffer`. Translates a
//! `crossterm::event::KeyEvent` into one of these operations and runs
//! it. Keeps the buffer's selection state coherent (Shift extends,
//! plain-arrow cancels).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::buffer::{Cursor, TextBuffer};

/// Outcome of dispatching a key. Used by the surrounding editor state
/// to decide whether to push a snapshot onto the undo stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditOutcome {
    /// Buffer text changed.
    Changed,
    /// Cursor / selection moved but text is unchanged.
    Moved,
    /// Key was not handled (caller may pass it elsewhere).
    Unhandled,
}

pub fn handle_key(buf: &mut TextBuffer, key: KeyEvent) -> EditOutcome {
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

    match key.code {
        // Movement -----------------------------------------------------
        KeyCode::Left => {
            update_selection(buf, shift);
            move_left(buf);
            EditOutcome::Moved
        }
        KeyCode::Right => {
            update_selection(buf, shift);
            move_right(buf);
            EditOutcome::Moved
        }
        KeyCode::Up => {
            update_selection(buf, shift);
            move_up(buf);
            EditOutcome::Moved
        }
        KeyCode::Down => {
            update_selection(buf, shift);
            move_down(buf);
            EditOutcome::Moved
        }
        KeyCode::Home => {
            update_selection(buf, shift);
            if ctrl {
                buf.set_cursor(0, 0);
            } else {
                let row = buf.cursor().row;
                buf.set_cursor(row, 0);
            }
            EditOutcome::Moved
        }
        KeyCode::End => {
            update_selection(buf, shift);
            if ctrl {
                let last = buf.line_count().saturating_sub(1);
                buf.set_cursor(last, buf.line_chars(last));
            } else {
                let row = buf.cursor().row;
                buf.set_cursor(row, buf.line_chars(row));
            }
            EditOutcome::Moved
        }

        // Text editing -------------------------------------------------
        // Ctrl+H is sent as Char('h')+CONTROL by some terminals; treat
        // it as Backspace.
        KeyCode::Char('h') if ctrl => {
            if buf.delete_selection().is_some() {
                return EditOutcome::Changed;
            }
            buf.backspace();
            EditOutcome::Changed
        }
        KeyCode::Char(c) if !ctrl => {
            // Replace any active selection with the typed character.
            buf.delete_selection();
            buf.insert_char(c);
            EditOutcome::Changed
        }
        KeyCode::Tab => {
            buf.delete_selection();
            buf.insert_char('\t');
            EditOutcome::Changed
        }
        KeyCode::Enter => {
            buf.delete_selection();
            buf.insert_newline();
            EditOutcome::Changed
        }
        KeyCode::Backspace => {
            if buf.delete_selection().is_some() {
                return EditOutcome::Changed;
            }
            buf.backspace();
            EditOutcome::Changed
        }
        KeyCode::Delete => {
            if buf.delete_selection().is_some() {
                return EditOutcome::Changed;
            }
            buf.delete_forward();
            EditOutcome::Changed
        }

        _ => EditOutcome::Unhandled,
    }
}

/// Manages the selection anchor based on whether Shift is held: holding
/// Shift starts (or extends) a selection at the *current* cursor, and
/// any plain motion clears it.
fn update_selection(buf: &mut TextBuffer, shift: bool) {
    if shift {
        if buf.selection_anchor().is_none() {
            buf.start_selection();
        }
    } else {
        buf.cancel_selection();
    }
}

fn move_left(buf: &mut TextBuffer) {
    let Cursor { row, col } = buf.cursor();
    if col > 0 {
        buf.set_cursor(row, col - 1);
    } else if row > 0 {
        let new_row = row - 1;
        buf.set_cursor(new_row, buf.line_chars(new_row));
    }
}

fn move_right(buf: &mut TextBuffer) {
    let Cursor { row, col } = buf.cursor();
    let line_len = buf.line_chars(row);
    if col < line_len {
        buf.set_cursor(row, col + 1);
    } else if row + 1 < buf.line_count() {
        buf.set_cursor(row + 1, 0);
    }
}

fn move_up(buf: &mut TextBuffer) {
    let Cursor { row, col } = buf.cursor();
    if row == 0 {
        buf.set_cursor(0, 0);
        return;
    }
    let new_row = row - 1;
    buf.set_cursor(new_row, col.min(buf.line_chars(new_row)));
}

fn move_down(buf: &mut TextBuffer) {
    let Cursor { row, col } = buf.cursor();
    let last = buf.line_count().saturating_sub(1);
    if row >= last {
        buf.set_cursor(last, buf.line_chars(last));
        return;
    }
    let new_row = row + 1;
    buf.set_cursor(new_row, col.min(buf.line_chars(new_row)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn typing_char_inserts() {
        let mut b = TextBuffer::new();
        assert_eq!(
            handle_key(&mut b, key(KeyCode::Char('a'), KeyModifiers::NONE)),
            EditOutcome::Changed
        );
        assert_eq!(b.text(), "a");
    }

    #[test]
    fn shift_arrow_extends_selection() {
        let mut b = TextBuffer::from_text("abcd");
        b.set_cursor(0, 0);
        handle_key(&mut b, key(KeyCode::Right, KeyModifiers::SHIFT));
        handle_key(&mut b, key(KeyCode::Right, KeyModifiers::SHIFT));
        let (s, e) = b.selection_range().unwrap();
        assert_eq!((s.col, e.col), (0, 2));
    }

    #[test]
    fn plain_arrow_clears_selection() {
        let mut b = TextBuffer::from_text("abcd");
        b.set_cursor(0, 0);
        b.start_selection();
        b.set_cursor(0, 2);
        handle_key(&mut b, key(KeyCode::Right, KeyModifiers::NONE));
        assert!(!b.is_selecting());
    }

    #[test]
    fn typing_replaces_selection() {
        let mut b = TextBuffer::from_text("abcd");
        b.set_cursor(0, 1);
        b.start_selection();
        b.set_cursor(0, 3);
        handle_key(&mut b, key(KeyCode::Char('X'), KeyModifiers::NONE));
        assert_eq!(b.text(), "aXd");
    }

    #[test]
    fn ctrl_home_jumps_to_top() {
        let mut b = TextBuffer::from_text("a\nb\nc");
        b.set_cursor(2, 1);
        handle_key(&mut b, key(KeyCode::Home, KeyModifiers::CONTROL));
        assert_eq!(b.cursor(), Cursor::new(0, 0));
    }

    #[test]
    fn ctrl_end_jumps_to_bottom() {
        let mut b = TextBuffer::from_text("a\nb\nccc");
        b.set_cursor(0, 0);
        handle_key(&mut b, key(KeyCode::End, KeyModifiers::CONTROL));
        assert_eq!(b.cursor(), Cursor::new(2, 3));
    }

    #[test]
    fn move_up_clamps_column() {
        let mut b = TextBuffer::from_text("ab\nlonger line");
        b.set_cursor(1, 11);
        handle_key(&mut b, key(KeyCode::Up, KeyModifiers::NONE));
        assert_eq!(b.cursor(), Cursor::new(0, 2));
    }
}
