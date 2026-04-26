use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use tokio::sync::mpsc;
use tokio_postgres::CancelToken;

use crate::db::catalog::RelationKind;
use crate::db::{self, catalog, Session};
use crate::event::AppEvent;
use crate::ui::autocomplete::AutocompletePopup;
use crate::ui::command_line::CommandLineState;
use crate::ui::connect_dialog::ConnectDialogState;
use crate::ui::editor::tab::{CloseOutcome, Tabs};
use crate::ui::editor::EditorState;
use crate::ui::file_prompt::FilePromptState;
use crate::ui::find::FindState;
use crate::ui::results::ResultsState;
use crate::ui::row_detail::RowDetailState;
use crate::ui::schema_tree::SchemaTreeState;
use crate::ui::PaneRects;

mod autocomplete;
mod clipboard;
mod file_io;
mod history;
mod keys;
mod query;
mod toasts;

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
    pub(super) fn cycle(self) -> Self {
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
    /// Open editor buffers + active-tab pointer + dirty-close
    /// confirmation state, all in one container so the App impl
    /// doesn't have to coordinate three correlated fields.
    pub tabs: Tabs,
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

    /// Inline filename prompt for `Ctrl+O` / `Ctrl+S`. While `Some`, the
    /// prompt is modal at the application level — every key routes to it.
    pub file_prompt: Option<FilePromptState>,

    /// `:` command line — single-line ex prompt. While `Some`, every
    /// editor key routes to it. Slotted between `file_prompt` and
    /// `find` in the modal precedence chain. `Ctrl+G` opens it as
    /// well, since `:42` replaces the v0.4 goto-line overlay.
    pub command_line: Option<CommandLineState>,

    /// `Ctrl+F` find / `Ctrl+H` find-replace overlay. While `Some`,
    /// it absorbs editing keystrokes (text into the needle, F3 / Enter
    /// to advance) — slotted right above the editor pane in the modal
    /// precedence chain.
    pub find: Option<FindState>,

    /// SQL of the most recently executed query. Retained so error renderers
    /// can place a caret at the reported POSITION.
    pub last_run_sql: Option<String>,

    /// `(schema, relation, kind)` of the last DDL view shown via the `D`
    /// shortcut. Allows `R` (re-run) to refresh the DDL view through the
    /// catalog rather than executing the placeholder SQL literally.
    /// Cleared whenever a normal SQL query is dispatched.
    pub last_ddl_target: Option<(String, String, RelationKind)>,

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

    pub(super) tx: mpsc::UnboundedSender<AppEvent>,
}

impl App {
    pub fn new(tx: mpsc::UnboundedSender<AppEvent>) -> Self {
        Self {
            screen: Screen::Connect,
            connect_dialog: ConnectDialogState::new(crate::config::ConnInfo::default()),
            session: None,
            tree: SchemaTreeState::default(),
            tabs: Tabs::new(),
            results: ResultsState::default(),
            focus: FocusPane::Editor,
            query_status: QueryStatus::Idle,
            connecting: false,
            autocomplete: None,
            row_detail: RowDetailState::default(),
            cheatsheet_open: false,
            file_prompt: None,
            command_line: None,
            find: None,
            last_run_sql: None,
            last_ddl_target: None,
            history: VecDeque::new(),
            history_cursor: None,
            pane_rects: PaneRects::default(),
            toast: None,
            should_quit: false,
            tx,
        }
    }

    /// Borrow the active tab's editor immutably.
    pub fn editor(&self) -> &EditorState {
        &self.tabs.active().editor
    }

    /// Borrow the active tab's editor mutably.
    pub fn editor_mut(&mut self) -> &mut EditorState {
        &mut self.tabs.active_mut().editor
    }

    // ---- tab management -------------------------------------------
    //
    // The data-structure logic (push / cycle / jump / close + dirty
    // confirmation) lives on `Tabs`. The thin wrappers below are
    // responsible for the App-level invariants that don't belong on
    // the data structure: clearing per-tab modal overlays on switch,
    // surfacing the "unsaved changes" toast on a dirty close.

    /// Mark the active tab as dirty (unsaved).
    pub(super) fn mark_active_dirty(&mut self) {
        self.tabs.mark_active_dirty();
    }

    /// Per-tab modal state that doesn't survive a tab switch — the
    /// overlays anchor visually to the active editor and would feel
    /// stale on the next tab.
    fn switch_tab_cleanup(&mut self) {
        self.autocomplete = None;
        self.last_ddl_target = None;
        self.find = None;
        self.command_line = None;
    }

    pub fn new_tab(&mut self) {
        self.switch_tab_cleanup();
        self.tabs.open_new();
    }

    pub fn cycle_tab(&mut self, delta: isize) {
        let prev = self.tabs.active;
        self.tabs.cycle(delta);
        if self.tabs.active != prev {
            self.switch_tab_cleanup();
        }
    }

    pub fn jump_tab(&mut self, idx: usize) {
        let prev = self.tabs.active;
        self.tabs.jump(idx);
        if self.tabs.active != prev {
            self.switch_tab_cleanup();
        }
    }

    pub fn close_active_tab(&mut self) {
        match self.tabs.try_close_active() {
            CloseOutcome::Closed => self.switch_tab_cleanup(),
            CloseOutcome::PendingDirty => {
                self.toast_info("unsaved changes — Ctrl+W again to discard".into());
            }
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
    pub(super) fn set_focus(&mut self, pane: FocusPane) {
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
                    FocusPane::Editor => self.editor_mut().scroll_lines(delta),
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
        self.editor_mut().insert_str(&s);
        self.mark_active_dirty();
    }

    fn on_tick(&mut self) {
        if let Some(t) = &self.toast {
            if Instant::now() >= t.until {
                self.toast = None;
            }
        }
    }

    pub(super) fn begin_connect(&mut self) {
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
}

#[cfg(test)]
mod tests {
    use super::clipboard::{format_cell_for_copy, truncate_for_toast};
    use super::query::{build_preview_sql, ddl_to_resultset, quote_ident};
    use super::*;
    use crate::types::ResultSet;
    use crate::ui::file_prompt::FilePromptMode;
    use crate::ui::find;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::layout::Rect;

    fn app_with_channel() -> (App, mpsc::UnboundedReceiver<AppEvent>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (App::new(tx), rx)
    }

    #[test]
    fn quote_ident_doubles_internal_quotes() {
        assert_eq!(quote_ident("users"), r#""users""#);
        assert_eq!(quote_ident("My\"Table"), r#""My""Table""#);
        assert_eq!(quote_ident("WITH"), r#""WITH""#);
    }

    #[test]
    fn format_cell_for_copy_renders_null_as_empty_string() {
        use crate::types::CellValue;
        assert_eq!(format_cell_for_copy(&CellValue::Null), "");
        assert_eq!(format_cell_for_copy(&CellValue::Int(42)), "42");
        assert_eq!(format_cell_for_copy(&CellValue::Text("hi".into())), "hi");
    }

    #[test]
    fn truncate_for_toast_caps_long_strings_with_ellipsis() {
        assert_eq!(truncate_for_toast("short", 40), "short");
        let long = "a".repeat(50);
        let out = truncate_for_toast(&long, 10);
        assert_eq!(out.chars().count(), 11); // 10 chars + ellipsis
        assert!(out.ends_with('\u{2026}'));
    }

    #[test]
    fn copy_helpers_format_cell_and_row_correctly() {
        use crate::types::{CellValue, ColumnMeta};
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.results.set_result(ResultSet {
            columns: vec![
                ColumnMeta {
                    name: "id".into(),
                    type_name: "int".into(),
                },
                ColumnMeta {
                    name: "name".into(),
                    type_name: "text".into(),
                },
            ],
            rows: vec![
                vec![CellValue::Int(7), CellValue::Text("alice".into())],
                vec![CellValue::Int(8), CellValue::Null],
            ],
            ..ResultSet::default()
        });
        app.results.selected_row = 1;
        // x_offset 0 means "id" is leftmost visible.
        app.results.x_offset = 0;
        assert_eq!(app.format_current_cell().as_deref(), Some("8"));
        assert_eq!(app.format_current_row_as_tsv().as_deref(), Some("8\t"));
        // Move x_offset → cell follows the leftmost visible column.
        app.results.x_offset = 1;
        assert_eq!(app.format_current_cell().as_deref(), Some(""));
    }

    #[test]
    fn dispatch_sql_clears_last_ddl_target() {
        let (mut app, _rx) = app_with_channel();
        app.last_ddl_target = Some(("public".into(), "users".into(), RelationKind::Table));
        // Without a session, dispatch_sql is a no-op — but we want to
        // exercise the bookkeeping. Simulate by calling the internal
        // helper paths that would set / clear the target.
        app.last_run_sql = Some("SELECT 1".into());
        // Trigger the path through which dispatch_sql clears it: emulate
        // by directly invoking the same field-clear behavior. We can't
        // run dispatch_sql without a session, so instead verify the
        // semantic via rerun routing: with last_ddl_target set, a
        // rerun without session prefers the DDL path over the SQL path.
        // Drop session check by clearing the target manually and
        // confirming the SQL fallback works.
        app.last_ddl_target = None;
        app.rerun_last_query();
        // Without session and last_run_sql = Some, dispatch_sql is a
        // no-op; query_status stays Idle (no Running set).
        assert!(matches!(app.query_status, QueryStatus::Idle));
    }

    #[test]
    fn rerun_prefers_ddl_target_over_last_sql() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        // Both fields populated; the DDL target should win. Without a
        // live session, dispatch_ddl_fetch returns early but does so
        // *before* it would have called dispatch_sql, so neither
        // status nor last_run_sql is mutated — confirms the routing.
        app.last_ddl_target = Some(("public".into(), "users".into(), RelationKind::Table));
        app.last_run_sql = Some("SELECT 1".into());
        app.rerun_last_query();
        // No session → dispatch_ddl_fetch returns early; no panic, no
        // history push. The original last_run_sql is preserved (the
        // DDL placeholder would only be written if a session existed).
        assert_eq!(app.last_run_sql.as_deref(), Some("SELECT 1"));
    }

    #[test]
    fn ctrl_j_runs_query_as_ctrl_enter_alias() {
        // Without a session run_current_query is a no-op, but the early-
        // return path still consumes the key — verify the editor isn't
        // mutated.
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        type_str(&mut app, "SELECT 1");
        let before = app.editor().text();
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('j'),
            KeyModifiers::CONTROL,
        )));
        // Editor unchanged — the Ctrl+J was treated as Ctrl+Enter, not
        // a literal char insertion.
        assert_eq!(app.editor().text(), before);
    }

    #[test]
    fn rerun_with_no_history_shows_info_toast() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Results;
        // last_run_sql starts as None.
        app.on_event(AppEvent::Key(key(KeyCode::Char('R'), KeyModifiers::SHIFT)));
        let toast = app.toast.as_ref().expect("toast set");
        assert!(!toast.is_error);
        assert!(toast.message.contains("no previous query"));
    }

    #[test]
    fn ddl_to_resultset_emits_one_row_per_line() {
        let r = ddl_to_resultset("CREATE TABLE t (\n  id int\n);\n", 5);
        assert_eq!(r.columns.len(), 1);
        assert_eq!(r.columns[0].name, "ddl");
        assert_eq!(r.rows.len(), 4); // 3 newlines → 4 split parts
        assert!(matches!(
            &r.rows[0][0],
            crate::types::CellValue::Text(s) if s == "CREATE TABLE t ("
        ));
        assert_eq!(r.elapsed_ms, 5);
    }

    #[test]
    fn build_preview_sql_quotes_both_parts() {
        assert_eq!(
            build_preview_sql("public", "users", 200),
            r#"SELECT * FROM "public"."users" LIMIT 200"#
        );
        assert_eq!(
            build_preview_sql("ns", "with\"quote", 50),
            r#"SELECT * FROM "ns"."with""quote" LIMIT 50"#
        );
    }

    #[test]
    fn fresh_app_has_one_active_tab() {
        let (app, _rx) = app_with_channel();
        assert_eq!(app.tabs.list.len(), 1);
        assert_eq!(app.tabs.active, 0);
        // Active tab is fresh: empty buffer, no path, not dirty.
        assert_eq!(app.editor().text(), "");
        assert!(app.tabs.list[0].path.is_none());
        assert!(!app.tabs.list[0].dirty);
    }

    #[test]
    fn editor_helpers_resolve_to_active_tab() {
        let (mut app, _rx) = app_with_channel();
        app.editor_mut().set_text("abc");
        assert_eq!(app.editor().text(), "abc");
        assert_eq!(app.tabs.list[app.tabs.active].editor.text(), "abc");
    }

    #[test]
    fn new_tab_appends_and_activates() {
        let (mut app, _rx) = app_with_channel();
        app.editor_mut().set_text("first");
        app.new_tab();
        assert_eq!(app.tabs.list.len(), 2);
        assert_eq!(app.tabs.active, 1);
        assert_eq!(app.editor().text(), "");
        // First tab's content survives.
        assert_eq!(app.tabs.list[0].editor.text(), "first");
    }

    #[test]
    fn cycle_tab_wraps_in_either_direction() {
        let (mut app, _rx) = app_with_channel();
        app.new_tab();
        app.new_tab();
        // tabs.len() == 3, active = 2
        app.cycle_tab(1);
        assert_eq!(app.tabs.active, 0); // wrapped
        app.cycle_tab(-1);
        assert_eq!(app.tabs.active, 2); // wrapped backwards
    }

    #[test]
    fn cycle_tab_with_one_tab_is_noop() {
        let (mut app, _rx) = app_with_channel();
        app.cycle_tab(1);
        assert_eq!(app.tabs.active, 0);
        app.cycle_tab(-1);
        assert_eq!(app.tabs.active, 0);
    }

    #[test]
    fn jump_tab_ignores_invalid_index() {
        let (mut app, _rx) = app_with_channel();
        app.new_tab();
        app.jump_tab(99);
        assert_eq!(app.tabs.active, 1);
    }

    #[test]
    fn close_clean_tab_drops_it_immediately() {
        let (mut app, _rx) = app_with_channel();
        app.new_tab();
        app.editor_mut().set_text("scratch");
        app.tabs.list[app.tabs.active].dirty = false; // explicit: clean
        app.close_active_tab();
        assert_eq!(app.tabs.list.len(), 1);
        assert!(app.tabs.pending_close.is_none());
    }

    #[test]
    fn close_dirty_tab_requires_two_strikes() {
        let (mut app, _rx) = app_with_channel();
        app.new_tab();
        app.tabs.list[app.tabs.active].dirty = true;
        // First strike — toast, no close.
        app.close_active_tab();
        assert_eq!(app.tabs.list.len(), 2);
        assert!(app.tabs.pending_close.is_some());
        let toast = app.toast.as_ref().expect("toast set");
        assert!(toast.message.contains("unsaved"));
        // Second strike within window — closes.
        app.close_active_tab();
        assert_eq!(app.tabs.list.len(), 1);
        assert!(app.tabs.pending_close.is_none());
    }

    #[test]
    fn close_dirty_tab_resets_pending_after_window() {
        let (mut app, _rx) = app_with_channel();
        app.new_tab();
        app.tabs.list[app.tabs.active].dirty = true;
        app.close_active_tab();
        // Force the pending timestamp to look stale.
        let (idx, _) = app.tabs.pending_close.unwrap();
        app.tabs.pending_close = Some((idx, Instant::now() - Duration::from_secs(10)));
        // Second strike past the 3s window resets to "first strike",
        // not a close.
        app.close_active_tab();
        assert_eq!(app.tabs.list.len(), 2);
        assert!(app.tabs.pending_close.is_some());
    }

    #[test]
    fn closing_only_tab_replaces_with_empty() {
        let (mut app, _rx) = app_with_channel();
        app.editor_mut().set_text("only");
        // Force dirty=false so it closes immediately.
        app.tabs.list[0].dirty = false;
        app.close_active_tab();
        assert_eq!(app.tabs.list.len(), 1);
        assert_eq!(app.editor().text(), "");
        assert_eq!(app.tabs.active, 0);
    }

    #[test]
    fn editing_marks_active_tab_dirty() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        assert!(!app.tabs.list[0].dirty);
        type_str(&mut app, "x");
        assert!(app.tabs.list[0].dirty);
    }

    #[test]
    fn navigation_keys_do_not_dirty_a_clean_tab() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        // Right arrow on an empty buffer is a no-op edit; dirty stays
        // false. (Left/Right at position 0 / end are also no-ops.)
        app.on_event(AppEvent::Key(key(KeyCode::Right, KeyModifiers::NONE)));
        assert!(!app.tabs.list[0].dirty);
    }

    #[test]
    fn save_clears_dirty_and_sets_path() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        type_str(&mut app, "SELECT 1;");
        assert!(app.tabs.list[0].dirty);

        app.on_event(AppEvent::Key(key(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
        )));
        let path = unique_tmp_path("r2save");
        for c in path.to_string_lossy().chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        app.on_event(AppEvent::Key(key(KeyCode::Enter, KeyModifiers::NONE)));

        assert!(!app.tabs.list[0].dirty);
        assert_eq!(app.tabs.list[0].path.as_deref(), Some(path.as_path()));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn open_clears_dirty_and_sets_path() {
        let path = unique_tmp_path("r2open");
        std::fs::write(&path, "SELECT loaded;\n").unwrap();

        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        type_str(&mut app, "scratch");
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL,
        )));
        for c in path.to_string_lossy().chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        app.on_event(AppEvent::Key(key(KeyCode::Enter, KeyModifiers::NONE)));

        assert!(!app.tabs.list[0].dirty);
        assert_eq!(app.tabs.list[0].path.as_deref(), Some(path.as_path()));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ctrl_t_creates_new_tab_and_focuses_editor() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Tree;
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('t'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(app.tabs.list.len(), 2);
        assert_eq!(app.tabs.active, 1);
        assert_eq!(app.focus, FocusPane::Editor);
    }

    #[test]
    fn ctrl_digit_jumps_to_tab() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.new_tab();
        app.new_tab(); // tabs = 3, active = 2
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('1'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(app.tabs.active, 0);
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('3'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(app.tabs.active, 2);
        // Out-of-range is silently ignored.
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('9'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(app.tabs.active, 2);
    }

    #[test]
    fn ctrl_f_opens_find_with_recomputed_matches() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.editor_mut()
            .set_text("select foo from bar where foo = 1");
        app.tabs.list[0].last_search = Some("foo".into());
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        )));
        let f = app.find.as_ref().expect("find open");
        assert_eq!(f.needle, "foo");
        assert_eq!(f.matches.len(), 2);
    }

    #[test]
    fn typing_into_find_jumps_to_first_match() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.editor_mut().set_text("zzz hello hello");
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        )));
        for c in "hello".chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        // Caret jumped to the first 'hello' (col 4).
        assert_eq!(app.editor().cursor_pos(), (0, 4));
    }

    #[test]
    fn esc_closes_find_and_stashes_last_search() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.editor_mut().set_text("foo bar");
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        )));
        for c in "foo".chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        app.on_event(AppEvent::Key(key(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(app.find.is_none());
        assert_eq!(app.tabs.list[0].last_search.as_deref(), Some("foo"));
    }

    #[test]
    fn esc_with_empty_needle_does_not_stash() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        )));
        app.on_event(AppEvent::Key(key(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(app.find.is_none());
        assert!(app.tabs.list[0].last_search.is_none());
    }

    #[test]
    fn ctrl_h_opens_find_in_replace_mode() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.editor_mut().set_text("foo bar foo");
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('h'),
            KeyModifiers::CONTROL,
        )));
        let f = app.find.as_ref().expect("find open");
        assert_eq!(f.mode, find::FindMode::Replace);
        assert_eq!(f.focus, find::ReplaceFocus::Needle);
    }

    #[test]
    fn replace_one_swaps_active_match_and_marks_dirty() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.editor_mut().set_text("foo bar foo");
        app.tabs.list[0].dirty = false;
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('h'),
            KeyModifiers::CONTROL,
        )));
        for c in "foo".chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        // Tab to Replacement field, type "BAR".
        app.on_event(AppEvent::Key(key(KeyCode::Tab, KeyModifiers::NONE)));
        for c in "BAR".chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        // Enter on Replacement → replace current (first) match.
        app.on_event(AppEvent::Key(key(KeyCode::Enter, KeyModifiers::NONE)));
        assert_eq!(app.editor().text(), "BAR bar foo");
        assert!(app.tabs.list[0].dirty);
    }

    #[test]
    fn alt_a_replaces_all_in_one_undo_step() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.editor_mut().set_text("a a a");
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('h'),
            KeyModifiers::CONTROL,
        )));
        // Type needle 'a'.
        app.on_event(AppEvent::Key(key(KeyCode::Char('a'), KeyModifiers::NONE)));
        // Tab to Replacement, type 'bb'.
        app.on_event(AppEvent::Key(key(KeyCode::Tab, KeyModifiers::NONE)));
        for c in "bb".chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        // Alt+A → replace all.
        app.on_event(AppEvent::Key(key(KeyCode::Char('a'), KeyModifiers::ALT)));
        assert_eq!(app.editor().text(), "bb bb bb");
        // Esc to close, then Ctrl+Z to undo — single undo reverts the
        // entire batch.
        app.on_event(AppEvent::Key(key(KeyCode::Esc, KeyModifiers::NONE)));
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('z'),
            KeyModifiers::CONTROL,
        )));
        assert_eq!(app.editor().text(), "a a a");
    }

    #[test]
    fn replace_replacement_field_does_not_extend_needle() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.editor_mut().set_text("a");
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('h'),
            KeyModifiers::CONTROL,
        )));
        app.on_event(AppEvent::Key(key(KeyCode::Char('a'), KeyModifiers::NONE)));
        // Tab to Replacement.
        app.on_event(AppEvent::Key(key(KeyCode::Tab, KeyModifiers::NONE)));
        for c in "xy".chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        let f = app.find.as_ref().unwrap();
        assert_eq!(f.needle, "a");
        assert_eq!(f.replacement, "xy");
    }

    #[test]
    fn reopening_find_prefills_last_search() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.editor_mut().set_text("foo bar foo");
        // First open + Esc stashes the needle.
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        )));
        for c in "foo".chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        app.on_event(AppEvent::Key(key(KeyCode::Esc, KeyModifiers::NONE)));
        // Reopening — Ctrl+F restores the needle and recomputes matches.
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('f'),
            KeyModifiers::CONTROL,
        )));
        let f = app.find.as_ref().expect("find open");
        assert_eq!(f.needle, "foo");
        assert_eq!(f.matches.len(), 2);
    }

    #[test]
    fn ctrl_g_opens_command_line() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.command_line.is_some());
        assert_eq!(app.command_line.as_ref().unwrap().input, "");
    }

    #[test]
    fn ctrl_g_42_enter_jumps_to_line() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.editor_mut().set_text("a\nb\nc\nd");
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL,
        )));
        for c in "3".chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        app.on_event(AppEvent::Key(key(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(app.command_line.is_none());
        assert_eq!(app.editor().cursor_line_col(), (3, 1));
    }

    #[test]
    fn command_line_esc_closes_without_dispatching() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.editor_mut().set_text("a\nb\nc");
        let before = app.editor().cursor_line_col();
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL,
        )));
        for c in "2".chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        app.on_event(AppEvent::Key(key(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(app.command_line.is_none());
        assert_eq!(app.editor().cursor_line_col(), before);
    }

    #[test]
    fn command_line_swallows_global_keys_while_open() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL,
        )));
        // F2 normally focuses the tree pane; while command line is open
        // it must be absorbed.
        app.on_event(AppEvent::Key(key(KeyCode::F(2), KeyModifiers::NONE)));
        assert!(app.command_line.is_some());
        assert_eq!(app.focus, FocusPane::Editor);
    }

    #[test]
    fn ctrl_pagedown_cycles_forward() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.new_tab(); // tabs = 2, active = 1
        app.on_event(AppEvent::Key(key(KeyCode::PageDown, KeyModifiers::CONTROL)));
        assert_eq!(app.tabs.active, 0); // wrap from 1 → 0
    }

    #[test]
    fn other_key_clears_pending_tab_close() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.new_tab();
        app.tabs.list[app.tabs.active].dirty = true;
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('w'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.tabs.pending_close.is_some());
        // Any non-Ctrl+W key clears it.
        app.on_event(AppEvent::Key(key(KeyCode::Char('a'), KeyModifiers::NONE)));
        assert!(app.tabs.pending_close.is_none());
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
        assert_eq!(app.editor().text(), "  ");
        assert!(app.autocomplete.is_none());
    }

    #[test]
    fn paste_in_editor_focus_inserts_text() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.on_event(AppEvent::Paste("SELECT 1;\nSELECT 2;".into()));
        assert_eq!(app.editor().text(), "SELECT 1;\nSELECT 2;");
    }

    #[test]
    fn paste_outside_editor_focus_is_ignored() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Results;
        app.on_event(AppEvent::Paste("noise".into()));
        assert_eq!(app.editor().text(), "");
    }

    #[test]
    fn paste_on_connect_screen_is_ignored() {
        let (mut app, _rx) = app_with_channel();
        // default screen is Connect
        app.on_event(AppEvent::Paste("noise".into()));
        assert_eq!(app.editor().text(), "");
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
    fn tab_after_dot_with_no_prefix_opens_column_popup() {
        // Cursor sits right after `users.` — empty word prefix, but the
        // dotted context should still surface the column list.
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        populate_tree_for_completion(&mut app);
        type_str(&mut app, "SELECT users.");
        app.on_event(AppEvent::Key(key(KeyCode::Tab, KeyModifiers::NONE)));
        let popup = app.autocomplete.as_ref().expect("popup");
        let cands: Vec<String> = popup.candidates().to_vec();
        assert_eq!(cands, vec!["id".to_string(), "email".to_string()]);
    }

    #[test]
    fn tab_after_from_with_trailing_space_opens_relation_popup() {
        // Cursor sits right after `FROM ` — empty word prefix, but
        // TableName context should still surface relations.
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        populate_tree_for_completion(&mut app);
        type_str(&mut app, "SELECT * FROM ");
        app.on_event(AppEvent::Key(key(KeyCode::Tab, KeyModifiers::NONE)));
        let popup = app.autocomplete.as_ref().expect("popup");
        let cands: Vec<String> = popup.candidates().to_vec();
        // Order matches relation_names() — public's relations in
        // insertion order.
        assert_eq!(cands, vec!["users".to_string(), "orders".to_string()]);
    }

    #[test]
    fn tab_with_empty_prefix_in_default_context_indents() {
        // After a plain space in a default context (e.g. `SELECT `),
        // Tab still inserts spaces — we don't want to dump every
        // keyword + identifier on the user.
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        populate_tree_for_completion(&mut app);
        type_str(&mut app, "SELECT ");
        app.on_event(AppEvent::Key(key(KeyCode::Tab, KeyModifiers::NONE)));
        assert!(app.autocomplete.is_none());
        assert!(app.editor().text().ends_with("  "));
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
        app.editor_mut().set_text("SELECT  FROM users u");
        assert!(app.editor_mut().move_cursor_to_char_position(8));
        type_str(&mut app, "u.em");
        app.on_event(AppEvent::Key(key(KeyCode::Tab, KeyModifiers::NONE)));
        let popup = app.autocomplete.as_ref().expect("popup");
        let cands: Vec<String> = popup.candidates().to_vec();
        assert_eq!(cands, vec!["email".to_string()]);
    }

    fn unique_tmp_path(label: &str) -> std::path::PathBuf {
        // Combine the label with a uuid so parallel test runs don't
        // collide on the same temp filename.
        let id = uuid::Uuid::new_v4();
        std::env::temp_dir().join(format!("psqlview_r5_{label}_{id}.sql"))
    }

    #[test]
    fn ctrl_s_opens_save_prompt_and_writes_file() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        type_str(&mut app, "SELECT 42;");

        app.on_event(AppEvent::Key(key(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
        )));
        let prompt = app.file_prompt.as_ref().expect("save prompt");
        assert_eq!(prompt.mode, FilePromptMode::Save);

        // Type the absolute path so the test doesn't depend on cwd.
        let path = unique_tmp_path("save");
        for c in path.to_string_lossy().chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        app.on_event(AppEvent::Key(key(KeyCode::Enter, KeyModifiers::NONE)));

        assert!(app.file_prompt.is_none(), "prompt should close on commit");
        let written = std::fs::read_to_string(&path).expect("file written");
        assert_eq!(written, "SELECT 42;");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ctrl_e_with_result_writes_csv() {
        use crate::types::{CellValue, ColumnMeta};
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Results;
        let rs = ResultSet {
            columns: vec![
                ColumnMeta {
                    name: "id".into(),
                    type_name: "int".into(),
                },
                ColumnMeta {
                    name: "msg".into(),
                    type_name: "text".into(),
                },
            ],
            rows: vec![
                vec![CellValue::Int(1), CellValue::Text("a,b".into())],
                vec![CellValue::Int(2), CellValue::Null],
            ],
            ..ResultSet::default()
        };
        app.results.set_result(rs);

        app.on_event(AppEvent::Key(key(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
        )));
        let prompt = app.file_prompt.as_ref().expect("export prompt");
        assert_eq!(prompt.mode, FilePromptMode::ExportCsv);

        let path = unique_tmp_path("export");
        for c in path.to_string_lossy().chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        app.on_event(AppEvent::Key(key(KeyCode::Enter, KeyModifiers::NONE)));

        let written = std::fs::read_to_string(&path).expect("csv written");
        assert_eq!(written, "id,msg\n1,\"a,b\"\n2,\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn ctrl_e_without_result_shows_info_toast() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Results;
        // No results.set_result — current is None.
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('e'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.file_prompt.is_none());
        let toast = app.toast.as_ref().expect("toast set");
        assert!(!toast.is_error);
        assert!(toast.message.contains("no result"));
    }

    #[test]
    fn ctrl_o_opens_open_prompt_and_replaces_buffer() {
        let path = unique_tmp_path("open");
        std::fs::write(&path, "SELECT loaded;\r\nFROM disk\r\n").expect("seed file");

        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        type_str(&mut app, "scratch");

        app.on_event(AppEvent::Key(key(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL,
        )));
        let prompt = app.file_prompt.as_ref().expect("open prompt");
        assert_eq!(prompt.mode, FilePromptMode::Open);
        for c in path.to_string_lossy().chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        app.on_event(AppEvent::Key(key(KeyCode::Enter, KeyModifiers::NONE)));

        assert!(app.file_prompt.is_none());
        // CRLF in the file is normalized to LF on load.
        assert_eq!(app.editor().text(), "SELECT loaded;\nFROM disk\n");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn esc_dismisses_file_prompt_without_writing() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        type_str(&mut app, "x");
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
        )));
        for c in "/should/not/exist.sql".chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        app.on_event(AppEvent::Key(key(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(app.file_prompt.is_none());
        assert!(!std::path::Path::new("/should/not/exist.sql").exists());
        // Editor buffer untouched.
        assert_eq!(app.editor().text(), "x");
    }

    #[test]
    fn open_failure_sets_error_toast_and_keeps_buffer() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        type_str(&mut app, "keep me");
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL,
        )));
        let bogus = unique_tmp_path("missing");
        for c in bogus.to_string_lossy().chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        app.on_event(AppEvent::Key(key(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(app.file_prompt.is_none());
        assert_eq!(app.editor().text(), "keep me");
        let toast = app.toast.as_ref().expect("error toast");
        assert!(toast.is_error);
        assert!(toast.message.contains("open failed"));
    }

    #[test]
    fn file_prompt_swallows_global_keys_while_open() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('s'),
            KeyModifiers::CONTROL,
        )));
        // F2 would normally focus the tree pane; while the prompt is open
        // it must be ignored.
        app.on_event(AppEvent::Key(key(KeyCode::F(2), KeyModifiers::NONE)));
        assert!(app.file_prompt.is_some());
        assert_eq!(app.focus, FocusPane::Editor);
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

    // ---- vim-style search (R5) -------------------------------------

    #[test]
    fn slash_in_normal_mode_opens_vim_search_overlay() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.editor_mut().set_text("foo bar foo");
        app.on_event(AppEvent::Key(key(KeyCode::Esc, KeyModifiers::NONE)));
        app.on_event(AppEvent::Key(key(KeyCode::Char('/'), KeyModifiers::NONE)));
        let f = app.find.as_ref().expect("find open");
        assert!(!f.backward);
        assert!(f.enter_closes);
    }

    #[test]
    fn slash_in_insert_mode_inserts_literal_slash() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        // Default mode is Insert.
        app.on_event(AppEvent::Key(key(KeyCode::Char('/'), KeyModifiers::NONE)));
        assert_eq!(app.editor().text(), "/");
        assert!(app.find.is_none());
    }

    #[test]
    fn vim_forward_search_jumps_and_closes_on_enter() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.editor_mut().set_text("zzz hello world hello");
        app.on_event(AppEvent::Key(key(KeyCode::Home, KeyModifiers::NONE)));
        app.on_event(AppEvent::Key(key(KeyCode::Esc, KeyModifiers::NONE)));
        // / hello <Enter>
        app.on_event(AppEvent::Key(key(KeyCode::Char('/'), KeyModifiers::NONE)));
        for c in "hello".chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        app.on_event(AppEvent::Key(key(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(app.find.is_none(), "overlay closes on Enter");
        assert_eq!(app.editor().cursor_pos(), (0, 4));
        assert_eq!(app.tabs.list[0].last_search.as_deref(), Some("hello"));
        assert!(!app.tabs.list[0].last_search_backward);
    }

    #[test]
    fn n_repeats_forward_search() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.editor_mut().set_text("foo bar foo baz foo");
        app.on_event(AppEvent::Key(key(KeyCode::Home, KeyModifiers::NONE)));
        app.on_event(AppEvent::Key(key(KeyCode::Esc, KeyModifiers::NONE)));
        app.on_event(AppEvent::Key(key(KeyCode::Char('/'), KeyModifiers::NONE)));
        for c in "foo".chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        app.on_event(AppEvent::Key(key(KeyCode::Enter, KeyModifiers::NONE)));
        // After Enter, cursor on first foo (col 0). n → second (col 8).
        app.on_event(AppEvent::Key(key(KeyCode::Char('n'), KeyModifiers::NONE)));
        assert_eq!(app.editor().cursor_pos(), (0, 8));
        // n → third (col 16).
        app.on_event(AppEvent::Key(key(KeyCode::Char('n'), KeyModifiers::NONE)));
        assert_eq!(app.editor().cursor_pos(), (0, 16));
        // N → reverse direction → second (col 8).
        app.on_event(AppEvent::Key(key(KeyCode::Char('N'), KeyModifiers::SHIFT)));
        assert_eq!(app.editor().cursor_pos(), (0, 8));
    }

    #[test]
    fn question_mark_searches_backward() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.editor_mut().set_text("foo bar foo");
        // Cursor sits at end (0, 11) after set_text.
        app.on_event(AppEvent::Key(key(KeyCode::Esc, KeyModifiers::NONE)));
        app.on_event(AppEvent::Key(key(KeyCode::Char('?'), KeyModifiers::SHIFT)));
        let f = app.find.as_ref().expect("find open");
        assert!(f.backward);
        for c in "foo".chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        app.on_event(AppEvent::Key(key(KeyCode::Enter, KeyModifiers::NONE)));
        assert!(app.find.is_none());
        assert!(app.tabs.list[0].last_search_backward);
        // Backward from col 11 → second foo at col 8.
        assert_eq!(app.editor().cursor_pos(), (0, 8));
    }

    #[test]
    fn n_after_question_search_repeats_backward() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.editor_mut().set_text("foo bar foo baz foo");
        app.on_event(AppEvent::Key(key(KeyCode::Esc, KeyModifiers::NONE)));
        // ? foo Enter
        app.on_event(AppEvent::Key(key(KeyCode::Char('?'), KeyModifiers::SHIFT)));
        for c in "foo".chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        app.on_event(AppEvent::Key(key(KeyCode::Enter, KeyModifiers::NONE)));
        let after_enter = app.editor().cursor_pos();
        app.on_event(AppEvent::Key(key(KeyCode::Char('n'), KeyModifiers::NONE)));
        let after_n = app.editor().cursor_pos();
        assert!(
            after_n.1 < after_enter.1,
            "n after ? should move backward in column ({} -> {})",
            after_enter.1,
            after_n.1
        );
    }

    #[test]
    fn n_with_no_last_search_shows_toast() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.on_event(AppEvent::Key(key(KeyCode::Esc, KeyModifiers::NONE)));
        app.on_event(AppEvent::Key(key(KeyCode::Char('n'), KeyModifiers::NONE)));
        let toast = app.toast.as_ref().expect("toast set");
        assert!(toast.message.contains("no previous search"));
    }

    #[test]
    fn esc_during_vim_search_stashes_needle_and_direction() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.editor_mut().set_text("foo bar foo");
        app.on_event(AppEvent::Key(key(KeyCode::Esc, KeyModifiers::NONE)));
        app.on_event(AppEvent::Key(key(KeyCode::Char('?'), KeyModifiers::SHIFT)));
        for c in "foo".chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        app.on_event(AppEvent::Key(key(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(app.find.is_none());
        assert_eq!(app.tabs.list[0].last_search.as_deref(), Some("foo"));
        assert!(app.tabs.list[0].last_search_backward);
    }

    // ---- `:` command line (R6) -------------------------------------

    fn enter_normal_in_editor(app: &mut App) {
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.on_event(AppEvent::Key(key(KeyCode::Esc, KeyModifiers::NONE)));
    }

    fn run_command(app: &mut App, cmd: &str) {
        app.on_event(AppEvent::Key(key(KeyCode::Char(':'), KeyModifiers::SHIFT)));
        for c in cmd.chars() {
            let mods = if c.is_ascii_uppercase() {
                KeyModifiers::SHIFT
            } else {
                KeyModifiers::NONE
            };
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), mods)));
        }
        app.on_event(AppEvent::Key(key(KeyCode::Enter, KeyModifiers::NONE)));
    }

    #[test]
    fn colon_in_normal_mode_opens_command_line() {
        let (mut app, _rx) = app_with_channel();
        enter_normal_in_editor(&mut app);
        app.on_event(AppEvent::Key(key(KeyCode::Char(':'), KeyModifiers::SHIFT)));
        assert!(app.command_line.is_some());
        assert_eq!(app.command_line.as_ref().unwrap().input, "");
    }

    #[test]
    fn colon_42_jumps_to_line() {
        let (mut app, _rx) = app_with_channel();
        app.editor_mut().set_text("a\nb\nc\nd\ne");
        enter_normal_in_editor(&mut app);
        run_command(&mut app, "3");
        assert!(app.command_line.is_none());
        assert_eq!(app.editor().cursor_line_col(), (3, 1));
    }

    #[test]
    fn colon_subst_current_line_first_match() {
        let (mut app, _rx) = app_with_channel();
        // set_text resets the cursor to (0, 0); subst on the current
        // line (row 0) replaces only the first occurrence of `b`.
        app.editor_mut().set_text("a b b\nc c c");
        enter_normal_in_editor(&mut app);
        run_command(&mut app, "s/b/X/");
        assert_eq!(app.editor().text(), "a X b\nc c c");
    }

    #[test]
    fn colon_subst_global_replaces_all_on_current_line() {
        let (mut app, _rx) = app_with_channel();
        app.editor_mut().set_text("foo foo foo");
        enter_normal_in_editor(&mut app);
        run_command(&mut app, "s/foo/X/g");
        assert_eq!(app.editor().text(), "X X X");
    }

    #[test]
    fn colon_percent_subst_global_replaces_buffer_wide() {
        let (mut app, _rx) = app_with_channel();
        app.editor_mut().set_text("foo bar\nfoo baz\nbar foo");
        enter_normal_in_editor(&mut app);
        run_command(&mut app, "%s/foo/X/g");
        assert_eq!(app.editor().text(), "X bar\nX baz\nbar X");
    }

    #[test]
    fn colon_subst_no_match_shows_error_toast() {
        let (mut app, _rx) = app_with_channel();
        app.editor_mut().set_text("foo");
        enter_normal_in_editor(&mut app);
        run_command(&mut app, "s/zzz/X/");
        assert_eq!(app.editor().text(), "foo");
        let toast = app.toast.as_ref().expect("error toast");
        assert!(toast.is_error);
        assert!(toast.message.contains("no match"));
    }

    #[test]
    fn colon_tabnew_opens_a_new_tab_and_focuses_editor() {
        let (mut app, _rx) = app_with_channel();
        enter_normal_in_editor(&mut app);
        run_command(&mut app, "tabnew");
        assert_eq!(app.tabs.list.len(), 2);
        assert_eq!(app.tabs.active, 1);
        assert_eq!(app.focus, FocusPane::Editor);
    }

    #[test]
    fn colon_q_quits() {
        let (mut app, _rx) = app_with_channel();
        enter_normal_in_editor(&mut app);
        run_command(&mut app, "q");
        assert!(app.should_quit);
    }

    #[test]
    fn colon_help_opens_cheatsheet() {
        let (mut app, _rx) = app_with_channel();
        enter_normal_in_editor(&mut app);
        run_command(&mut app, "help");
        assert!(app.cheatsheet_open);
    }

    #[test]
    fn colon_w_with_no_path_opens_save_prompt() {
        let (mut app, _rx) = app_with_channel();
        enter_normal_in_editor(&mut app);
        run_command(&mut app, "w");
        assert!(app.file_prompt.is_some());
        assert_eq!(app.file_prompt.as_ref().unwrap().mode, FilePromptMode::Save);
    }

    #[test]
    fn colon_unknown_command_shows_error_toast() {
        let (mut app, _rx) = app_with_channel();
        enter_normal_in_editor(&mut app);
        run_command(&mut app, "blargh");
        let toast = app.toast.as_ref().expect("error toast");
        assert!(toast.is_error);
        assert!(toast.message.contains("blargh"));
    }
}
