//! Editor mode for vim-flavored modal editing.
//!
//! `Insert` is the default (pre-modal-editor behavior). `Normal` and
//! `Visual` follow vim conventions; Command mode lands in R6.

use super::buffer::Cursor;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    /// Plain text-entry; every printable key inserts a character.
    /// Default on a fresh `EditorState` so a user who doesn't know about
    /// modal editing isn't suddenly typing into a no-op buffer.
    #[default]
    Insert,
    /// Motion / command mode — keys are interpreted as commands rather
    /// than literal input.
    Normal,
    /// Selection-extend mode. `anchor` records the cursor position at
    /// the moment Visual was entered; the live selection always runs
    /// between `anchor` and the current cursor.
    Visual { anchor: Cursor },
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Insert => "[INSERT]",
            Self::Normal => "[NORMAL]",
            Self::Visual { .. } => "[VISUAL]",
        }
    }
}
