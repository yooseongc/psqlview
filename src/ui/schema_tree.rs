use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState};
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
}

/// Reference to the currently selected logical node, without borrowing.
#[derive(Debug, Clone)]
pub enum NodeRef {
    Schema { name: String, loaded: bool },
    Relation { schema: String, name: String, loaded: bool },
    Column { schema: String, relation: String, name: String },
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

    pub fn current_node(&self) -> Option<NodeRef> {
        self.flatten().into_iter().nth(self.selected).map(|f| f.node)
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

pub fn draw(frame: &mut Frame<'_>, state: &SchemaTreeState, focused: bool, area: Rect) {
    let rows = state.flatten();
    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| {
            let indent = "  ".repeat(row.depth);
            let style = match &row.node {
                NodeRef::Schema { .. } => Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
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
    frame.render_stateful_widget(list, area, &mut list_state);
}
