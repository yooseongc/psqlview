use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState};
use ratatui::Frame;

pub const SQL_KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "JOIN",
    "ON",
    "LEFT",
    "RIGHT",
    "INNER",
    "OUTER",
    "FULL",
    "CROSS",
    "GROUP",
    "BY",
    "HAVING",
    "ORDER",
    "LIMIT",
    "OFFSET",
    "INSERT",
    "INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE",
    "CREATE",
    "TABLE",
    "VIEW",
    "INDEX",
    "DROP",
    "ALTER",
    "ADD",
    "COLUMN",
    "AS",
    "DISTINCT",
    "UNION",
    "ALL",
    "WITH",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "AND",
    "OR",
    "NOT",
    "NULL",
    "IS",
    "IN",
    "LIKE",
    "ILIKE",
    "BETWEEN",
    "EXPLAIN",
    "ANALYZE",
    "RETURNING",
];

/// Prefix-filtered candidate list shown as an overlay in the editor pane.
///
/// Opened by pressing Tab in the editor when a word prefix (identifier
/// start) sits at the cursor. Candidates are a snapshot of SQL keywords
/// plus loaded schema/table/column names; they do not refresh while the
/// popup is open.
pub struct AutocompletePopup {
    prefix: String,
    all: Vec<String>,
    filtered: Vec<String>,
    selected: usize,
}

impl AutocompletePopup {
    /// Creates a popup if the prefix matches at least one candidate.
    pub fn open(prefix: String, all: Vec<String>) -> Option<Self> {
        if prefix.is_empty() {
            return None;
        }
        let filtered = filter(&prefix, &all);
        if filtered.is_empty() {
            return None;
        }
        Some(Self {
            prefix,
            all,
            filtered,
            selected: 0,
        })
    }

    pub fn extend_prefix(&mut self, c: char) {
        self.prefix.push(c);
        self.recompute();
    }

    pub fn shrink_prefix(&mut self) {
        self.prefix.pop();
        self.recompute();
    }

    fn recompute(&mut self) {
        self.filtered = filter(&self.prefix, &self.all);
        if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len().saturating_sub(1);
        }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.filtered.len() {
            self.selected += 1;
        }
    }

    pub fn current(&self) -> Option<&str> {
        self.filtered.get(self.selected).map(|s| s.as_str())
    }

    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    pub fn is_empty(&self) -> bool {
        self.filtered.is_empty()
    }
}

fn filter(prefix: &str, all: &[String]) -> Vec<String> {
    let needle = prefix.to_ascii_lowercase();
    all.iter()
        .filter(|c| c.to_ascii_lowercase().starts_with(&needle))
        .cloned()
        .collect()
}

pub fn draw(frame: &mut Frame<'_>, popup: &AutocompletePopup, editor_area: Rect) {
    if popup.is_empty() {
        return;
    }
    const MAX_ROWS: u16 = 6;
    const MIN_WIDTH: u16 = 16;
    let longest = popup
        .filtered
        .iter()
        .map(|s| s.chars().count() as u16)
        .max()
        .unwrap_or(0);
    let width = (longest + 4).max(MIN_WIDTH);
    let rows = (popup.filtered.len() as u16).min(MAX_ROWS) + 2; // +2 for borders

    let width = width.min(editor_area.width);
    let rows = rows.min(editor_area.height);
    let x = editor_area.x;
    let y = editor_area.y + editor_area.height.saturating_sub(rows);

    let rect = Rect {
        x,
        y,
        width,
        height: rows,
    };

    let items: Vec<ListItem> = popup
        .filtered
        .iter()
        .map(|c| {
            ListItem::new(Line::from(Span::styled(
                c.clone(),
                Style::default().fg(Color::White),
            )))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(format!(" {} ", popup.prefix()))
        .border_style(Style::default().fg(Color::Yellow));

    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
    );

    let mut state = ListState::default();
    state.select(Some(popup.selected));

    frame.render_widget(Clear, rect);
    frame.render_stateful_widget(list, rect, &mut state);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidates() -> Vec<String> {
        vec![
            "SELECT".into(),
            "SET".into(),
            "FROM".into(),
            "users".into(),
            "user_id".into(),
        ]
    }

    #[test]
    fn open_returns_none_on_no_match() {
        assert!(AutocompletePopup::open("zzz".into(), candidates()).is_none());
    }

    #[test]
    fn open_returns_none_on_empty_prefix() {
        assert!(AutocompletePopup::open("".into(), candidates()).is_none());
    }

    #[test]
    fn open_is_case_insensitive() {
        let p = AutocompletePopup::open("sel".into(), candidates()).expect("popup");
        assert_eq!(p.current(), Some("SELECT"));
    }

    #[test]
    fn extend_prefix_narrows_filtered() {
        let mut p = AutocompletePopup::open("s".into(), candidates()).expect("popup");
        assert_eq!(p.filtered.len(), 2); // SELECT, SET
        p.extend_prefix('e');
        assert_eq!(p.filtered.len(), 2); // SELECT, SET still both start with "se"
        p.extend_prefix('l');
        assert_eq!(p.filtered.len(), 1);
        assert_eq!(p.current(), Some("SELECT"));
    }

    #[test]
    fn shrink_prefix_rewidens_filtered() {
        let mut p = AutocompletePopup::open("sel".into(), candidates()).expect("popup");
        assert_eq!(p.filtered.len(), 1);
        p.shrink_prefix();
        p.shrink_prefix();
        assert!(p.filtered.len() >= 2);
    }

    #[test]
    fn move_up_down_stays_in_bounds() {
        let mut p = AutocompletePopup::open("u".into(), candidates()).expect("popup");
        // "users", "user_id" → 2 candidates
        assert_eq!(p.selected, 0);
        p.move_up(); // no-op
        assert_eq!(p.selected, 0);
        p.move_down();
        assert_eq!(p.selected, 1);
        p.move_down(); // no-op at end
        assert_eq!(p.selected, 1);
        p.move_up();
        assert_eq!(p.selected, 0);
    }

    #[test]
    fn current_follows_selection() {
        let mut p = AutocompletePopup::open("u".into(), candidates()).expect("popup");
        let first = p.current().unwrap().to_string();
        p.move_down();
        let second = p.current().unwrap().to_string();
        assert_ne!(first, second);
    }
}
