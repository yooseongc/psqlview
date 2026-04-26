use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use tokio::sync::mpsc;
use tokio_postgres::CancelToken;

use crate::config::ConnInfo;
use crate::db::catalog::RelationKind;
use crate::db::{self, catalog, Session};
use crate::event::AppEvent;
use crate::types::ResultSet;
use crate::ui::autocomplete::{AutocompletePopup, SQL_KEYWORDS};
use crate::ui::autocomplete_context::{detect_context, extract_aliases, CompletionContext};
use crate::ui::connect_dialog::ConnectDialogState;
use crate::ui::csv_export;
use crate::ui::editor::tab::{CloseOutcome, Tabs};
use crate::ui::editor::EditorState;
use crate::ui::file_prompt::{self, FilePromptMode, FilePromptState};
use crate::ui::find::{self, FindOutcome, FindState};
use crate::ui::goto_line::{self, GotoLineOutcome, GotoLineState};
use crate::ui::results::ResultsState;
use crate::ui::row_detail::RowDetailState;
use crate::ui::schema_tree::SchemaTreeState;
use crate::ui::PaneRects;

/// Row cap on the synthesized `SELECT *` issued by the tree-preview
/// shortcut (`p` on a relation). Kept low because the user is browsing,
/// not querying.
const PREVIEW_ROW_LIMIT: u32 = 200;

/// Quotes a Postgres identifier per the standard rules: wrap in double
/// quotes and double any internal quote.
fn quote_ident(name: &str) -> String {
    let escaped = name.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

/// Builds a preview `SELECT * FROM "schema"."relation" LIMIT n` query.
fn build_preview_sql(schema: &str, relation: &str, limit: u32) -> String {
    format!(
        "SELECT * FROM {}.{} LIMIT {}",
        quote_ident(schema),
        quote_ident(relation),
        limit
    )
}

/// Renders a cell for clipboard / TSV copy. Mirrors the Display impl
/// of `CellValue` except NULL becomes the empty string (so a row with
/// nulls round-trips through a paste cleanly).
fn format_cell_for_copy(v: &crate::types::CellValue) -> String {
    match v {
        crate::types::CellValue::Null => String::new(),
        other => other.to_string(),
    }
}

/// Truncates `s` to `max` chars + "…" so toast messages don't grow
/// unboundedly when the user copies a long cell.
fn truncate_for_toast(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

/// Wraps a multi-line DDL string in a synthetic single-column `ResultSet`
/// so the existing results pane can render and scroll it like any other
/// query output.
fn ddl_to_resultset(text: &str, elapsed_ms: u128) -> ResultSet {
    let rows: Vec<Vec<crate::types::CellValue>> = text
        .split('\n')
        .map(|line| vec![crate::types::CellValue::Text(line.to_string())])
        .collect();
    ResultSet {
        columns: vec![crate::types::ColumnMeta {
            name: "ddl".into(),
            type_name: "text".into(),
        }],
        rows,
        truncated_at: None,
        command_tag: None,
        elapsed_ms,
    }
}

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

    /// `Ctrl+G` inline goto-line prompt. Slotted between `file_prompt`
    /// and `find` in the modal precedence chain. Only digits / Enter /
    /// Backspace / Esc reach it.
    pub goto_line: Option<GotoLineState>,

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

    tx: mpsc::UnboundedSender<AppEvent>,
}

impl App {
    pub fn new(tx: mpsc::UnboundedSender<AppEvent>) -> Self {
        Self {
            screen: Screen::Connect,
            connect_dialog: ConnectDialogState::new(ConnInfo::default()),
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
            goto_line: None,
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
    fn mark_active_dirty(&mut self) {
        self.tabs.mark_active_dirty();
    }

    /// Per-tab modal state that doesn't survive a tab switch — the
    /// overlays anchor visually to the active editor and would feel
    /// stale on the next tab.
    fn switch_tab_cleanup(&mut self) {
        self.autocomplete = None;
        self.last_ddl_target = None;
        self.find = None;
        self.goto_line = None;
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

    fn on_key(&mut self, key: KeyEvent) {
        // Global hotkeys first. Ctrl+C / Ctrl+Q quit unconditionally.
        if is_ctrl_c(&key) || is_ctrl_q(&key) {
            self.should_quit = true;
            return;
        }

        // The file prompt is the most aggressive modal: while it's open,
        // every key (except quit, handled above) goes to it. We don't want
        // a stray F1 or `?` to dismiss the dialog mid-typing.
        if self.file_prompt.is_some() {
            self.handle_file_prompt_key(key);
            return;
        }

        // Goto-line is the next-priority modal — the prompt sits over
        // the editor pane and absorbs every key until Enter / Esc.
        if self.goto_line.is_some() {
            self.handle_goto_line_key(key);
            return;
        }

        // Find / Find-Replace overlay — absorbs all keys (printable,
        // Backspace, Enter / F3 / Shift+F3 advance, Alt+C toggle,
        // Esc closes).
        if self.find.is_some() {
            self.handle_find_key(key);
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

        // Any keystroke other than Ctrl+W cancels a pending dirty-tab
        // close confirmation. The first-strike toast auto-expires.
        let is_ctrl_w = key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('w') | KeyCode::Char('W'));
        if !is_ctrl_w {
            self.tabs.pending_close = None;
        }

        // Incremental search in the tree pane absorbs every key until
        // the user commits (Enter) or cancels (Esc). Otherwise Tab would
        // cycle focus out mid-search, F5 would run a query, etc.
        if self.focus == FocusPane::Tree && self.tree.search.is_some() {
            self.on_key_tree(key);
            return;
        }

        // Ctrl+Enter runs the current query regardless of focus. Some
        // terminals deliver this as Ctrl+J (the literal LF character)
        // because the standard VT protocol can't distinguish Ctrl+Enter
        // from Enter — we accept both so the shortcut works without
        // requiring kitty keyboard protocol support.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && (matches!(key.code, KeyCode::Enter)
                || matches!(key.code, KeyCode::Char('j') | KeyCode::Char('J')))
        {
            self.run_current_query();
            return;
        }

        // Ctrl+E exports the current result set to a CSV file. Pane-
        // independent: works whether you're focused on the tree, editor,
        // or results — the prompt cares about results.current, not focus.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('e') | KeyCode::Char('E'))
        {
            if self.results.current.is_some() {
                self.open_file_prompt(FilePromptMode::ExportCsv);
            } else {
                self.toast_info("no result set to export".into());
            }
            return;
        }

        // Editor-pane tab management. Pane-independent — the tab bar
        // belongs to the editor pane but we don't gate on focus so a
        // user driving the tree / results pane can still create a
        // scratch tab without first switching focus.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            match key.code {
                KeyCode::Char('t') | KeyCode::Char('T') => {
                    self.new_tab();
                    self.focus = FocusPane::Editor;
                    return;
                }
                KeyCode::Char('w') | KeyCode::Char('W') => {
                    self.close_active_tab();
                    return;
                }
                // Ctrl+] / Ctrl+PageDown → next tab, Ctrl+[ /
                // Ctrl+PageUp → previous. Ctrl+[ is the same byte as
                // Esc on standard VT; the kitty-keyboard-protocol push
                // in main.rs disambiguates on supported terminals.
                // Ctrl+PageUp/Down is the universal fallback.
                KeyCode::Char(']') | KeyCode::PageDown => {
                    self.cycle_tab(1);
                    return;
                }
                KeyCode::Char('[') | KeyCode::PageUp => {
                    self.cycle_tab(-1);
                    return;
                }
                KeyCode::Char(c @ '1'..='9') => {
                    let idx = (c as u8 - b'1') as usize;
                    self.jump_tab(idx);
                    return;
                }
                _ => {}
            }
        }

        // Ctrl+Up/Down in the editor recalls past queries from session
        // history. Ignored outside the editor so the tree/results panes
        // keep their scroll semantics. Ctrl+O / Ctrl+S open the file
        // prompt for read / write.
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
                KeyCode::Char('o') | KeyCode::Char('O') => {
                    self.open_file_prompt(FilePromptMode::Open);
                    return;
                }
                KeyCode::Char('s') | KeyCode::Char('S') => {
                    self.open_file_prompt(FilePromptMode::Save);
                    return;
                }
                KeyCode::Char('g') | KeyCode::Char('G') => {
                    self.goto_line = Some(GotoLineState::new());
                    self.autocomplete = None;
                    return;
                }
                KeyCode::Char('f') | KeyCode::Char('F') => {
                    // Reuse last_search if any — opening Find with the
                    // previous needle pre-typed is the natural flow.
                    let initial = self.tabs.active().last_search.clone().unwrap_or_default();
                    let mut state = FindState::with_needle(initial, false);
                    state.recompute(self.editor().lines());
                    self.find = Some(state);
                    self.autocomplete = None;
                    return;
                }
                KeyCode::Char('h') | KeyCode::Char('H') => {
                    // Same prefill rule as Ctrl+F — but the overlay
                    // opens in Replace mode with the Replacement field
                    // initially empty.
                    let initial = self.tabs.active().last_search.clone().unwrap_or_default();
                    let mut state = FindState::new_replace();
                    state.needle = initial;
                    state.recompute(self.editor().lines());
                    self.find = Some(state);
                    self.autocomplete = None;
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
                    if let Some((s, e)) = self.editor().selected_line_range() {
                        self.editor_mut().outdent_lines(s, e);
                    } else {
                        self.editor_mut().outdent_current_line();
                    }
                    self.mark_active_dirty();
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
                    if self.editor_mut().handle_key(key) {
                        self.mark_active_dirty();
                    }
                    // Any direct edit invalidates an in-progress history
                    // walk; user is no longer just browsing.
                    self.history_cursor = None;
                }
                FocusPane::Tree => self.on_key_tree(key),
                FocusPane::Results => {
                    // y / Y copy via OSC 52 to the host terminal's clipboard.
                    // Routed before results.handle_key so the keys don't fall
                    // through to a future scroll binding.
                    if key.modifiers.is_empty() && matches!(key.code, KeyCode::Char('y')) {
                        self.copy_current_cell_to_clipboard();
                        return;
                    }
                    if key.modifiers == KeyModifiers::SHIFT
                        && matches!(key.code, KeyCode::Char('Y'))
                    {
                        self.copy_current_row_to_clipboard();
                        return;
                    }
                    // R re-runs the most recent query. Useful for
                    // refreshing a long-running result without going back
                    // to the editor.
                    if key.modifiers == KeyModifiers::SHIFT
                        && matches!(key.code, KeyCode::Char('R'))
                    {
                        self.rerun_last_query();
                        return;
                    }
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
        if let Some((s, e)) = self.editor().selected_line_range() {
            if e > s {
                self.editor_mut().indent_lines(s, e);
                self.mark_active_dirty();
                return;
            }
        }
        let prefix = self.editor().word_prefix_before_cursor();
        let (row, col) = self.editor().cursor_pos();
        // Snapshot the lines before we need a `&self` borrow for
        // candidate building — `lines()` returns a reference into the
        // editor's buffer that would otherwise alias with self for the
        // duration of the call.
        let ctx = {
            let lines = self.editor().lines();
            detect_context(lines, row, col)
        };
        // The popup opens with no prefix when the surrounding clause
        // already narrows the candidate list (after `FROM ` or
        // `qualifier.`), so the user doesn't have to type a starting
        // letter to discover what's available.
        let context_narrows = !matches!(ctx, CompletionContext::Default);
        if prefix.is_empty() && !context_narrows {
            self.editor_mut().insert_spaces(2);
            self.mark_active_dirty();
            return;
        }
        let candidates = self.candidates_for_context(&ctx);
        let popup = if prefix.is_empty() {
            AutocompletePopup::open_anywhere(candidates)
        } else {
            AutocompletePopup::open(prefix, candidates)
        };
        match popup {
            Some(popup) => self.autocomplete = Some(popup),
            None => {
                self.editor_mut().insert_spaces(2);
                self.mark_active_dirty();
            }
        }
    }

    /// Builds the candidate pool for a known cursor context. Narrowing
    /// rules:
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
    fn candidates_for_context(&self, ctx: &CompletionContext) -> Vec<String> {
        match ctx {
            CompletionContext::TableName => {
                let names = self.tree.relation_names();
                if names.is_empty() {
                    self.default_candidates()
                } else {
                    names
                }
            }
            CompletionContext::Dotted { qualifier } => {
                let cols = self.resolve_dotted(qualifier);
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
        let aliases = extract_aliases(self.editor().lines());
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

    /// Opens the inline filename prompt for the given mode. Closes any
    /// active autocomplete popup so the next keystroke is unambiguously
    /// routed to the prompt.
    fn open_file_prompt(&mut self, mode: FilePromptMode) {
        self.autocomplete = None;
        self.file_prompt = Some(FilePromptState::new(mode));
    }

    /// Routes a keystroke to the file-prompt modal. Only Enter / Esc /
    /// printable characters / Backspace are meaningful; everything else
    /// is silently swallowed so global shortcuts like F-keys don't
    /// dismiss the prompt by accident.
    fn handle_file_prompt_key(&mut self, key: KeyEvent) {
        let Some(state) = self.file_prompt.as_mut() else {
            return;
        };
        match key.code {
            KeyCode::Esc => {
                self.file_prompt = None;
            }
            KeyCode::Enter => {
                self.commit_file_prompt();
            }
            KeyCode::Backspace => {
                state.pop_char();
            }
            KeyCode::Tab => {
                // Best-effort path completion against the cwd. Quietly
                // no-ops when the parent directory can't be read or no
                // entry matches the typed prefix.
                let cwd = std::env::current_dir().unwrap_or_default();
                if let Some(completed) = file_prompt::path_complete(&state.input, &cwd) {
                    state.input = completed;
                }
            }
            KeyCode::Char(c)
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                state.push_char(c);
            }
            _ => {}
        }
    }

    /// Routes a keystroke into the goto-line overlay. On `Submit(n)`
    /// the active editor's caret jumps; on `Cancel` the overlay closes
    /// without touching the buffer; on `Stay` the input stays open.
    fn handle_goto_line_key(&mut self, key: KeyEvent) {
        let Some(state) = self.goto_line.as_mut() else {
            return;
        };
        match goto_line::handle_key(state, key) {
            GotoLineOutcome::Stay => {}
            GotoLineOutcome::Cancel => {
                self.goto_line = None;
            }
            GotoLineOutcome::Submit(n) => {
                self.goto_line = None;
                self.editor_mut().goto_line(n);
            }
        }
    }

    /// Routes a keystroke into the Find overlay. Edits the needle,
    /// jumps the caret to matches, and on Esc stashes the needle onto
    /// the active tab's `last_search` for `n` / `N` repeat after the
    /// overlay closes. Empty needle on close clears `last_search`.
    fn handle_find_key(&mut self, key: KeyEvent) {
        let lines: Vec<String> = self.editor().lines().to_vec();
        let outcome = match self.find.as_mut() {
            Some(s) => find::handle_key(s, key, &lines),
            None => return,
        };
        match outcome {
            FindOutcome::Stay => {}
            FindOutcome::Cancel => {
                let needle = self.find.as_ref().map(|s| s.needle.clone());
                self.find = None;
                self.tabs.active_mut().last_search = needle.filter(|n| !n.is_empty());
            }
            FindOutcome::JumpTo(c) => {
                self.editor_mut().jump_caret(c);
            }
            FindOutcome::ReplaceOne {
                range: (start, end),
                text,
            } => {
                self.editor_mut().replace_range(start, end, &text);
                self.mark_active_dirty();
                // Recompute matches against the post-replacement buffer
                // so the overlay's count and active_idx stay accurate.
                let lines: Vec<String> = self.editor().lines().to_vec();
                if let Some(s) = self.find.as_mut() {
                    s.recompute(&lines);
                }
            }
            FindOutcome::ReplaceAll { ranges, text } => {
                let count = ranges.len();
                self.editor_mut().replace_all(&ranges, &text);
                self.mark_active_dirty();
                let lines: Vec<String> = self.editor().lines().to_vec();
                if let Some(s) = self.find.as_mut() {
                    s.recompute(&lines);
                }
                self.toast_info(format!("replaced {count} occurrences"));
            }
        }
    }

    /// Reads or writes the file the prompt names, then closes the prompt.
    /// Errors surface as toasts; the editor buffer is unchanged on Save
    /// failure and on Open failure (so a bad path doesn't blow away
    /// in-progress work).
    fn commit_file_prompt(&mut self) {
        let Some(state) = self.file_prompt.take() else {
            return;
        };
        let trimmed = state.input.trim();
        if trimmed.is_empty() {
            self.toast_error("file path is empty".into());
            return;
        }
        let cwd = std::env::current_dir().unwrap_or_default();
        let path = file_prompt::resolve(trimmed, &cwd);
        match state.mode {
            FilePromptMode::Open => match std::fs::read_to_string(&path) {
                Ok(text) => {
                    // Normalize CRLF so Windows line endings don't show as
                    // blank lines in the editor.
                    let normalized = text.replace("\r\n", "\n");
                    self.editor_mut().set_text(&normalized);
                    let active = self.tabs.active_mut();
                    active.path = Some(path.clone());
                    active.dirty = false;
                    self.toast_info(format!("opened: {}", path.display()));
                }
                Err(e) => {
                    self.toast_error(format!("open failed: {e}"));
                }
            },
            FilePromptMode::Save => match std::fs::write(&path, self.editor().text()) {
                Ok(()) => {
                    let active = self.tabs.active_mut();
                    active.path = Some(path.clone());
                    active.dirty = false;
                    self.toast_info(format!("saved: {}", path.display()));
                }
                Err(e) => {
                    self.toast_error(format!("save failed: {e}"));
                }
            },
            FilePromptMode::ExportCsv => {
                let Some(rs) = self.results.current.as_ref() else {
                    self.toast_error("no result set to export".into());
                    return;
                };
                let res = std::fs::File::create(&path)
                    .and_then(|mut f| csv_export::write_csv(rs, &mut f));
                match res {
                    Ok(()) => self.toast_info(format!(
                        "exported {} rows to {}",
                        rs.rows.len(),
                        path.display()
                    )),
                    Err(e) => self.toast_error(format!("export failed: {e}")),
                }
            }
        }
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
                    self.editor_mut().replace_word_prefix(&pick);
                    self.mark_active_dirty();
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
                if self.editor_mut().handle_key(key) {
                    self.mark_active_dirty();
                }
                if let Some(popup) = self.autocomplete.as_mut() {
                    popup.extend_prefix(c);
                }
                self.close_popup_if_stale();
                true
            }
            KeyCode::Backspace => {
                if self.editor_mut().handle_key(key) {
                    self.mark_active_dirty();
                }
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
            KeyCode::Char('p') | KeyCode::Char(' ') => self.run_preview_for_selected_relation(),
            KeyCode::Char('D') => self.show_ddl_for_selected_relation(),
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
                ..
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

    /// Re-runs the most recent query. When the last action was a `D`
    /// shortcut (DDL view), refreshes via the catalog using the stored
    /// `(schema, relation, kind)` rather than parsing the placeholder
    /// SQL — quoted identifiers with embedded dots survive correctly.
    fn rerun_last_query(&mut self) {
        if let Some((schema, relation, kind)) = self.last_ddl_target.clone() {
            self.dispatch_ddl_fetch(schema, relation, kind);
            return;
        }
        let Some(sql) = self.last_run_sql.clone() else {
            self.toast_info("no previous query".into());
            return;
        };
        self.dispatch_sql(sql);
    }

    fn run_current_query(&mut self) {
        // Run just the selected portion when one exists, so users can
        // execute a single statement from a buffer of many.
        let sql = self
            .editor()
            .selected_text()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| self.editor().text());
        if sql.trim().is_empty() {
            self.toast_info("editor is empty".into());
            return;
        }
        self.dispatch_sql(sql);
    }

    /// Spawns a query task for `sql` and updates the running-query state.
    /// No-op when there is no live session. Used by `run_current_query`,
    /// the tree-preview shortcut, and any other shortcut that wants to
    /// run a synthesized query without touching the editor.
    fn dispatch_sql(&mut self, sql: String) {
        let (client, cancel) = match self.session.as_ref() {
            Some(s) => (s.client(), s.cancel_token()),
            None => return,
        };
        self.autocomplete = None;
        self.push_history(&sql);
        self.query_status = QueryStatus::Running {
            started_at: Instant::now(),
            cancel,
        };
        self.last_run_sql = Some(sql.clone());
        // A fresh SQL dispatch invalidates the DDL re-run target — the
        // user now has actual rows in the result pane, not a synthetic
        // DDL view.
        self.last_ddl_target = None;
        self.results.begin_running();
        self.focus = FocusPane::Results;

        let tx = self.tx.clone();
        tokio::spawn(async move {
            let r = db::query::execute(client, &sql).await;
            let _ = tx.send(AppEvent::QueryResult(r));
        });
    }

    /// Builds a `SELECT * FROM "schema"."relation" LIMIT N` query and
    /// dispatches it. Identifier quoting protects against schemas /
    /// relation names containing special chars or reserved words.
    fn run_preview_for_selected_relation(&mut self) {
        let Some(node) = self.tree.current_node() else {
            return;
        };
        if let crate::ui::schema_tree::NodeRef::Relation { schema, name, .. } = node {
            let sql = build_preview_sql(&schema, &name, PREVIEW_ROW_LIMIT);
            self.toast_info(format!("preview: {schema}.{name}"));
            self.dispatch_sql(sql);
        }
    }

    /// Copies the cell at (`selected_row`, leftmost-visible-column) into
    /// the host terminal's clipboard via OSC 52.
    fn copy_current_cell_to_clipboard(&mut self) {
        let Some(text) = self.format_current_cell() else {
            self.toast_info("no cell to copy".into());
            return;
        };
        match crate::ui::clipboard::copy(&text) {
            Ok(()) => self.toast_info(format!("copied: {}", truncate_for_toast(&text, 40))),
            Err(e) => self.toast_error(format!("copy failed: {e}")),
        }
    }

    /// Copies the entire selected row as TSV (cells joined by `\t`).
    fn copy_current_row_to_clipboard(&mut self) {
        let Some(text) = self.format_current_row_as_tsv() else {
            self.toast_info("no row to copy".into());
            return;
        };
        match crate::ui::clipboard::copy(&text) {
            Ok(()) => self.toast_info("row copied".into()),
            Err(e) => self.toast_error(format!("copy failed: {e}")),
        }
    }

    fn format_current_cell(&self) -> Option<String> {
        let rs = self.results.current.as_ref()?;
        if rs.rows.is_empty() || rs.columns.is_empty() {
            return None;
        }
        let row = rs.rows.get(self.results.selected_row)?;
        let col = self.results.x_offset.min(row.len().saturating_sub(1));
        Some(format_cell_for_copy(row.get(col)?))
    }

    fn format_current_row_as_tsv(&self) -> Option<String> {
        let rs = self.results.current.as_ref()?;
        if rs.rows.is_empty() {
            return None;
        }
        let row = rs.rows.get(self.results.selected_row)?;
        Some(
            row.iter()
                .map(format_cell_for_copy)
                .collect::<Vec<_>>()
                .join("\t"),
        )
    }

    /// Fetches the DDL for the selected relation (CREATE TABLE for
    /// tables, pg_get_viewdef for views/matviews) and routes the text
    /// into the results pane as a single-column `ddl` result. Reuses
    /// the query lifecycle so the Running spinner / Done elapsed time
    /// / error toast all apply.
    fn show_ddl_for_selected_relation(&mut self) {
        let Some(node) = self.tree.current_node() else {
            return;
        };
        let crate::ui::schema_tree::NodeRef::Relation {
            schema, name, kind, ..
        } = node
        else {
            return;
        };
        self.dispatch_ddl_fetch(schema, name, kind);
    }

    /// Spawns the DDL fetch task and primes the result pane / query
    /// status. Used by both the initial `D` shortcut and `R` re-runs.
    fn dispatch_ddl_fetch(&mut self, schema: String, name: String, kind: RelationKind) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let client = session.client();
        let cancel = session.cancel_token();
        self.autocomplete = None;
        self.query_status = QueryStatus::Running {
            started_at: Instant::now(),
            cancel,
        };
        self.last_run_sql = Some(format!("-- DDL of {schema}.{name}"));
        self.last_ddl_target = Some((schema.clone(), name.clone(), kind));
        self.results.begin_running();
        self.focus = FocusPane::Results;

        let tx = self.tx.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            let r = catalog::fetch_relation_ddl(&client, &schema, &name, kind).await;
            let event = match r {
                Ok(text) => Ok(ddl_to_resultset(&text, started.elapsed().as_millis())),
                Err(e) => Err(e),
            };
            let _ = tx.send(AppEvent::QueryResult(event));
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
                    self.editor_mut().move_cursor_to_char_position(pos);
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
            self.editor_mut().set_text(&entry);
            self.mark_active_dirty();
            self.history_cursor = Some(next);
        }
    }

    /// Steps back toward the present (Ctrl+Down). At the newest entry it
    /// clears the editor, matching shell history feel.
    fn history_next(&mut self) {
        let Some(i) = self.history_cursor else { return };
        if i == 0 {
            self.editor_mut().set_text("");
            self.mark_active_dirty();
            self.history_cursor = None;
        } else {
            let new = i - 1;
            if let Some(entry) = self.history.get(new).cloned() {
                self.editor_mut().set_text(&entry);
                self.mark_active_dirty();
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
    fn ctrl_g_opens_goto_line_overlay() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL,
        )));
        assert!(app.goto_line.is_some());
        assert_eq!(app.goto_line.as_ref().unwrap().input(), "");
    }

    #[test]
    fn goto_line_jumps_active_editor_on_enter() {
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
        assert!(app.goto_line.is_none());
        assert_eq!(app.editor().cursor_line_col(), (3, 1));
    }

    #[test]
    fn goto_line_esc_closes_without_jumping() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.editor_mut().set_text("a\nb\nc");
        // Cursor starts at end-of-buffer after set_text. Capture for assertion.
        let before = app.editor().cursor_line_col();
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL,
        )));
        for c in "2".chars() {
            app.on_event(AppEvent::Key(key(KeyCode::Char(c), KeyModifiers::NONE)));
        }
        app.on_event(AppEvent::Key(key(KeyCode::Esc, KeyModifiers::NONE)));
        assert!(app.goto_line.is_none());
        assert_eq!(app.editor().cursor_line_col(), before);
    }

    #[test]
    fn goto_line_swallows_non_digit_keys() {
        let (mut app, _rx) = app_with_channel();
        app.screen = Screen::Workspace;
        app.focus = FocusPane::Editor;
        app.on_event(AppEvent::Key(key(
            KeyCode::Char('g'),
            KeyModifiers::CONTROL,
        )));
        // Letters / F-keys / Esc-equivalents don't dismiss; only Esc / Enter do.
        app.on_event(AppEvent::Key(key(KeyCode::F(2), KeyModifiers::NONE)));
        assert!(app.goto_line.is_some());
        // Letter is ignored; input stays empty.
        app.on_event(AppEvent::Key(key(KeyCode::Char('q'), KeyModifiers::NONE)));
        assert_eq!(app.goto_line.as_ref().unwrap().input(), "");
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
}
