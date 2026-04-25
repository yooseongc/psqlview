use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use ratatui::Frame;

use crate::db::catalog::{Column, Relation, RelationKind};

use super::focus_style;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadState {
    Unloaded,
    Loading,
    Loaded,
}

#[derive(Debug, Clone)]
pub struct RelationEntry {
    pub name: String,
    pub kind: RelationKind,
    pub columns: Vec<Column>,
    pub expanded: bool,
    pub load: LoadState,
}

#[derive(Debug, Clone)]
pub struct SchemaEntry {
    pub name: String,
    pub relations: Vec<RelationEntry>,
    pub expanded: bool,
    pub load: LoadState,
}

#[derive(Default)]
pub struct SchemaTreeState {
    pub schemas: Vec<SchemaEntry>,
    pub selected: usize,
    /// Last rendered visible-row count. Updated by `draw` each frame so
    /// PageUp/PageDown can step by a screenful rather than a fixed 20.
    pub visible_rows: usize,
    /// Incremental search state. `Some(query)` means the user has pressed
    /// `/` and is typing; an empty string means the prompt is showing
    /// but no characters have been typed yet.
    pub search: Option<String>,
    /// Last committed search needle. Populated on `Enter` after `/`, used
    /// by `n`/`N` to repeat the search without retyping.
    pub last_search: Option<String>,
}

/// Reference to the currently selected logical node, without borrowing.
#[derive(Debug, Clone)]
pub enum NodeRef {
    Schema {
        name: String,
        loaded: bool,
    },
    Relation {
        schema: String,
        name: String,
        loaded: bool,
    },
    Column {
        schema: String,
        relation: String,
        name: String,
    },
}

impl SchemaTreeState {
    pub fn set_schemas(&mut self, names: Vec<String>) {
        self.schemas = names
            .into_iter()
            .map(|n| SchemaEntry {
                name: n,
                relations: Vec::new(),
                expanded: false,
                load: LoadState::Unloaded,
            })
            .collect();
        self.selected = 0;
    }

    pub fn set_relations(&mut self, schema: &str, relations: Vec<Relation>) {
        if let Some(s) = self.schemas.iter_mut().find(|s| s.name == schema) {
            s.relations = relations
                .into_iter()
                .map(|r| RelationEntry {
                    name: r.name,
                    kind: r.kind,
                    columns: Vec::new(),
                    expanded: false,
                    load: LoadState::Unloaded,
                })
                .collect();
            s.load = LoadState::Loaded;
            s.expanded = true;
        }
    }

    pub fn set_columns(&mut self, schema: &str, relation: &str, columns: Vec<Column>) {
        if let Some(s) = self.schemas.iter_mut().find(|s| s.name == schema) {
            if let Some(r) = s.relations.iter_mut().find(|r| r.name == relation) {
                r.columns = columns;
                r.load = LoadState::Loaded;
                r.expanded = true;
            }
        }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
        }
    }

    pub fn move_down(&mut self) {
        let max = self.flatten().len().saturating_sub(1);
        if self.selected < max {
            self.selected += 1;
        }
    }

    pub fn scroll_rows(&mut self, delta: i32) {
        let max = self.flatten().len().saturating_sub(1) as i32;
        let new = (self.selected as i32 + delta).clamp(0, max);
        self.selected = new as usize;
    }

    pub fn page_up(&mut self) {
        let step = self.visible_rows.max(1) as i32;
        self.scroll_rows(-step);
    }

    pub fn page_down(&mut self) {
        let step = self.visible_rows.max(1) as i32;
        self.scroll_rows(step);
    }

    pub fn jump_to_start(&mut self) {
        self.selected = 0;
    }

    pub fn jump_to_end(&mut self) {
        self.selected = self.flatten().len().saturating_sub(1);
    }

    /// Finds the next flattened row whose label contains `needle`
    /// (case-insensitive), starting after `from_exclusive`. Wraps to the
    /// top if nothing matches beyond the start position. Returns the
    /// matching index if any.
    pub fn find_next(&self, needle: &str, from_exclusive: usize) -> Option<usize> {
        if needle.is_empty() {
            return None;
        }
        let rows = self.flatten();
        if rows.is_empty() {
            return None;
        }
        let needle_l = needle.to_ascii_lowercase();
        let n = rows.len();
        for step in 1..=n {
            let i = (from_exclusive + step) % n;
            if rows[i].label.to_ascii_lowercase().contains(&needle_l) {
                return Some(i);
            }
        }
        None
    }

    /// Same as `find_next` but scanning backward (wraps downward).
    pub fn find_prev(&self, needle: &str, from_exclusive: usize) -> Option<usize> {
        if needle.is_empty() {
            return None;
        }
        let rows = self.flatten();
        if rows.is_empty() {
            return None;
        }
        let needle_l = needle.to_ascii_lowercase();
        let n = rows.len();
        for step in 1..=n {
            let i = (from_exclusive + n - step) % n;
            if rows[i].label.to_ascii_lowercase().contains(&needle_l) {
                return Some(i);
            }
        }
        None
    }

    pub fn current_node(&self) -> Option<NodeRef> {
        self.flatten()
            .into_iter()
            .nth(self.selected)
            .map(|f| f.node)
    }

    pub fn toggle_current(&mut self) {
        let Some(flat) = self.flatten().into_iter().nth(self.selected) else {
            return;
        };
        match flat.node {
            NodeRef::Schema { name, .. } => {
                if let Some(s) = self.schemas.iter_mut().find(|s| s.name == name) {
                    s.expanded = !s.expanded;
                }
            }
            NodeRef::Relation { schema, name, .. } => {
                if let Some(s) = self.schemas.iter_mut().find(|s| s.name == schema) {
                    if let Some(r) = s.relations.iter_mut().find(|r| r.name == name) {
                        r.expanded = !r.expanded;
                    }
                }
            }
            NodeRef::Column { .. } => {}
        }
    }

    pub fn collapse_current(&mut self) {
        let Some(flat) = self.flatten().into_iter().nth(self.selected) else {
            return;
        };
        match flat.node {
            NodeRef::Schema { name, .. } => {
                if let Some(s) = self.schemas.iter_mut().find(|s| s.name == name) {
                    s.expanded = false;
                }
            }
            NodeRef::Relation { schema, name, .. } => {
                if let Some(s) = self.schemas.iter_mut().find(|s| s.name == schema) {
                    if let Some(r) = s.relations.iter_mut().find(|r| r.name == name) {
                        r.expanded = false;
                    }
                }
            }
            NodeRef::Column { .. } => {}
        }
    }

    pub fn mark_loading_current(&mut self) {
        let Some(flat) = self.flatten().into_iter().nth(self.selected) else {
            return;
        };
        match flat.node {
            NodeRef::Schema { name, .. } => {
                if let Some(s) = self.schemas.iter_mut().find(|s| s.name == name) {
                    s.load = LoadState::Loading;
                }
            }
            NodeRef::Relation { schema, name, .. } => {
                if let Some(s) = self.schemas.iter_mut().find(|s| s.name == schema) {
                    if let Some(r) = s.relations.iter_mut().find(|r| r.name == name) {
                        r.load = LoadState::Loading;
                    }
                }
            }
            NodeRef::Column { .. } => {}
        }
    }

    /// Loaded relation names across all schemas, deduplicated. Used as the
    /// candidate list when the cursor sits after FROM / JOIN / etc.
    pub fn relation_names(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for s in &self.schemas {
            for r in &s.relations {
                if seen.insert(r.name.clone()) {
                    out.push(r.name.clone());
                }
            }
        }
        out
    }

    /// Loaded relation names in the named schema. Empty if the schema is
    /// unknown or its relations haven't been loaded yet.
    pub fn relation_names_in_schema(&self, schema: &str) -> Vec<String> {
        self.schemas
            .iter()
            .find(|s| s.name == schema)
            .map(|s| s.relations.iter().map(|r| r.name.clone()).collect())
            .unwrap_or_default()
    }

    /// Column names of the first loaded relation matching `relation` in any
    /// schema. Empty if the relation isn't found or its columns haven't
    /// been loaded yet (the user hasn't expanded the relation in the tree).
    pub fn columns_of_relation(&self, relation: &str) -> Vec<String> {
        for s in &self.schemas {
            for r in &s.relations {
                if r.name == relation {
                    return r.columns.iter().map(|c| c.name.clone()).collect();
                }
            }
        }
        Vec::new()
    }

    /// Returns all known identifier names (schemas, relations, columns) for
    /// autocomplete candidates. Dedups and preserves first-seen order so a
    /// popup shows stable results. Only walks what's already loaded — no I/O.
    pub fn collect_identifiers(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let push =
            |s: &str, out: &mut Vec<String>, seen: &mut std::collections::HashSet<String>| {
                if seen.insert(s.to_string()) {
                    out.push(s.to_string());
                }
            };
        for s in &self.schemas {
            push(&s.name, &mut out, &mut seen);
            for r in &s.relations {
                push(&r.name, &mut out, &mut seen);
                for c in &r.columns {
                    push(&c.name, &mut out, &mut seen);
                }
            }
        }
        out
    }

    fn flatten(&self) -> Vec<FlatRow> {
        let mut out = Vec::new();
        for s in &self.schemas {
            out.push(FlatRow {
                depth: 0,
                node: NodeRef::Schema {
                    name: s.name.clone(),
                    loaded: matches!(s.load, LoadState::Loaded),
                },
                label: schema_label(s),
            });
            if s.expanded {
                for r in &s.relations {
                    out.push(FlatRow {
                        depth: 1,
                        node: NodeRef::Relation {
                            schema: s.name.clone(),
                            name: r.name.clone(),
                            loaded: matches!(r.load, LoadState::Loaded),
                        },
                        label: relation_label(r),
                    });
                    if r.expanded {
                        for c in &r.columns {
                            out.push(FlatRow {
                                depth: 2,
                                node: NodeRef::Column {
                                    schema: s.name.clone(),
                                    relation: r.name.clone(),
                                    name: c.name.clone(),
                                },
                                label: column_label(c),
                            });
                        }
                    }
                }
            }
        }
        out
    }
}

struct FlatRow {
    depth: usize,
    node: NodeRef,
    label: String,
}

fn schema_label(s: &SchemaEntry) -> String {
    let marker = match (s.expanded, s.load) {
        (_, LoadState::Loading) => "… ",
        (true, _) => "▾ ",
        (false, _) => "▸ ",
    };
    format!("{marker}{}", s.name)
}

fn relation_label(r: &RelationEntry) -> String {
    let marker = match (r.expanded, r.load) {
        (_, LoadState::Loading) => "… ",
        (true, _) => "▾ ",
        (false, _) => "▸ ",
    };
    format!("{marker}{} [{}]", r.name, r.kind.label())
}

fn column_label(c: &Column) -> String {
    let nn = if c.nullable { "" } else { " NOT NULL" };
    format!("• {} : {}{}", c.name, c.data_type, nn)
}

pub fn draw(frame: &mut Frame<'_>, state: &mut SchemaTreeState, focused: bool, area: Rect) {
    // When an incremental search is active, reserve the bottom row inside
    // the border for the search prompt.
    let search_row = state.search.is_some() && area.height >= 3;
    let search_needle = state.search.clone();
    let list_area = if search_row {
        // Reduce by 1 to leave space for the prompt line.
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(1)])
            .split(area);
        let prompt = format!("/{}", search_needle.as_deref().unwrap_or(""));
        let p = Paragraph::new(Line::from(Span::styled(
            prompt,
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )));
        frame.render_widget(p, chunks[1]);
        chunks[0]
    } else {
        area
    };
    // area.height includes the 2 border rows; the inner list takes the rest.
    state.visible_rows = list_area.height.saturating_sub(2) as usize;
    let rows = state.flatten();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| {
            let indent = "  ".repeat(row.depth);
            let style = match &row.node {
                NodeRef::Schema { .. } => Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
                NodeRef::Relation { .. } => Style::default().fg(Color::White),
                NodeRef::Column { .. } => Style::default().fg(Color::Gray),
            };
            ListItem::new(Line::from(vec![
                Span::raw(indent),
                Span::styled(row.label.clone(), style),
            ]))
        })
        .collect();

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Schema ")
        .border_style(focus_style(focused));

    let list = List::new(items)
        .block(block)
        .highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut list_state = ListState::default();
    if !rows.is_empty() {
        list_state.select(Some(state.selected.min(rows.len() - 1)));
    }
    frame.render_stateful_widget(list, list_area, &mut list_state);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(name: &str) -> Relation {
        Relation {
            name: name.into(),
            kind: RelationKind::Table,
        }
    }

    fn col(name: &str) -> Column {
        Column {
            name: name.into(),
            data_type: "text".into(),
            nullable: true,
            default: None,
        }
    }

    #[test]
    fn flatten_order_matches_selection_indexing() {
        let mut s = SchemaTreeState::default();
        s.set_schemas(vec!["a".into(), "b".into()]);
        // Expand schema "a" via toggle (selection is already 0).
        s.toggle_current();
        s.set_relations("a", vec![rel("t")]);
        // Move down to the relation and toggle to expand it.
        s.move_down();
        s.toggle_current();
        s.set_columns("a", "t", vec![col("c1"), col("c2")]);

        let flat = s.flatten();
        let labels: Vec<_> = flat
            .iter()
            .map(|r| match &r.node {
                NodeRef::Schema { name, .. } => format!("S:{name}"),
                NodeRef::Relation { schema, name, .. } => format!("R:{schema}.{name}"),
                NodeRef::Column { name, .. } => format!("C:{name}"),
            })
            .collect();
        assert_eq!(labels, vec!["S:a", "R:a.t", "C:c1", "C:c2", "S:b"]);

        s.selected = 3;
        match s.current_node() {
            Some(NodeRef::Column { name, .. }) => assert_eq!(name, "c2"),
            other => panic!("expected column c2, got {other:?}"),
        }
    }

    #[test]
    fn expand_collapse_roundtrip_preserves_selection_bounds() {
        let mut s = SchemaTreeState::default();
        s.set_schemas(vec!["a".into()]);
        s.toggle_current();
        s.set_relations("a", vec![rel("t")]);
        s.move_down();
        s.toggle_current();
        s.set_columns("a", "t", vec![col("c1")]);
        // Navigate to the column.
        s.move_down();
        s.move_down();
        assert_eq!(s.selected, 2);
        // Collapse the schema — tree shrinks to [S:a].
        s.selected = 0;
        s.collapse_current();
        // Now move_down must not panic nor exceed the new upper bound.
        for _ in 0..10 {
            s.move_down();
        }
        assert_eq!(s.flatten().len(), 1);
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn page_down_uses_visible_rows_step() {
        let mut s = SchemaTreeState::default();
        let schemas: Vec<String> = (0..100).map(|i| format!("s{i}")).collect();
        s.set_schemas(schemas);
        s.visible_rows = 10;
        s.page_down();
        assert_eq!(s.selected, 10);
        s.page_down();
        assert_eq!(s.selected, 20);
    }

    #[test]
    fn page_up_clamps_to_zero() {
        let mut s = SchemaTreeState::default();
        s.set_schemas(vec!["a".into(), "b".into(), "c".into()]);
        s.visible_rows = 10;
        s.page_up();
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn jump_to_start_and_end_for_tree() {
        let mut s = SchemaTreeState::default();
        s.set_schemas(vec!["a".into(), "b".into(), "c".into(), "d".into()]);
        s.jump_to_end();
        assert_eq!(s.selected, 3);
        s.jump_to_start();
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn page_down_without_visible_rows_moves_by_one() {
        let mut s = SchemaTreeState::default();
        s.set_schemas(vec!["a".into(), "b".into(), "c".into()]);
        // visible_rows defaults to 0, which is clamped to 1 in page_*.
        s.page_down();
        assert_eq!(s.selected, 1);
    }

    #[test]
    fn find_next_is_case_insensitive_and_wraps() {
        let mut s = SchemaTreeState::default();
        s.set_schemas(vec!["Alpha".into(), "beta".into(), "GAMMA".into()]);
        assert_eq!(s.find_next("bet", 0), Some(1));
        // Wrap from last back to first.
        assert_eq!(s.find_next("alp", 2), Some(0));
    }

    #[test]
    fn find_next_returns_none_for_no_match() {
        let mut s = SchemaTreeState::default();
        s.set_schemas(vec!["foo".into(), "bar".into()]);
        assert!(s.find_next("zzz", 0).is_none());
    }

    #[test]
    fn find_prev_wraps_downward() {
        let mut s = SchemaTreeState::default();
        s.set_schemas(vec!["a".into(), "b".into(), "c".into()]);
        assert_eq!(s.find_prev("a", 0), Some(0));
        assert_eq!(s.find_prev("b", 0), Some(1));
    }

    #[test]
    fn collect_identifiers_dedups_and_includes_all_levels() {
        let mut s = SchemaTreeState::default();
        s.set_schemas(vec!["public".into(), "psqlview_test".into()]);
        s.set_relations("public", vec![rel("users"), rel("orders")]);
        s.set_columns("public", "users", vec![col("id"), col("email")]);
        // Duplicate column name across relations must appear once.
        s.set_relations("psqlview_test", vec![rel("users")]);
        s.set_columns("psqlview_test", "users", vec![col("id"), col("name")]);

        let ids = s.collect_identifiers();
        assert!(ids.contains(&"public".to_string()));
        assert!(ids.contains(&"psqlview_test".to_string()));
        assert!(ids.contains(&"users".to_string()));
        assert!(ids.contains(&"orders".to_string()));
        assert!(ids.contains(&"id".to_string()));
        assert!(ids.contains(&"email".to_string()));
        assert!(ids.contains(&"name".to_string()));
        // Dedup: "users" and "id" appear multiple times across the tree
        // but must be in the output only once each.
        assert_eq!(ids.iter().filter(|n| *n == "users").count(), 1);
        assert_eq!(ids.iter().filter(|n| *n == "id").count(), 1);
    }

    #[test]
    fn relation_names_dedups_across_schemas() {
        let mut s = SchemaTreeState::default();
        s.set_schemas(vec!["public".into(), "psqlview_test".into()]);
        s.set_relations("public", vec![rel("users"), rel("orders")]);
        s.set_relations("psqlview_test", vec![rel("users")]);
        let names = s.relation_names();
        assert_eq!(names.iter().filter(|n| *n == "users").count(), 1);
        assert!(names.contains(&"orders".to_string()));
    }

    #[test]
    fn relation_names_in_schema_filters_to_one_schema() {
        let mut s = SchemaTreeState::default();
        s.set_schemas(vec!["public".into(), "other".into()]);
        s.set_relations("public", vec![rel("users")]);
        s.set_relations("other", vec![rel("logs")]);
        assert_eq!(s.relation_names_in_schema("public"), vec!["users"]);
        assert_eq!(s.relation_names_in_schema("other"), vec!["logs"]);
        assert!(s.relation_names_in_schema("nope").is_empty());
    }

    #[test]
    fn columns_of_relation_returns_loaded_columns() {
        let mut s = SchemaTreeState::default();
        s.set_schemas(vec!["public".into()]);
        s.set_relations("public", vec![rel("users")]);
        s.set_columns("public", "users", vec![col("id"), col("email")]);
        assert_eq!(s.columns_of_relation("users"), vec!["id", "email"]);
        assert!(s.columns_of_relation("missing").is_empty());
    }

    #[test]
    fn toggle_current_on_column_is_noop() {
        let mut s = SchemaTreeState::default();
        s.set_schemas(vec!["a".into()]);
        s.toggle_current();
        s.set_relations("a", vec![rel("t")]);
        s.move_down();
        s.toggle_current();
        s.set_columns("a", "t", vec![col("c1")]);
        // Select the column.
        s.selected = 2;
        let before = s.flatten().len();
        s.toggle_current();
        s.collapse_current();
        assert_eq!(s.flatten().len(), before);
    }
}
