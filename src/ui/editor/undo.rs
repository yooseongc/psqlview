//! Snapshot-based undo / redo stack for the text buffer.
//!
//! On every edit we push the *prior* buffer snapshot onto `undo`. Any
//! new edit clears `redo`. Ctrl+Z pops `undo` onto `redo` and restores;
//! Ctrl+Y does the inverse. Snapshots are bounded so a runaway loop
//! can't grow the heap indefinitely.

use super::buffer::TextBuffer;

const MAX_DEPTH: usize = 256;

#[derive(Debug, Default, Clone)]
pub struct UndoStack {
    undo: Vec<TextBuffer>,
    redo: Vec<TextBuffer>,
}

impl UndoStack {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the buffer state before an edit. Call right before
    /// mutating, with the buffer's current state.
    pub fn record(&mut self, before: &TextBuffer) {
        self.undo.push(before.clone());
        if self.undo.len() > MAX_DEPTH {
            // Drop oldest.
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    /// Pops the most recent snapshot. Returns the state to restore, or
    /// `None` if nothing to undo. The caller must hand the *current*
    /// state to `redo_push` to make redo work.
    pub fn undo(&mut self, current: &TextBuffer) -> Option<TextBuffer> {
        let prev = self.undo.pop()?;
        self.redo.push(current.clone());
        Some(prev)
    }

    /// Mirror of `undo` going the other direction.
    pub fn redo(&mut self, current: &TextBuffer) -> Option<TextBuffer> {
        let next = self.redo.pop()?;
        self.undo.push(current.clone());
        Some(next)
    }

    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_then_undo_returns_prior() {
        let mut stack = UndoStack::new();
        let mut buf = TextBuffer::new();

        stack.record(&buf); // state: ""
        buf.insert_char('a');
        let restored = stack.undo(&buf).expect("undo");
        assert!(restored.is_empty());
    }

    #[test]
    fn redo_after_undo_recovers_change() {
        let mut stack = UndoStack::new();
        let mut buf = TextBuffer::new();
        stack.record(&buf);
        buf.insert_char('x');
        let after = buf.clone();
        let undone = stack.undo(&buf).unwrap();
        // After undo our caller would set buf = undone.
        let mut buf = undone;
        let redone = stack.redo(&buf).unwrap();
        buf = redone;
        assert_eq!(buf.text(), after.text());
    }

    #[test]
    fn new_record_clears_redo() {
        let mut stack = UndoStack::new();
        let mut buf = TextBuffer::new();
        stack.record(&buf);
        buf.insert_char('a');
        let buf = stack.undo(&buf).unwrap();
        // At this point redo has the post-'a' state.
        // A fresh edit must invalidate it.
        let mut buf2 = buf.clone();
        stack.record(&buf2);
        buf2.insert_char('b');
        assert!(stack.redo(&buf2).is_none());
    }

    #[test]
    fn undo_is_capped() {
        let mut stack = UndoStack::new();
        let mut buf = TextBuffer::new();
        for _ in 0..(MAX_DEPTH + 50) {
            stack.record(&buf);
            buf.insert_char('x');
        }
        // Stack grew but never exceeded MAX_DEPTH.
        assert!(stack.undo.len() <= MAX_DEPTH);
    }
}
