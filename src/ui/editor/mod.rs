//! SQL editor pane. Self-built — `tui-textarea` 0.7 can't express per-
//! token syntax coloring, so the buffer + cursor + undo + view layers
//! live here in-tree. Public surface is `EditorState` + `draw`.
//!
//! The implementation is intentionally split:
//! * `mod.rs` (this file) — `EditorState` struct, public accessors,
//!   the top-level `handle_key` dispatcher (Ctrl+Z/Y + mode branch),
//!   and the `draw` entry point.
//! * `insert.rs` — Insert-mode key handler.
//! * `normal.rs` — Normal- and operator-pending dispatchers + helpers
//!   (count accumulation, gg/G targets, linewise range math, the
//!   `Operator` enum).
//! * `visual.rs` — Visual-mode dispatcher + selection-applied
//!   operators.
//! * `edits.rs` — public edit operations (replace_word_prefix,
//!   insert_str, set_text, scroll, goto_line, jump_caret,
//!   replace_range, replace_all, indent / outdent).
//! * `util.rs` — free helpers shared by the modal handlers.

pub mod bracket;
pub mod buffer;
pub mod edit;
mod edits;
mod insert;
pub mod mode;
pub mod motion;
mod normal;
pub mod render;
pub mod tab;
pub mod text_object;
pub mod undo;
mod util;
mod visual;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders};
use ratatui::Frame;

use super::focus_style;
use buffer::TextBuffer;
use mode::Mode;
use normal::Operator;
use render::ViewState;
use undo::UndoStack;

const PLACEHOLDER: &str = "-- F5 / Ctrl+Enter to run, Tab = autocomplete";

pub struct EditorState {
    buf: TextBuffer,
    undo: UndoStack,
    view: ViewState,
    mode: Mode,
    /// Accumulated count prefix in Normal / Visual mode. `0` means no
    /// count pending; `1`–`9` followed by any digit (including `0`)
    /// extends the count. Reset whenever a motion / mode-entry /
    /// unmapped key fires.
    pending_count: u32,
    /// First half of a pending chord (currently only `g` for `gg`).
    pending_chord: Option<char>,
    /// Operator (`d` / `y` / `c`) awaiting its target.
    pending_op: Option<Operator>,
    /// Text-object scope (`i` for inner, `a` for around) once an
    /// operator is pending. `None` while no `di` / `da` prefix has
    /// been seen.
    pending_obj_scope: Option<text_object::Scope>,
    /// Single unnamed register — yanked / deleted text lands here so
    /// `p` / `P` can paste it back. Per-tab so the data structure
    /// stays self-contained; cross-tab sharing would need a global.
    register: String,
}

impl Default for EditorState {
    fn default() -> Self {
        Self::new()
    }
}

impl EditorState {
    pub fn new() -> Self {
        Self {
            buf: TextBuffer::new(),
            undo: UndoStack::new(),
            view: ViewState::default(),
            mode: Mode::default(),
            pending_count: 0,
            pending_chord: None,
            pending_op: None,
            pending_obj_scope: None,
            register: String::new(),
        }
    }

    pub fn mode(&self) -> Mode {
        self.mode
    }

    // ---- inspectors -------------------------------------------------

    pub fn text(&self) -> String {
        self.buf.text()
    }

    /// Current cursor position, 1-indexed for human display.
    pub fn cursor_line_col(&self) -> (usize, usize) {
        let c = self.buf.cursor();
        (c.row + 1, c.col + 1)
    }

    /// Raw buffer lines (LF-separated). Used by the completion-context
    /// detector to tokenize the prefix preceding the cursor.
    pub fn lines(&self) -> &[String] {
        self.buf.lines()
    }

    /// Cursor position as `(row, col)` in 0-indexed char units.
    pub fn cursor_pos(&self) -> (usize, usize) {
        let c = self.buf.cursor();
        (c.row, c.col)
    }

    /// Returns the currently selected text, or `None` if no selection
    /// is active. Used to let F5 / Ctrl+Enter run just the highlighted
    /// SQL.
    pub fn selected_text(&self) -> Option<String> {
        let (s, e) = self.buf.selection_range()?;
        if s == e {
            return None;
        }
        Some(self.buf.text_in_range(s, e))
    }

    /// Returns the inclusive `[start_row, end_row]` range covered by
    /// the active selection, or `None` if no selection is active.
    pub fn selected_line_range(&self) -> Option<(usize, usize)> {
        let (s, e) = self.buf.selection_range()?;
        Some((s.row, e.row))
    }

    /// Returns the identifier prefix ending at the cursor, or empty
    /// when the cursor is not sitting after `[A-Za-z_][A-Za-z0-9_]*`.
    pub fn word_prefix_before_cursor(&self) -> String {
        let cur = self.buf.cursor();
        let Some(line) = self.buf.lines().get(cur.row) else {
            return String::new();
        };
        let chars: Vec<char> = line.chars().collect();
        let col = cur.col.min(chars.len());
        let mut start = col;
        while start > 0 {
            let c = chars[start - 1];
            if c == '_' || c.is_ascii_alphanumeric() {
                start -= 1;
            } else {
                break;
            }
        }
        if start == col {
            return String::new();
        }
        if chars[start].is_ascii_digit() {
            return String::new();
        }
        chars[start..col].iter().collect()
    }

    // ---- mutations --------------------------------------------------

    /// Routes a key event through the mode-aware dispatcher. Ctrl+Z /
    /// Ctrl+Y for undo / redo are intercepted up front so they fire
    /// in any mode.
    ///
    /// Returns `true` when the buffer text changed — callers use this
    /// to mark the active tab dirty without false positives from
    /// arrow-key / scroll navigation. Undo / redo always return `true`
    /// because the buffer was replaced.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('z') | KeyCode::Char('Z') => {
                    if let Some(prev) = self.undo.undo(&self.buf) {
                        self.buf = prev;
                        return true;
                    }
                    return false;
                }
                KeyCode::Char('y') | KeyCode::Char('Y') => {
                    if let Some(next) = self.undo.redo(&self.buf) {
                        self.buf = next;
                        return true;
                    }
                    return false;
                }
                _ => {}
            }
        }

        match self.mode {
            Mode::Insert => {
                if matches!(key.code, KeyCode::Esc) && key.modifiers.is_empty() {
                    self.mode = Mode::Normal;
                    self.buf.cancel_selection();
                    return false;
                }
                self.handle_insert_key(key)
            }
            Mode::Normal => self.handle_normal_key(key),
            Mode::Visual => self.handle_visual_key(key),
        }
    }

    #[cfg(test)]
    fn type_text(&mut self, s: &str) {
        for c in s.chars() {
            let key = match c {
                '\n' => KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
                _ => KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE),
            };
            self.handle_key(key);
        }
    }
}

pub fn draw(
    frame: &mut Frame<'_>,
    state: &mut EditorState,
    focused: bool,
    hints: &render::RenderHints<'_>,
    area: Rect,
) {
    let mode_style = match state.mode {
        Mode::Insert => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        Mode::Normal => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        Mode::Visual => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
    };
    let title = Line::from(vec![
        Span::raw(" SQL editor "),
        Span::styled(state.mode.label(), mode_style),
        Span::raw("  [F5 run \u{00b7} Tab complete] "),
    ]);
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(focus_style(focused));
    let placeholder = if state.buf.is_empty() {
        Some(PLACEHOLDER)
    } else {
        None
    };
    render::draw(
        frame,
        &state.buf,
        &mut state.view,
        render::DrawArgs {
            area,
            focused,
            block,
            placeholder,
            hints,
        },
    );
}

#[cfg(test)]
mod tests;
