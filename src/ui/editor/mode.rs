//! Editor mode for vim-flavored modal editing.
//!
//! `Insert` is the default. `Normal` and `Visual` follow vim
//! conventions. The `:` command line is a separate modal overlay
//! rather than another mode variant.

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
    /// Selection-extend mode. The live anchor sits on `TextBuffer`
    /// (`selection_anchor`); this variant is just the dispatch flag
    /// that keeps the selection while motions move the cursor.
    Visual,
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Insert => "[INSERT]",
            Self::Normal => "[NORMAL]",
            Self::Visual => "[VISUAL]",
        }
    }
}
