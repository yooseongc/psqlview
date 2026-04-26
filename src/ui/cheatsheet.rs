//! Centred modal that lists every keybinding. Scrolls vertically
//! since the list outgrows a small terminal. Reuses the same
//! `Paragraph::scroll` + `clamp_scroll` shape as the row-detail
//! modal so both scrollable overlays behave the same way.

use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

/// Keybinding cheatsheet rows. A `(_, "")` pair is a section header
/// (rendered cyan + bold); a `("", "")` pair is a blank spacer line;
/// every other pair becomes a `key  description` row.
const ROWS: &[(&str, &str)] = &[
    ("Global", ""),
    ("Ctrl+Q  /  Ctrl+C", "quit"),
    ("F1  /  ?", "open this cheatsheet"),
    ("Esc", "dismiss toast / cancel running query"),
    ("", ""),
    ("Workspace", ""),
    ("F2  /  F3  /  F4", "focus Tree / Editor / Results"),
    (
        "Alt+1  /  Alt+2  /  Alt+3",
        "same (terminal-fallback aliases)",
    ),
    ("Tab  /  Shift+Tab", "cycle focus (outside the editor)"),
    ("F5  /  Ctrl+Enter", "run query (selection or whole buffer)"),
    ("Ctrl+E", "export current result set to CSV"),
    ("", ""),
    ("Connect dialog", ""),
    ("Tab  /  arrows", "move between fields"),
    ("Enter  /  Ctrl+Enter", "submit"),
    ("", ""),
    ("Editor — modes (vim)", ""),
    ("Esc", "Insert \u{2192} Normal"),
    ("i  /  a", "Insert at / after cursor"),
    ("I  /  A", "Insert at line start / end"),
    ("o  /  O", "open new line below / above + Insert"),
    ("v", "Normal \u{2192} Visual"),
    ("", ""),
    ("Editor — Normal motions", ""),
    ("h  j  k  l  (or arrows)", "char left / down / up / right"),
    ("w  /  b  /  e", "next word / prev word / word end"),
    ("0  /  ^  /  $", "line start / first non-blank / line end"),
    ("gg  /  G", "first line / last line  (5G \u{2192} line 5)"),
    ("%", "matching bracket"),
    (
        "<digits><motion>",
        "count prefix  (5w \u{2192} 5 words; bare 0 is LineStart)",
    ),
    ("", ""),
    ("Editor — Normal search", ""),
    ("/pat<Enter>", "search forward"),
    ("?pat<Enter>", "search backward"),
    ("n  /  N", "repeat last search  (same / reverse direction)"),
    ("", ""),
    ("Editor — operators + text objects", ""),
    (
        "d  /  y  /  c",
        "delete / yank / change  (op + motion or text obj)",
    ),
    ("dd  /  yy  /  cc", "linewise  (count = N lines)"),
    ("x", "delete char  (count-aware)"),
    ("p  /  P", "paste register after / before cursor"),
    (
        "iw aw  /  iW aW",
        "small word / WORD  (catches schema.table)",
    ),
    ("i\" a\"  /  i' a'", "quoted string  (inner / around)"),
    ("i( a(", "parens  (inner / around)"),
    ("", ""),
    ("Editor — `:` command line", ""),
    (": (or Ctrl+G)", "open the command line"),
    (":N", "goto line N"),
    (":s/pat/repl/[g]", "substitute current line  (g = all)"),
    (":%s/pat/repl/[g]", "substitute whole buffer"),
    (":w  [path]", "save  (path arg overrides)"),
    (":e <path>", "open path"),
    (
        ":tabnew  :tabn  :tabp  :tabc",
        "Ctrl+T / Ctrl+] / Ctrl+[ / Ctrl+W aliases",
    ),
    (":q", "quit"),
    (":help", "open this cheatsheet"),
    ("", ""),
    ("Editor — buffers + file + history", ""),
    ("Ctrl+T", "new tab"),
    ("Ctrl+W", "close tab  (twice within 3s if dirty)"),
    (
        "Ctrl+]  /  Ctrl+[",
        "next / previous tab  (Ctrl+PageDown/Up fallback)",
    ),
    ("Ctrl+1  ..  Ctrl+9", "jump to tab N"),
    ("Ctrl+O  /  Ctrl+S", "open / save file"),
    ("Ctrl+Up  /  Ctrl+Down", "recall previous / next query"),
    ("", ""),
    ("Editor — Insert helpers (non-vim fallback)", ""),
    ("Tab", "autocomplete popup  (or 2-space indent)"),
    ("Shift+Tab", "outdent line / selection"),
    ("Ctrl+Z  /  Ctrl+Y", "undo / redo"),
    ("Ctrl+F", "find  (incremental)"),
    ("Ctrl+H", "find / replace"),
    ("Ctrl+Shift+V", "paste  (bracketed, terminal feature)"),
    ("", ""),
    ("Schema tree", ""),
    ("Up Down  /  h j k l", "navigate"),
    ("PageUp  /  PageDown", "page"),
    ("Home  /  End", "first / last"),
    ("Enter", "expand / load"),
    ("Space  /  p", "preview rows  (SELECT * LIMIT 200)"),
    ("D", "show DDL of selected table"),
    ("/  ·  n  ·  N", "incremental search + repeat"),
    ("", ""),
    ("Results", ""),
    ("Up Down  /  j k", "row"),
    ("Left Right  /  h l", "column"),
    ("Ctrl+Left  /  Ctrl+Right", "first / last column"),
    ("PageUp  /  PageDown", "page"),
    ("Home  /  End", "first / last row"),
    (
        "s",
        "sort by leftmost column  (Asc \u{2192} Desc \u{2192} off)",
    ),
    ("Enter", "open row-detail modal"),
    ("y  /  Y", "copy cell / row  (OSC 52)"),
    ("R", "re-run last query  (or refresh DDL view)"),
    ("", ""),
    ("Row detail (modal)", ""),
    ("Up Down  /  j k", "scroll"),
    ("PageUp  /  PageDown", "page"),
    ("Esc  /  Enter", "close"),
    ("", ""),
    ("Cheatsheet (this view)", ""),
    ("Up Down  /  j k", "scroll"),
    ("PageUp  /  PageDown", "page"),
    ("Esc  /  Enter  /  ?  /  q", "close"),
    ("", ""),
    ("Mouse", ""),
    ("Left click", "focus pane under pointer"),
    ("Wheel", "scroll the pane under pointer"),
    (
        "Shift+drag",
        "select text  (terminal-native, bypasses capture)",
    ),
];

/// Modal state — `open` mirrors `RowDetailState` so the cascade
/// in `App::on_key` checks it the same way; `scroll` is fed into
/// `Paragraph::scroll` and clamped at draw time so the user can't
/// page past the bottom indefinitely.
#[derive(Default)]
pub struct CheatsheetState {
    pub open: bool,
    pub scroll: u16,
}

impl CheatsheetState {
    pub fn open(&mut self) {
        self.open = true;
        self.scroll = 0;
    }
    pub fn close(&mut self) {
        self.open = false;
        self.scroll = 0;
    }
    pub fn scroll_up(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_sub(n);
    }
    pub fn scroll_down(&mut self, n: u16) {
        self.scroll = self.scroll.saturating_add(n);
    }
}

pub fn draw(frame: &mut Frame<'_>, state: &CheatsheetState, screen: Rect) {
    if !state.open {
        return;
    }

    // Modal box: 90 cols wide (or screen width minus padding); 80% of
    // the screen height. Centred.
    let w = 90u16.min(screen.width.saturating_sub(2));
    let max_h = screen.height.saturating_sub(2);
    let h = (screen.height * 8 / 10).max(10).min(max_h);
    let x = screen.x + screen.width.saturating_sub(w) / 2;
    let y = screen.y + screen.height.saturating_sub(h) / 2;
    let area = Rect {
        x,
        y,
        width: w,
        height: h,
    };

    let lines: Vec<Line> = ROWS
        .iter()
        .map(|(key, desc)| {
            if desc.is_empty() && !key.is_empty() {
                Line::from(Span::styled(
                    (*key).to_string(),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ))
            } else if key.is_empty() {
                Line::from("")
            } else {
                Line::from(vec![
                    Span::styled(
                        format!("  {:<32}", key),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled((*desc).to_string(), Style::default().fg(Color::White)),
                ])
            }
        })
        .collect();

    let total_lines = lines.len() as u16;
    let scroll = clamp_scroll(state.scroll, total_lines, h);
    let scrollable = total_lines + 2 > h; // 2 = top + bottom borders
    let title = if scrollable {
        " Keybindings  [\u{2191}\u{2193} / jk scroll \u{00b7} PgUp/PgDn page \u{00b7} Esc close] "
    } else {
        " Keybindings  [Esc / Enter / ? / q to close] "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .title(title)
        .border_style(Style::default().fg(Color::Yellow));

    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false })
        .scroll((scroll, 0));

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

fn clamp_scroll(requested: u16, total: u16, visible: u16) -> u16 {
    // Mirror row_detail's clamp: leave at least one line of content
    // visible at max scroll.
    let max = total.saturating_sub(visible.saturating_sub(2));
    requested.min(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_starts_closed() {
        let s = CheatsheetState::default();
        assert!(!s.open);
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn open_resets_scroll() {
        let mut s = CheatsheetState::default();
        s.scroll_down(10);
        s.open();
        assert!(s.open);
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn close_clears_open_and_scroll() {
        let mut s = CheatsheetState::default();
        s.open();
        s.scroll_down(5);
        s.close();
        assert!(!s.open);
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn scroll_up_saturates_at_zero() {
        let mut s = CheatsheetState::default();
        s.scroll_up(5);
        assert_eq!(s.scroll, 0);
    }

    #[test]
    fn scroll_down_accumulates_then_draw_clamps() {
        // The state itself doesn't know the viewport — it just adds.
        // `clamp_scroll` (run inside `draw`) caps the visible offset.
        let mut s = CheatsheetState::default();
        s.scroll_down(99);
        assert_eq!(s.scroll, 99);
        // With 50 total lines and 20 visible, the cap is 50 - (20 - 2) = 32.
        assert_eq!(clamp_scroll(s.scroll, 50, 20), 32);
    }

    #[test]
    fn clamp_scroll_zero_when_content_fits() {
        assert_eq!(clamp_scroll(0, 10, 30), 0);
        assert_eq!(clamp_scroll(99, 10, 30), 0);
    }

    #[test]
    fn rows_table_is_non_empty_and_has_at_least_one_section_header() {
        // Sanity: a section header is `(non-empty, "")`.
        assert!(!ROWS.is_empty());
        assert!(ROWS.iter().any(|(k, d)| !k.is_empty() && d.is_empty()));
    }
}
