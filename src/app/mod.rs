use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use tokio::sync::mpsc;
use tokio_postgres::CancelToken;

use crate::db::catalog::RelationKind;
use crate::db::{self, catalog, Session};
use crate::event::AppEvent;
use crate::ui::autocomplete::AutocompletePopup;
use crate::ui::cheatsheet::CheatsheetState;
use crate::ui::command_line::CommandLineState;
use crate::ui::connect_dialog::ConnectDialogState;
use crate::ui::editor::tab::{CloseOutcome, Tabs};
use crate::ui::editor::EditorState;
use crate::ui::file_prompt::FilePromptState;
use crate::ui::find::FindState;
use crate::ui::results::ResultsState;
use crate::ui::row_detail::RowDetailState;
use crate::ui::schema_tree::SchemaTreeState;
use crate::ui::substitute_confirm::SubstituteState;
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

    /// Keybinding cheatsheet overlay (open + scroll position).
    pub cheatsheet: CheatsheetState,

    /// Inline filename prompt for `Ctrl+O` / `Ctrl+S`. While `Some`, the
    /// prompt is modal at the application level — every key routes to it.
    pub file_prompt: Option<FilePromptState>,

    /// `:` command line — single-line ex prompt. While `Some`, every
    /// editor key routes to it. Slotted between `file_prompt` and
    /// `find` in the modal precedence chain. `Ctrl+G` opens it too —
    /// `:42` covers goto-line, so the dedicated overlay was retired.
    pub command_line: Option<CommandLineState>,

    /// `Ctrl+F` find / `Ctrl+H` find-replace overlay. While `Some`,
    /// it absorbs editing keystrokes (text into the needle, F3 / Enter
    /// to advance) — slotted right above the editor pane in the modal
    /// precedence chain.
    pub find: Option<FindState>,

    /// `:s/.../c` interactive substitute confirm modal. While `Some`,
    /// absorbs `y` / `n` / `a` / `q` and Esc; everything else is
    /// swallowed. Slotted between `command_line` and `find` so a
    /// pending confirm can't be hijacked by Ctrl+F.
    pub subst_confirm: Option<SubstituteState>,

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
            cheatsheet: CheatsheetState::default(),
            file_prompt: None,
            command_line: None,
            find: None,
            subst_confirm: None,
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
        if self.cheatsheet.open || self.row_detail.open {
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
        if self.cheatsheet.open || self.row_detail.open {
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
mod tests;
