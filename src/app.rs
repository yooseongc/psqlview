use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tokio::sync::mpsc;
use tokio_postgres::CancelToken;

use crate::config::ConnInfo;
use crate::db::{self, catalog, Session};
use crate::event::AppEvent;
use crate::types::ResultSet;
use crate::ui::autocomplete::{AutocompletePopup, SQL_KEYWORDS};
use crate::ui::autocomplete_context::{detect_context, extract_aliases, CompletionContext};
use crate::ui::connect_dialog::ConnectDialogState;
use crate::ui::editor::EditorState;
use crate::ui::results::ResultsState;
use crate::ui::row_detail::RowDetailState;
use crate::ui::schema_tree::SchemaTreeState;
use crate::ui::PaneRects;

/// Top-level screen the app is rendering.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Login dialog — host/port/user/db/password/ssl.
    Connect,
    /// Main three-pane layout (tree, editor, results).
    Workspace,
}

/// Which workspace pane currently receives keyboard input.
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

/// Lifecycle of the currently (or most recently) executing query.
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
    /// Query failed; the `String` is the pre-formatted multi-line message.
    Failed(String),
}

/// Transient status overlay shown in the top-right. Construct via
/// `App::toast_info` / `App::toast_error` rather than building by hand —
/// those helpers set an appropriate TTL.
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

    /// Modal overlay showing every column of the currently-selected result
    /// row. Opened by Enter on the Results pane.
    pub row_detail: RowDetailState,

    /// Whether the keybinding cheatsheet overlay is visible.
    pub cheatsheet_open: bool,

    /// SQL of the most recently executed query. Retained so error renderers
    /// can place a caret at the reported POSITION.
    pub last_run_sql: Option<String>,

    /// Rolling buffer of executed queries for the current session (newest at
    /// front). Memory-only — never written to disk.
    pub history: VecDeque<String>,
    /// Index into `history` during Ctrl+Up/Ctrl+Down recall. `None` means
    /// no recall in progress; each edit resets it.
    pub history_cursor: Option<usize>,

    /// Screen rects of the three workspace panes as of the last draw.
    /// Used to route mouse events to the pane under the pointer.
    pub pane_rects: PaneRects,

    pub toast: Option<Toast>,
    pub should_quit: bool,

    tx: mpsc::UnboundedSender<AppEvent>,
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
            row_detail: RowDetailState::default(),
            cheatsheet_open: false,
            last_run_sql: None,
            history: VecDeque::new(),
            history_cursor: None,
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

    /// Sets focus to `pane`, closing the autocomplete popup if the new
    /// pane isn't the editor (the popup only makes sense while editing).
    fn set_focus(&mut self, pane: FocusPane) {
        self.focus = pane;
        if pane != FocusPane::Editor {
            self.autocomplete = None;
        }
    }

    fn on_mouse(&mut self, ev: MouseEvent) {
        if self.screen != Screen::Workspace {
            return;
        }
        // Modal overlays (cheatsheet, row detail) eat mouse events too —
        // otherwise clicks fall through to the panes underneath, which
        // looks like the modal isn't actually active.
        if self.cheatsheet_open || self.row_detail.open {
            return;
        }
        let target = self.pane_rects.hit_test(ev.column, ev.row);
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some(pane) = target {
                    self.set_focus(pane);
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
                    FocusPane::Editor => self.editor.scroll_lines(delta),
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
        // Don't shove pasted text into the editor while a modal is up.
        if self.cheatsheet_open || self.row_detail.open {
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

        // Modal overlays capture keys before any pane does.
        if self.cheatsheet_open {
            match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?') | KeyCode::Char('q') => {
                    self.cheatsheet_open = false;
                }
                _ => {}
            }
            return;
        }
        if self.row_detail.open {
            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.row_detail.close(),
                KeyCode::Up | KeyCode::Char('k') => self.row_detail.scroll_up(1),
                KeyCode::Down | KeyCode::Char('j') => self.row_detail.scroll_down(1),
                KeyCode::PageUp => self.row_detail.scroll_up(10),
                KeyCode::PageDown => self.row_detail.scroll_down(10),
                _ => {}
            }
            return;
        }
        // F1 anywhere opens the cheatsheet. `?` also opens it, but only
        // outside contexts that swallow the character (editor, search,
        // autocomplete — those treat it as typed input).
        let help_via_slash = matches!(key.code, KeyCode::Char('?'))
            && !self.connecting
            && !matches!(self.query_status, QueryStatus::Running { .. })
            && self.tree.search.is_none()
            && self.autocomplete.is_none()
            && !(self.focus == FocusPane::Editor && self.screen == Screen::Workspace);
        let help_via_f1 = matches!(key.code, KeyCode::F(1));
        if help_via_slash || help_via_f1 {
            self.cheatsheet_open = true;
            return;
        }
        // Esc dismisses a visible toast immediately before anything else
        // reads Esc. Skipped while a modal sub-state owns Esc: connecting,
        // running query, active autocomplete, or tree incremental search.
        if matches!(key.code, KeyCode::Esc)
            && self.toast.is_some()
            && !self.connecting
            && !matches!(self.query_status, QueryStatus::Running { .. })
            && self.autocomplete.is_none()
            && self.tree.search.is_none()
        {
            self.toast = None;
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
                self.set_focus(pane);
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

        // Incremental search in the tree pane absorbs every key until
        // the user commits (Enter) or cancels (Esc). Otherwise Tab would
        // cycle focus out mid-search, F5 would run a query, etc.
        if self.focus == FocusPane::Tree && self.tree.search.is_some() {
            self.on_key_tree(key);
            return;
        }

        // Ctrl+Enter runs the current query regardless of focus.
        if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Enter) {
            self.run_current_query();
            return;
        }

        // Ctrl+Up/Down in the editor recalls past queries from session
        // history. Ignored outside the editor so the tree/results panes
        // keep their scroll semantics.
        if self.focus == FocusPane::Editor && key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Up => {
                    self.history_prev();
                    return;
                }
                KeyCode::Down => {
                    self.history_next();
                    return;
                }
                _ => {}
            }
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
                    if let Some((s, e)) = self.editor.selected_line_range() {
                        self.editor.outdent_lines(s, e);
                    } else {
                        self.editor.outdent_current_line();
                    }
                } else {
                    self.focus = match self.focus {
                        FocusPane::Tree => FocusPane::Results,
                        FocusPane::Editor => FocusPane::Tree,
                        FocusPane::Results => FocusPane::Editor,
                    };
                }
            }
            _ => match self.focus {
                FocusPane::Editor => {
                    self.editor.handle_key(key);
                    // Any direct edit invalidates an in-progress history
                    // walk; user is no longer just browsing.
                    self.history_cursor = None;
                }
                FocusPane::Tree => self.on_key_tree(key),
                FocusPane::Results => {
                    // Enter on a populated result opens the per-row
                    // detail modal, bypassing the Results handler.
                    if matches!(key.code, KeyCode::Enter)
                        && self
                            .results
                            .current
                            .as_ref()
                            .is_some_and(|s| !s.rows.is_empty())
                    {
                        self.row_detail.open();
                    } else {
                        self.results.handle_key(key);
                    }
                }
            },
        }
    }

    /// Opens the autocomplete popup if there's a word prefix at the cursor
    /// with at least one match, otherwise inserts a 2-space indent. If a
    /// multi-line selection is active, instead block-indents the entire
    /// selected range.
    fn handle_editor_tab(&mut self) {
        if let Some((s, e)) = self.editor.selected_line_range() {
            if e > s {
                self.editor.indent_lines(s, e);
                return;
            }
        }
        let prefix = self.editor.word_prefix_before_cursor();
        if prefix.is_empty() {
            self.editor.insert_spaces(2);
            return;
        }
        let candidates = self.completion_candidates();
        match AutocompletePopup::open(prefix, candidates) {
            Some(popup) => self.autocomplete = Some(popup),
            None => self.editor.insert_spaces(2),
        }
    }

    /// Builds the candidate pool for the autocomplete popup. The list is
    /// narrowed by the cursor's surrounding clause:
    ///
    /// - After `FROM` / `JOIN` / `INTO` / `UPDATE` / `TABLE`: relation
    ///   names only.
    /// - After `qualifier.`: columns of `qualifier`, where `qualifier` is
    ///   resolved as (1) an alias defined in the same buffer, (2) a known
    ///   relation name, or (3) a known schema name (in which case the
    ///   candidates become the relation names in that schema).
    /// - Otherwise: the full keyword + identifier list.
    ///
    /// Falls back to the default list if a context-specific lookup yields
    /// no candidates — better to show *something* than to mis-narrow when
    /// the schema tree hasn't been loaded yet.
    fn completion_candidates(&self) -> Vec<String> {
        let lines = self.editor.lines();
        let (row, col) = self.editor.cursor_pos();
        match detect_context(lines, row, col) {
            CompletionContext::TableName => {
                let names = self.tree.relation_names();
                if names.is_empty() {
                    self.default_candidates()
                } else {
                    names
                }
            }
            CompletionContext::Dotted { qualifier } => {
                let cols = self.resolve_dotted(&qualifier);
                if cols.is_empty() {
                    self.default_candidates()
                } else {
                    cols
                }
            }
            CompletionContext::Default => self.default_candidates(),
        }
    }

    fn default_candidates(&self) -> Vec<String> {
        let mut out: Vec<String> = SQL_KEYWORDS.iter().map(|s| (*s).to_string()).collect();
        out.extend(self.tree.collect_identifiers());
        out
    }

    /// Resolves `qualifier.` to a column list. Tries alias mapping first
    /// (so `u.` after `FROM users u` lists `users` columns), then a direct
    /// relation match, then schema-qualified relations.
    fn resolve_dotted(&self, qualifier: &str) -> Vec<String> {
        let aliases = extract_aliases(self.editor.lines());
        let alias_target = aliases
            .iter()
            .find(|(alias, _)| alias.eq_ignore_ascii_case(qualifier))
            .map(|(_, rel)| rel.clone());
        if let Some(rel) = alias_target {
            let cols = self.tree.columns_of_relation(&rel);
            if !cols.is_empty() {
                return cols;
            }
        }
        let direct = self.tree.columns_of_relation(qualifier);
        if !direct.is_empty() {
            return direct;
        }
        self.tree.relation_names_in_schema(qualifier)
    }

    /// Closes the autocomplete popup if the current prefix is empty or
    /// no longer matches any candidate.
    fn close_popup_if_stale(&mut self) {
        let should_close = match self.autocomplete.as_ref() {
            Some(popup) => popup.prefix().is_empty() || popup.is_empty(),
            None => return,
        };
        if should_close {
            self.autocomplete = None;
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
                self.editor.handle_key(key);
                if let Some(popup) = self.autocomplete.as_mut() {
                    popup.extend_prefix(c);
                }
                self.close_popup_if_stale();
                true
            }
            KeyCode::Backspace => {
                self.editor.handle_key(key);
                if let Some(popup) = self.autocomplete.as_mut() {
                    popup.shrink_prefix();
                }
                self.close_popup_if_stale();
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
        // Incremental-search mode: characters extend the needle and
        // rejump; Enter commits; Esc cancels (last committed needle
        // preserved so `n`/`N` still work).
        if self.tree.search.is_some() {
            match key.code {
                KeyCode::Char(c) => {
                    if let Some(needle) = self.tree.search.as_mut() {
                        needle.push(c);
                    }
                    if let Some(needle) = self.tree.search.clone() {
                        if let Some(idx) = self.tree.find_next(&needle, self.tree.selected) {
                            self.tree.selected = idx;
                        }
                    }
                }
                KeyCode::Backspace => {
                    if let Some(needle) = self.tree.search.as_mut() {
                        needle.pop();
                    }
                }
                KeyCode::Enter => {
                    self.tree.last_search = self.tree.search.take();
                }
                KeyCode::Esc => {
                    self.tree.search = None;
                }
                _ => {}
            }
            return;
        }
        match key.code {
            KeyCode::Char('/') => {
                self.tree.search = Some(String::new());
            }
            KeyCode::Char('n') => {
                if let Some(needle) = self.tree.last_search.clone() {
                    if let Some(idx) = self.tree.find_next(&needle, self.tree.selected) {
                        self.tree.selected = idx;
                    }
                }
            }
            KeyCode::Char('N') => {
                if let Some(needle) = self.tree.last_search.clone() {
                    if let Some(idx) = self.tree.find_prev(&needle, self.tree.selected) {
                        self.tree.selected = idx;
                    }
                }
            }
            KeyCode::Up | KeyCode::Char('k') => self.tree.move_up(),
            KeyCode::Down | KeyCode::Char('j') => self.tree.move_down(),
            KeyCode::PageUp => self.tree.page_up(),
            KeyCode::PageDown => self.tree.page_down(),
            KeyCode::Home => self.tree.jump_to_start(),
            KeyCode::End => self.tree.jump_to_end(),
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
        // Grab the client + cancel token up front so we're done with
        // &self.session before we take &mut self for history/state.
        let (client, cancel) = match self.session.as_ref() {
            Some(s) => (s.client(), s.cancel_token()),
            None => return,
        };
        // Run just the selected portion when one exists, so users can
        // execute a single statement from a buffer of many.
        let sql = self
            .editor
            .selected_text()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| self.editor.text());
        if sql.trim().is_empty() {
            self.toast_info("editor is empty".into());
            return;
        }
        self.autocomplete = None;
        self.push_history(&sql);
        self.query_status = QueryStatus::Running {
            started_at: Instant::now(),
            cancel,
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
                // Jump the editor caret to the offending position so the
                // user can start typing the fix without hunting for it.
                if let Some(pos) = err.original_position() {
                    self.editor.move_cursor_to_char_position(pos);
                }
                self.query_status = QueryStatus::Failed(detailed.clone());
                self.results.clear();
                self.toast_error(detailed);
            }
        }
    }

    const HISTORY_MAX: usize = 50;

    /// Pushes a query onto the in-session history, de-duplicating the most
    /// recent entry so repeated F5 presses don't spam the buffer. Resets
    /// the recall cursor.
    fn push_history(&mut self, sql: &str) {
        let trimmed = sql.trim();
        if trimmed.is_empty() {
            return;
        }
        if self.history.front().map(|s| s.as_str()) == Some(trimmed) {
            self.history_cursor = None;
            return;
        }
        self.history.push_front(trimmed.to_string());
        while self.history.len() > Self::HISTORY_MAX {
            self.history.pop_back();
        }
        self.history_cursor = None;
    }

    /// Recalls an earlier query into the editor (Ctrl+Up). No-op at the
    /// oldest entry.
    fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.history_cursor {
            None => 0,
            Some(i) => (i + 1).min(self.history.len() - 1),
        };
        if let Some(entry) = self.history.get(next).cloned() {
            self.editor.set_text(&entry);
            self.history_cursor = Some(next);
        }
    }

    /// Steps back toward the present (Ctrl+Down). At the newest entry it
    /// clears the editor, matching shell history feel.
    fn history_next(&mut self) {
        let Some(i) = self.history_cursor else { return };
        if i == 0 {
            self.editor.set_text("");
            self.history_cursor = None;
        } else {
            let new = i - 1;
            if let Some(entry) = self.history.get(new).cloned() {
                self.editor.set_text(&entry);
                self.history_cursor = Some(new);
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
    use ratatui::layout::Rect;

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

    fn populate_tree_for_completion(app: &mut App) {
        use crate::db::catalog::{Column, Relation, RelationKind};
        app.tree.set_schemas(vec!["public".into()]);
        app.tree.set_relations(
            "public",
            vec![
                Relation {
                    name: "users".into(),
                    kind: RelationKind::Table,
                },
                Relation {
                    name: "orders".into(),
                    kind: RelationKind::Table,
                },
            ],
        );
        app.tree.set_columns(
            "public",
            "users",
            vec![
                Column {
                    name: "id".into(),
                    data_type: "int".into(),
                    nullable: false,
                    default: None,
                },
                Column {
                    name: "email".into(),
                    data_type: "text".into(),
                    nullable: true,
                    default: None,
                },
            ],
        );
    }

    fn type_str(app: &mut App, s: &str) {
        for c in s.chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
    }

    #[test]
    fn tab_after_from_narrows_to_relation_names() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        populate_tree_for_completion(&mut app);
        type_str(&mut app, "SELECT * FROM us");
        app.on_event(AppEvent::Key(key(KeyCode::Tab, KeyModifiers::NONE)));
        let popup = app.autocomplete.as_ref().expect("popup");
        let cands: Vec<String> = popup.candidates().to_vec();
        assert_eq!(cands, vec!["users".to_string()]);
    }

    #[test]
    fn tab_after_relation_dot_lists_columns() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        populate_tree_for_completion(&mut app);
        type_str(&mut app, "SELECT users.i");
        app.on_event(AppEvent::Key(key(KeyCode::Tab, KeyModifiers::NONE)));
        let popup = app.autocomplete.as_ref().expect("popup");
        let cands: Vec<String> = popup.candidates().to_vec();
        assert_eq!(cands, vec!["id".to_string()]);
    }

    #[test]
    fn tab_after_alias_dot_resolves_alias_to_relation() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        populate_tree_for_completion(&mut app);
        // Pre-populate the FROM clause, then drop the cursor between
        // SELECT and FROM and type the dotted alias prefix there. End
        // result: "SELECT u.em FROM users u" with the cursor right after
        // "em" so the word prefix is "em".
        app.editor.set_text("SELECT  FROM users u");
        assert!(app.editor.move_cursor_to_char_position(8));
        type_str(&mut app, "u.em");
        app.on_event(AppEvent::Key(key(KeyCode::Tab, KeyModifiers::NONE)));
        let popup = app.autocomplete.as_ref().expect("popup");
        let cands: Vec<String> = popup.candidates().to_vec();
        assert_eq!(cands, vec!["email".to_string()]);
    }

    #[test]
    fn tab_after_where_uses_default_candidates() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        populate_tree_for_completion(&mut app);
        type_str(&mut app, "SELECT * FROM users WHERE i");
        app.on_event(AppEvent::Key(key(KeyCode::Tab, KeyModifiers::NONE)));
        let popup = app.autocomplete.as_ref().expect("popup");
        let cands: Vec<String> = popup.candidates().to_vec();
        // Default pool includes both keywords (e.g. INTO, IN, IS) and
        // identifiers (e.g. id). It must NOT be just relation names.
        assert!(cands.contains(&"id".to_string()));
        assert!(cands.iter().any(|s| s == "IN" || s == "INTO" || s == "IS"));
    }
}
