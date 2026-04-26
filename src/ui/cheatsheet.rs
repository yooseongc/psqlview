use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

const ROWS: &[(&str, &str)] = &[
    ("Global", ""),
    ("Ctrl+Q / Ctrl+C", "quit"),
    ("F2 / F3 / F4", "focus Tree / Editor / Results"),
    (
        "Alt+1 / Alt+2 / Alt+3",
        "focus Tree / Editor / Results (fallback)",
    ),
    ("Tab / Shift+Tab", "cycle focus (outside editor)"),
    ("Esc", "dismiss toast / cancel query / cancel connect"),
    ("F1 / ?", "show this cheatsheet"),
    ("", ""),
    ("Connect", ""),
    ("Tab / Arrows", "move between fields"),
    ("Enter (last field) / Ctrl+Enter", "submit"),
    ("", ""),
    ("Editor modes (vim-flavored)", ""),
    (
        "Esc",
        "Insert \u{2192} Normal (idle by default; type to switch back)",
    ),
    (
        "i / a",
        "Insert at / after cursor (Normal \u{2192} Insert)",
    ),
    ("I / A", "Insert at line start / line end"),
    ("o / O", "open new line below / above and Insert"),
    ("", ""),
    ("Normal-mode motions", ""),
    ("h j k l (or arrows)", "char left / down / up / right"),
    ("w / b / e", "next word start / prev word start / word end"),
    ("0 / ^ / $", "line start / first non-blank / line end"),
    ("gg / G", "first line / last line (or N: 5G \u{2192} line 5)"),
    ("%", "matching bracket"),
    (
        "<digits>",
        "count prefix (e.g. 5w \u{2192} 5 words; 0 alone is LineStart)",
    ),
    ("", ""),
    ("Editor", ""),
    ("F5 / Ctrl+Enter", "run query (selection or whole buffer)"),
    ("Tab", "autocomplete popup (or 2-space indent)"),
    ("Shift+Tab", "outdent current line"),
    ("Ctrl+Up / Ctrl+Down", "recall previous / next query"),
    ("Ctrl+O / Ctrl+S", "open / save file (cwd-relative path)"),
    ("Ctrl+G", "goto line (1-based, clamped)"),
    (
        "Ctrl+F",
        "find (Enter / F3 next \u{00b7} Shift+F3 prev \u{00b7} Alt+C case); reopens with last needle",
    ),
    (
        "Ctrl+H",
        "find / replace (Tab field \u{00b7} Enter replace one \u{00b7} Alt+A all)",
    ),
    ("Ctrl+Shift+V (terminal)", "paste (bracketed)"),
    ("", ""),
    ("Editor tabs", ""),
    ("Ctrl+T", "open new (untitled) tab"),
    ("Ctrl+W", "close active tab (twice within 3s if dirty)"),
    (
        "Ctrl+] / Ctrl+[",
        "next / previous tab (Ctrl+PageDown/Up fallback)",
    ),
    ("Ctrl+1 .. Ctrl+9", "jump to tab N"),
    ("", ""),
    ("Schema tree", ""),
    ("Up Down Left Right  / hjkl", "navigate"),
    ("PageUp / PageDown", "page by screenful"),
    ("Home / End", "first / last item"),
    ("Enter", "expand / load"),
    (
        "p / Space",
        "preview rows of selected table (SELECT * LIMIT 200)",
    ),
    ("D", "show DDL of selected table (synthesized CREATE TABLE)"),
    ("/", "incremental search"),
    ("n / N", "repeat last search forward / back"),
    ("", ""),
    ("Results", ""),
    ("Up Down / jk", "row"),
    ("PageUp / PageDown", "page by screenful"),
    ("Home / End", "first / last row"),
    ("Left Right / hl", "column scroll"),
    ("Ctrl+Left / Ctrl+Right", "first / last column"),
    ("Ctrl+E", "export current result set to CSV"),
    ("y / Y", "copy current cell / row (OSC 52 clipboard)"),
    ("R", "re-run last query (or refresh DDL view)"),
    ("Enter", "row detail view"),
    (
        "s",
        "sort by leftmost visible column (Asc \u{2192} Desc \u{2192} off)",
    ),
    ("", ""),
    ("Row detail modal", ""),
    ("Up Down / jk", "scroll"),
    ("PageUp / PageDown", "scroll fast"),
    ("Esc / Enter", "close"),
    ("", ""),
    ("Mouse", ""),
    ("Left click", "focus pane under pointer"),
    ("Wheel", "scroll the pane under pointer"),
    (
        "Shift + drag (terminal)",
        "select text (native, bypass capture)",
    ),
];

pub fn draw(frame: &mut Frame<'_>, screen: Rect) {
    // Aim for 90 cols wide so the longest descriptions fit on one line,
    // but always cap to the screen width (Wrap handles narrower terms).
    let w = 90u16.min(screen.width.saturating_sub(2));
    let h = ((ROWS.len() + 2) as u16).min(screen.height.saturating_sub(2));
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

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Keybindings  [Esc / Enter / ? to close] ")
        .border_style(Style::default().fg(Color::Yellow));

    // wrap=trim:false keeps the leading 2-space indent on continuation
    // lines so wrapped descriptions visually align under the first one.
    let paragraph = Paragraph::new(lines)
        .block(block)
        .wrap(Wrap { trim: false });

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}
