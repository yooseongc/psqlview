use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use ratatui::layout::Rect;
use tokio::sync::mpsc;
use tokio_postgres::CancelToken;

use crate::config::ConnInfo;
use crate::db::{self, catalog, Session};
use crate::event::AppEvent;
use crate::types::ResultSet;
use crate::ui::autocomplete::{AutocompletePopup, SQL_KEYWORDS};
use crate::ui::connect_dialog::ConnectDialogState;
use crate::ui::editor::EditorState;
use crate::ui::results::ResultsState;
use crate::ui::schema_tree::SchemaTreeState;

/// Top-level screen the app is rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    Connect,
    Workspace,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FocusPane {
    Tree,
    Editor,
    Results,
}

impl FocusPane {
    fn cycle(self) -> Self {
        match self {
            Self::Tree => Self::Editor,
            Self::Editor => Self::Results,
            Self::Results => Self::Tree,
        }
    }
}

pub enum QueryStatus {
    Idle,
    Running {
        started_at: Instant,
        cancel: CancelToken,
    },
    Done {
        elapsed: Duration,
    },
    Cancelled,
    Failed(String),
}

pub struct Toast {
    pub message: String,
    pub until: Instant,
    pub is_error: bool,
}

pub struct App {
    pub screen: Screen,
    pub connect_dialog: ConnectDialogState,
    pub session: Option<Session>,

    pub tree: SchemaTreeState,
    pub editor: EditorState,
    pub results: ResultsState,

    pub focus: FocusPane,
    pub query_status: QueryStatus,
    pub connecting: bool,

    pub autocomplete: Option<AutocompletePopup>,

    /// SQL of the most recently executed query. Retained so error renderers
    /// can place a caret at the reported POSITION.
    pub last_run_sql: Option<String>,

    /// Screen rects of the three workspace panes as of the last draw.
    /// Used to route mouse events to the pane under the pointer.
    pub pane_rects: PaneRects,

    pub toast: Option<Toast>,
    pub should_quit: bool,

    tx: mpsc::UnboundedSender<AppEvent>,
}

#[derive(Default, Debug, Clone, Copy)]
pub struct PaneRects {
    pub tree: Rect,
    pub editor: Rect,
    pub results: Rect,
}

impl PaneRects {
    pub fn hit_test(&self, x: u16, y: u16) -> Option<FocusPane> {
        if rect_contains(self.editor, x, y) {
            return Some(FocusPane::Editor);
        }
        if rect_contains(self.results, x, y) {
            return Some(FocusPane::Results);
        }
        if rect_contains(self.tree, x, y) {
            return Some(FocusPane::Tree);
        }
        None
    }
}

fn rect_contains(r: Rect, x: u16, y: u16) -> bool {
    r.width > 0 && r.height > 0 && x >= r.x && x < r.x + r.width && y >= r.y && y < r.y + r.height
}

impl App {
    pub fn new(tx: mpsc::UnboundedSender<AppEvent>) -> Self {
        Self {
            screen: Screen::Connect,
            connect_dialog: ConnectDialogState::new(ConnInfo::default()),
            session: None,
            tree: SchemaTreeState::default(),
            editor: EditorState::new(),
            results: ResultsState::default(),
            focus: FocusPane::Editor,
            query_status: QueryStatus::Idle,
            connecting: false,
            autocomplete: None,
            last_run_sql: None,
            pane_rects: PaneRects::default(),
            toast: None,
            should_quit: false,
            tx,
        }
    }

    pub fn on_event(&mut self, ev: AppEvent) {
        match ev {
            AppEvent::Key(k) => self.on_key(k),
            AppEvent::Mouse(m) => self.on_mouse(m),
            AppEvent::Paste(s) => self.on_paste(s),
            AppEvent::Resize(_, _) => {}
            AppEvent::Tick => self.on_tick(),
            AppEvent::ConnectResult(r) => self.on_connect_result(r),
            AppEvent::QueryResult(r) => self.on_query_result(r),
            AppEvent::SchemasLoaded(r) => match r {
                Ok(schemas) => self.tree.set_schemas(schemas),
                Err(e) => self.toast_error(format!("load schemas: {e}")),
            },
            AppEvent::RelationsLoaded { schema, result } => match result {
                Ok(relations) => self.tree.set_relations(&schema, relations),
                Err(e) => self.toast_error(format!("load relations ({schema}): {e}")),
            },
            AppEvent::ColumnsLoaded {
                schema,
                table,
                result,
            } => match result {
                Ok(cols) => self.tree.set_columns(&schema, &table, cols),
                Err(e) => self.toast_error(format!("load columns ({schema}.{table}): {e}")),
            },
        }
    }

    fn on_mouse(&mut self, ev: MouseEvent) {
        if self.screen != Screen::Workspace {
            return;
        }
        let target = self.pane_rects.hit_test(ev.column, ev.row);
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(pane) = target {
                    self.focus = pane;
                    if pane != FocusPane::Editor {
                        self.autocomplete = None;
                    }
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
                let up = matches!(ev.kind, MouseEventKind::ScrollUp);
                const LINES: i32 = 3;
                let delta = if up { -LINES } else { LINES };
                // Some terminals (Tabby, for instance) emit wheel events
                // without meaningful column/row, so hit_test returns None.
                // Fall back to whichever pane has focus so the wheel still
                // does something useful.
                let pane = target.unwrap_or(self.focus);
                tracing::info!(
                    kind = ?ev.kind,
                    column = ev.column,
                    row = ev.row,
                    pane = ?pane,
                    "mouse scroll"
                );
                match pane {
                    FocusPane::Editor => self.editor.scroll_lines(delta as i16),
                    FocusPane::Results => self.results.scroll_rows(delta),
                    FocusPane::Tree => self.tree.scroll_rows(delta),
                }
            }
            _ => {}
        }
    }

    fn on_paste(&mut self, s: String) {
        if self.screen != Screen::Workspace || self.focus != FocusPane::Editor {
            return;
        }
        self.editor.insert_str(&s);
    }

    fn on_tick(&mut self) {
        if let Some(t) = &self.toast {
            if Instant::now() >= t.until {
                self.toast = None;
            }
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        // Global hotkeys first. Ctrl+C / Ctrl+Q quit unconditionally.
        if is_ctrl_c(&key) || is_ctrl_q(&key) {
            self.should_quit = true;
            return;
        }
        // Direct pane switches (Workspace only).
        // Primary: F2/F3/F4 — chosen because they don't clash with common
        // terminal shortcuts (Tabby's Alt+digit hijacks tab switching).
        // Alt+1/2/3 is kept as a backup for users whose terminals pass it.
        if self.screen == Screen::Workspace {
            let target = match key.code {
                KeyCode::F(2) => Some(FocusPane::Tree),
                KeyCode::F(3) => Some(FocusPane::Editor),
                KeyCode::F(4) => Some(FocusPane::Results),
                KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::ALT) => match c {
                    '1' => Some(FocusPane::Tree),
                    '2' => Some(FocusPane::Editor),
                    '3' => Some(FocusPane::Results),
                    _ => None,
                },
                _ => None,
            };
            if let Some(pane) = target {
                self.focus = pane;
                if pane != FocusPane::Editor {
                    self.autocomplete = None;
                }
                return;
            }
        }

        match self.screen {
            Screen::Connect => self.on_key_connect(key),
            Screen::Workspace => self.on_key_workspace(key),
        }
    }

    fn on_key_connect(&mut self, key: KeyEvent) {
        if self.connecting {
            if matches!(key.code, KeyCode::Esc) {
                self.connecting = false;
                self.toast_info("connect cancelled".into());
            }
            return;
        }
        // Esc on an idle connect dialog is a no-op now; Ctrl+Q quits.
        if matches!(key.code, KeyCode::Esc) {
            return;
        }
        let submit = self.connect_dialog.handle_key(key);
        if submit {
            self.begin_connect();
        }
    }

    fn on_key_workspace(&mut self, key: KeyEvent) {
        // Handle cancellation first when a query is running.
        if matches!(&self.query_status, QueryStatus::Running { .. }) {
            if matches!(key.code, KeyCode::Esc) {
                self.cancel_running_query();
            }
            return;
        }

        // Ctrl+Enter runs the current query regardless of focus.
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Enter) {
            self.run_current_query();
            return;
        }

        // While the autocomplete popup is open it consumes most keys first.
        if self.autocomplete.is_some()
            && self.focus == FocusPane::Editor
            && self.handle_autocomplete_key(key)
        {
            return;
        }

        match key.code {
            KeyCode::F(5) => self.run_current_query(),
            KeyCode::Tab if key.modifiers.is_empty() => {
                if self.focus == FocusPane::Editor {
                    self.handle_editor_tab();
                } else {
                    self.focus = self.focus.cycle();
                }
            }
            KeyCode::BackTab => {
                if self.focus == FocusPane::Editor {
                    self.editor.outdent_current_line();
                } else {
                    self.focus = match self.focus {
                        FocusPane::Tree => FocusPane::Results,
                        FocusPane::Editor => FocusPane::Tree,
                        FocusPane::Results => FocusPane::Editor,
                    };
                }
            }
            _ => match self.focus {
                FocusPane::Editor => self.editor.handle_key(key),
                FocusPane::Tree => self.on_key_tree(key),
                FocusPane::Results => self.results.handle_key(key),
            },
        }
    }

    /// Opens the autocomplete popup if there's a word prefix at the cursor
    /// with at least one match, otherwise inserts a 2-space indent.
    fn handle_editor_tab(&mut self) {
        let prefix = self.editor.word_prefix_before_cursor();
        if prefix.is_empty() {
            self.editor.insert_spaces(2);
            return;
        }
        let mut candidates: Vec<String> = SQL_KEYWORDS.iter().map(|s| (*s).to_string()).collect();
        candidates.extend(self.tree.collect_identifiers());
        match AutocompletePopup::open(prefix, candidates) {
            Some(popup) => self.autocomplete = Some(popup),
            None => self.editor.insert_spaces(2),
        }
    }

    /// Returns true if the key was consumed by the popup.
    fn handle_autocomplete_key(&mut self, key: KeyEvent) -> bool {
        let Some(popup) = self.autocomplete.as_mut() else {
            return false;
        };
        match key.code {
            KeyCode::Up => {
                popup.move_up();
                true
            }
            KeyCode::Down => {
                popup.move_down();
                true
            }
            KeyCode::Tab | KeyCode::Enter if key.modifiers.is_empty() => {
                if let Some(pick) = popup.current().map(str::to_string) {
                    self.editor.replace_word_prefix(&pick);
                }
                self.autocomplete = None;
                true
            }
            KeyCode::Esc => {
                self.autocomplete = None;
                true
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                // Let the editor insert the character, then extend the filter.
                // If no candidates remain, close the popup.
                self.editor.handle_key(key);
                if let Some(popup) = self.autocomplete.as_mut() {
                    popup.extend_prefix(c);
                    if popup.is_empty() {
                        self.autocomplete = None;
                    }
                }
                true
            }
            KeyCode::Backspace => {
                self.editor.handle_key(key);
                if let Some(popup) = self.autocomplete.as_mut() {
                    popup.shrink_prefix();
                    if popup.prefix().is_empty() || popup.is_empty() {
                        self.autocomplete = None;
                    }
                }
                true
            }
            _ => {
                // Any other key (arrows, F-keys, Ctrl-combos) closes the
                // popup but does NOT consume the key — caller handles it.
                self.autocomplete = None;
                false
            }
        }
    }

    fn on_key_tree(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.tree.move_up(),
            KeyCode::Down | KeyCode::Char('j') => self.tree.move_down(),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => self.expand_current_tree_node(),
            KeyCode::Left | KeyCode::Char('h') => self.tree.collapse_current(),
            _ => {}
        }
    }

    fn expand_current_tree_node(&mut self) {
        let Some(node) = self.tree.current_node() else {
            return;
        };
        let Some(session) = &self.session else {
            return;
        };
        let client = session.client();

        match node {
            crate::ui::schema_tree::NodeRef::Schema { name, loaded } => {
                if loaded {
                    self.tree.toggle_current();
                } else {
                    self.tree.mark_loading_current();
                    let tx = self.tx.clone();
                    let schema = name.clone();
                    tokio::spawn(async move {
                        let r = catalog::list_relations(&client, &schema).await;
                        let _ = tx.send(AppEvent::RelationsLoaded { schema, result: r });
                    });
                }
            }
            crate::ui::schema_tree::NodeRef::Relation {
                schema,
                name,
                loaded,
            } => {
                if loaded {
                    self.tree.toggle_current();
                } else {
                    self.tree.mark_loading_current();
                    let tx = self.tx.clone();
                    let s = schema.clone();
                    let t = name.clone();
                    tokio::spawn(async move {
                        let r = catalog::list_columns(&client, &s, &t).await;
                        let _ = tx.send(AppEvent::ColumnsLoaded {
                            schema: s,
                            table: t,
                            result: r,
                        });
                    });
                }
            }
            crate::ui::schema_tree::NodeRef::Column { .. } => {}
        }
    }

    fn begin_connect(&mut self) {
        let info = match self.connect_dialog.snapshot() {
            Ok(info) => info,
            Err(msg) => {
                self.toast_error(msg);
                return;
            }
        };
        self.connecting = true;
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let r = db::connect::connect(&info).await;
            let _ = tx.send(AppEvent::ConnectResult(r));
        });
    }

    fn on_connect_result(&mut self, r: Result<Session, db::DbError>) {
        self.connecting = false;
        match r {
            Ok(session) => {
                self.toast_info(format!(
                    "connected: {} (pg {})",
                    session.label,
                    session.server_version.display()
                ));
                if !session.server_version.is_supported() {
                    self.toast_error(
                        "server is older than PG 14 — functionality may be limited".into(),
                    );
                }
                let client = session.client();
                self.session = Some(session);
                self.screen = Screen::Workspace;
                self.focus = FocusPane::Editor;

                let tx = self.tx.clone();
                tokio::spawn(async move {
                    let r = catalog::list_schemas(&client).await;
                    let _ = tx.send(AppEvent::SchemasLoaded(r));
                });
            }
            Err(err) => self.toast_error(format!("connect failed: {err}")),
        }
    }

    fn run_current_query(&mut self) {
        let Some(session) = &self.session else {
            return;
        };
        let sql = self.editor.text();
        if sql.trim().is_empty() {
            self.toast_info("editor is empty".into());
            return;
        }
        let client = session.client();
        self.autocomplete = None;
        self.query_status = QueryStatus::Running {
            started_at: Instant::now(),
            cancel: session.cancel_token(),
        };
        self.last_run_sql = Some(sql.clone());
        self.results.begin_running();
        self.focus = FocusPane::Results;

        let tx = self.tx.clone();
        tokio::spawn(async move {
            let r = db::query::execute(client, &sql).await;
            let _ = tx.send(AppEvent::QueryResult(r));
        });
    }

    fn cancel_running_query(&mut self) {
        if let QueryStatus::Running { cancel, .. } = &self.query_status {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let mut roots = rustls::RootCertStore::empty();
                roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
                let cfg = rustls::ClientConfig::builder()
                    .with_root_certificates(roots)
                    .with_no_client_auth();
                let tls = tokio_postgres_rustls::MakeRustlsConnect::new(cfg);
                let _ = cancel.cancel_query(tls).await;
            });
            self.toast_info("cancelling…".into());
        }
    }

    fn on_query_result(&mut self, r: Result<ResultSet, db::DbError>) {
        match r {
            Ok(set) => {
                self.query_status = QueryStatus::Done {
                    elapsed: Duration::from_millis(set.elapsed_ms as u64),
                };
                self.results.set_result(set);
            }
            Err(db::DbError::Query(e))
                if e.code() == Some(&tokio_postgres::error::SqlState::QUERY_CANCELED)
                    || e.to_string().contains("canceling statement") =>
            {
                self.query_status = QueryStatus::Cancelled;
                self.results.clear();
                self.toast_info("query cancelled".into());
            }
            Err(err) => {
                let sql = self.last_run_sql.as_deref().unwrap_or("");
                let detailed = err.format_detailed_with_sql(sql);
                tracing::warn!(error = %detailed, "query failed");
                self.query_status = QueryStatus::Failed(detailed.clone());
                self.results.clear();
                self.toast_error(detailed);
            }
        }
    }

    fn toast_info(&mut self, message: String) {
        self.toast = Some(Toast {
            message,
            until: Instant::now() + Duration::from_secs(3),
            is_error: false,
        });
    }

    fn toast_error(&mut self, message: String) {
        tracing::warn!(%message, "error toast");
        // Multi-line errors (e.g., structured Postgres errors with DETAIL/HINT)
        // need more time to read — scale timeout with line count.
        let lines = message.lines().count().max(1) as u64;
        let ttl = 6 + 3 * (lines.saturating_sub(1));
        self.toast = Some(Toast {
            message,
            until: Instant::now() + Duration::from_secs(ttl),
            is_error: true,
        });
    }
}

fn is_ctrl_c(k: &KeyEvent) -> bool {
    k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('c' | 'C'))
}

fn is_ctrl_q(k: &KeyEvent) -> bool {
    k.modifiers.contains(KeyModifiers::CONTROL) && matches!(k.code, KeyCode::Char('q' | 'Q'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_channel() -> (App, mpsc::UnboundedReceiver<AppEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (App::new(tx), rx)
    }

    #[test]
    fn focus_cycles_tree_editor_results() {
        assert_eq!(FocusPane::Tree.cycle(), FocusPane::Editor);
        assert_eq!(FocusPane::Editor.cycle(), FocusPane::Results);
        assert_eq!(FocusPane::Results.cycle(), FocusPane::Tree);
    }

    #[test]
    fn connect_result_err_stays_on_connect_and_sets_error_toast() {
        let (mut app, _rx) = app_with_channel();
        app.connecting = true;
        app.on_event(AppEvent::ConnectResult(Err(db::DbError::Connect(
            "boom".into(),
        ))));
        assert_eq!(app.screen, Screen::Connect);
        assert!(!app.connecting);
        let t = app.toast.as_ref().expect("toast set");
        assert!(t.is_error);
        assert!(t.message.contains("connect failed"), "got: {}", t.message);
    }

    #[test]
    fn schemas_loaded_ok_populates_tree() {
        let (mut app, _rx) = app_with_channel();
        app.on_event(AppEvent::SchemasLoaded(Ok(vec!["public".into()])));
        assert_eq!(app.tree.schemas.len(), 1);
        assert_eq!(app.tree.schemas[0].name, "public");
    }

    #[test]
    fn schemas_loaded_err_sets_error_toast() {
        let (mut app, _rx) = app_with_channel();
        app.on_event(AppEvent::SchemasLoaded(Err(db::DbError::Connect(
            "x".into(),
        ))));
        let t = app.toast.as_ref().expect("toast set");
        assert!(t.is_error);
        assert!(t.message.contains("load schemas"), "got: {}", t.message);
    }

    #[test]
    fn relations_loaded_err_toast_mentions_schema() {
        let (mut app, _rx) = app_with_channel();
        app.on_event(AppEvent::RelationsLoaded {
            schema: "s".into(),
            result: Err(db::DbError::Connect("x".into())),
        });
        let t = app.toast.as_ref().expect("toast set");
        assert!(t.is_error);
        assert!(t.message.contains("(s)"), "got: {}", t.message);
    }

    #[test]
    fn tick_clears_expired_toast() {
        let (mut app, _rx) = app_with_channel();
        app.on_event(AppEvent::SchemasLoaded(Err(db::DbError::Connect(
            "x".into(),
        ))));
        assert!(app.toast.is_some());
        app.toast.as_mut().unwrap().until = Instant::now() - Duration::from_millis(1);
        app.on_event(AppEvent::Tick);
        assert!(app.toast.is_none());
    }

    #[test]
    fn tick_keeps_fresh_toast() {
        let (mut app, _rx) = app_with_channel();
        app.on_event(AppEvent::SchemasLoaded(Err(db::DbError::Connect(
            "x".into(),
        ))));
        app.on_event(AppEvent::Tick);
        assert!(app.toast.is_some());
    }

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn f_keys_switch_focus_pane() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.on_event(AppEvent::Key(key(KeyCode::F(2), KeyModifiers::NONE)));
        assert_eq!(app.focus, FocusPane::Tree);
        app.on_event(AppEvent::Key(key(KeyCode::F(4), KeyModifiers::NONE)));
        assert_eq!(app.focus, FocusPane::Results);
        app.on_event(AppEvent::Key(key(KeyCode::F(3), KeyModifiers::NONE)));
        assert_eq!(app.focus, FocusPane::Editor);
    }

    #[test]
    fn alt_digit_switches_focus_pane() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.on_event(AppEvent::Key(key(KeyCode::Char('1'), KeyModifiers::ALT)));
        assert_eq!(app.focus, FocusPane::Tree);
        app.on_event(AppEvent::Key(key(KeyCode::Char('3'), KeyModifiers::ALT)));
        assert_eq!(app.focus, FocusPane::Results);
        app.on_event(AppEvent::Key(key(KeyCode::Char('2'), KeyModifiers::ALT)));
        assert_eq!(app.focus, FocusPane::Editor);
    }

    #[test]
    fn esc_on_idle_workspace_is_noop() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.on_event(AppEvent::Key(key(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!app.should_quit);
        assert_eq!(app.screen, Screen::Workspace);
    }

    #[test]
    fn esc_on_idle_connect_is_noop() {
        let (mut app, _rx) = app_with_channel();
        // Default screen is Connect.
        app.on_event(AppEvent::Key(key(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(!app.should_quit);
        assert_eq!(app.screen, Screen::Connect);
    }

    #[test]
    fn f10_no_longer_quits() {
        let (mut app, _rx) = app_with_channel();
        app.on_event(AppEvent::Key(key(KeyCode::F(10), KeyModifiers::NONE)));
        assert!(!app.should_quit);
    }

    #[test]
    fn ctrl_q_quits() {
        let (mut app, _rx) = app_with_channel();
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('q'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.should_quit);
    }

    #[test]
    fn workspace_tab_in_tree_cycles_focus() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Tree;
        app.on_event(AppEvent::Key(key(KeyCode::Tab, KeyModifiers::NONE)));
        assert_eq!(app.focus, FocusPane::Editor);
    }

    #[test]
    fn workspace_tab_in_editor_does_not_cycle_focus() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.on_event(AppEvent::Key(key(KeyCode::Tab, KeyModifiers::NONE)));
        // No word prefix → Tab inserts 2 spaces; focus stays on editor.
        assert_eq!(app.focus, FocusPane::Editor);
        assert_eq!(app.editor.text(), "  ");
        assert!(app.autocomplete.is_none());
    }

    #[test]
    fn paste_in_editor_focus_inserts_text() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.on_event(AppEvent::Paste("SELECT 1;\nSELECT 2;".into()));
        assert_eq!(app.editor.text(), "SELECT 1;\nSELECT 2;");
    }

    #[test]
    fn paste_outside_editor_focus_is_ignored() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Results;
        app.on_event(AppEvent::Paste("noise".into()));
        assert_eq!(app.editor.text(), "");
    }

    #[test]
    fn paste_on_connect_screen_is_ignored() {
        let (mut app, _rx) = app_with_channel();
        // default screen is Connect
        app.on_event(AppEvent::Paste("noise".into()));
        assert_eq!(app.editor.text(), "");
    }

    #[test]
    fn mouse_scroll_without_pane_match_falls_back_to_focus() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Tree;
        // rects all zero-sized so hit_test always returns None.
        app.tree
            .set_schemas(vec!["a".into(), "b".into(), "c".into(), "d".into()]);
        assert_eq!(app.tree.selected, 0);
        let wheel = MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        app.on_event(AppEvent::Mouse(wheel));
        assert!(app.tree.selected > 0, "tree selection should advance");
    }

    #[test]
    fn mouse_click_switches_focus_to_clicked_pane() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.pane_rects.tree = Rect::new(0, 0, 30, 10);
        app.pane_rects.editor = Rect::new(30, 0, 50, 5);
        app.pane_rects.results = Rect::new(30, 5, 50, 5);

        let click = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 2,
            modifiers: KeyModifiers::NONE,
        };
        app.on_event(AppEvent::Mouse(click));
        assert_eq!(app.focus, FocusPane::Tree);
    }

    #[test]
    fn tab_with_word_prefix_opens_autocomplete_popup() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        for c in "SEL".chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        app.on_event(AppEvent::Key(key(KeyCode::Tab, KeyModifiers::NONE)));
        assert!(app.autocomplete.is_some(), "popup should open for 'SEL'");
    }
}
