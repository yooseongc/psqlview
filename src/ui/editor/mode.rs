//! Editor mode for vim-flavored modal editing.
//!
//! Two modes for v0.5.0 R2 — `Insert` (default, behaves like the
//! pre-modal editor) and `Normal` (motions / mode-entry primitives).
//! Visual / Command land in later rounds.

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
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Self::Insert => "[INSERT]",
            Self::Normal => "[NORMAL]",
        }
    }
}
