# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project in one line

A Rust TUI that connects to PostgreSQL 14+, lets the user browse schemas
and run ad-hoc SQL, and ships as a single statically-linked musl Linux
binary. Host dev environment is Windows; every build/test runs inside
Docker.

## Non-obvious constraints

- **Target is `x86_64-unknown-linux-musl`** — never build for the host.
  The `rust-toolchain.toml` pins Rust 1.93 and preloads the musl target;
  running `cargo build` on Windows will error out. Use `docker compose
  run --rm builder` or a manual `rust:1.93-alpine` container.
- **TLS is rustls-only.** Do not add `native-tls`, `openssl`,
  `openssl-sys`, or any crate that transitively pulls OpenSSL — it
  breaks the static musl build. If you must support client certs or a
  custom CA, extend `db/connect.rs::build_tls_connector` with rustls
  APIs.
- **rustls 0.23 requires a CryptoProvider.** `main.rs::install_rustls_provider`
  installs the ring provider exactly once before any TLS code runs.
  Integration tests must call `integration_common::init_crypto()` before
  any path that builds a rustls `ClientConfig`. Skipping this produces a
  confusing runtime panic, not a compile error.
- **PostgreSQL 14 is the floor.** When extending `db/catalog.rs`, stick
  to columns / views / functions that exist on PG 14 (no `pg_stat_io`,
  no MERGE, no SQL/JSON path, no `ALTER TABLE ... SET ACCESS METHOD`).
  Verify against all four versions in compose before declaring done.
- **Integration tests are opt-in.** Every integration test is `#[ignore]`
  and gated on `PSQLVIEW_PG_URL`. `cargo test` alone (no `--include-ignored`)
  runs only unit tests. The `tester` compose service sets the env var
  and passes `--include-ignored` automatically.

## Build and test commands

All commands are run from the repository root.

```sh
# Unit tests (fast, no DB required)
docker compose run --rm builder \
  cargo test --lib --target x86_64-unknown-linux-musl

# Single unit test
docker compose run --rm builder \
  cargo test --lib --target x86_64-unknown-linux-musl \
  --  db::query::tests::strip_handles_line_and_block_comments

# cargo check (during development — fastest feedback loop)
docker compose run --rm builder \
  cargo check --target x86_64-unknown-linux-musl --tests

# Release build → ./dist/psqlview (statically linked)
docker compose run --rm builder

# Full integration test matrix against PG 14/15/16/17
docker compose up -d pg14 pg15 pg16 pg17
for v in 14 15 16 17; do
  docker compose run --rm \
    -e PSQLVIEW_PG_URL="postgres://postgres:test@pg${v}:5432/postgres" \
    tester
done

# clippy
docker compose run --rm builder \
  cargo clippy --target x86_64-unknown-linux-musl --all-targets -- -D warnings

# rustfmt
docker compose run --rm builder cargo fmt --all
```

If Docker is not available, nothing in this repo is expected to work on
the host directly — that is intentional.

## Architecture

### Event-driven single-threaded draw loop

The app runs a standard ratatui loop in `main.rs::run_app`:

```
loop {
    terminal.draw(|f| ui::draw(f, &mut app));
    let ev = rx.recv().await;   // one AppEvent
    app.on_event(ev);           // fully synchronous
    drain_additional_events_nonblocking(&mut rx);
    if app.should_quit { break; }
}
```

All async / background work runs in `tokio::spawn`ed tasks that post
`AppEvent`s back through a single `mpsc::UnboundedSender`. `App` itself
contains **no `.await`** — it owns the sender (`tx`) and emits spawns.
This keeps the draw loop starvation-free and lets us cancel, connect,
and load catalog data concurrently without a mutex around `App`.

Event channel taxonomy (`src/event.rs`):
- Input: `Key`, `Mouse`, `Paste(String)`, `Resize`, `Tick` (250 ms)
- Completion: `ConnectResult`, `QueryResult`,
  `SchemasLoaded`, `RelationsLoaded`, `ColumnsLoaded`

Mouse and bracketed paste are enabled by `main.rs::setup_terminal`.
Mouse left-click routes to pane under pointer (via `ui::PaneRects`
updated each frame); wheel scroll routes to the same pane, falling
back to the focused pane if coordinates don't hit any rect. Paste
events are accepted only when the Editor has focus on the Workspace
screen and are inserted via `EditorState::insert_str` (CRLF → LF).

### State machine (src/app/)

`App` is split across `src/app/{mod, keys, query, file_io, autocomplete,
clipboard, history, toasts}.rs` — each `impl App` block lives next to
the state it touches. `mod.rs` owns the struct + lifecycle (`new`,
`on_event`, `on_tick`, `on_mouse`, `on_paste`, tab plumbing,
`begin_connect`, `on_connect_result`); `keys.rs` owns the full modal-
key cascade.

```
Screen::Connect  ──submit──▶  connecting=true  ──ConnectResult(ok)──▶  Screen::Workspace
                                                    │
                                                    └─err──▶ toast, stay on Connect

Workspace + FocusPane(Tree|Editor|Results) + QueryStatus(Idle|Running|Done|Cancelled|Failed)

F5  ──▶ spawn query task ─────────────────▶ QueryResult
Esc while Running ──▶ spawn cancel task (CancelToken.cancel_query)
```

Cancellation is important: tokio-postgres's `CancelToken` needs its own
TLS connector because it opens a *separate* connection to the server's
cancel request port. `App::cancel_running_query` constructs a fresh
rustls `MakeRustlsConnect` per call — it is cheap and avoids sharing the
live session's state.

### DB layer (src/db/)

- `connect.rs`: `tokio_postgres::Config` filled from `ConnInfo`; rustls
  connector built with webpki-roots; after connect, one round-trip to
  `SHOW server_version_num` populates `Session::server_version`.
- `query.rs`: routes input based on the first SQL keyword (after
  stripping `--` / `/* */` comments):
  - **SELECT / WITH / VALUES / TABLE / SHOW / EXPLAIN / FETCH** → typed
    streaming via `query_raw`, rows converted by `convert_cell` into
    `CellValue` enum (bool, int, float, numeric, text, date, time,
    timestamp, timestamptz, json/jsonb, uuid, bytea, inet, unsupported).
    Capped at `ROW_LIMIT = 10_000`.
  - On error, `DbError::format_detailed_with_sql` builds a multi-line
    message including SQLSTATE / DETAIL / HINT / a caret-pointed
    POSITION snippet. `DbError::original_position` exposes just the
    POSITION number so the editor can jump the caret there.
  - **anything else** → `simple_query` so multi-statement DDL / DML
    works; last `CommandComplete` tag bubbles up to the status bar.
- `catalog.rs`: four small async functions that back the schema tree.
  Uses `information_schema.schemata`, `information_schema.columns`,
  `pg_catalog.pg_class` + `pg_namespace`, and `pg_catalog.pg_database`
  — all available unchanged on PG 14+.

### UI layer (src/ui/)

Each submodule owns its own state struct (`EditorState`,
`SchemaTreeState`, `ResultsState`, `ConnectDialogState`) and exposes a
free `draw()` function. State mutation goes through typed methods; the
top-level `App` never touches the ratatui widgets directly.

- `schema_tree.rs` stores a nested `Vec<SchemaEntry>` with
  expanded/loaded flags. `flatten()` produces the flat list for
  rendering *and* selection indexing — keep the order consistent
  between the two, or selection math will desync. Also owns the
  incremental `/` search state (`search`, `last_search`).
- `results.rs` computes column widths from the first 256 rows (`min
  4, max 40`, Unicode-width-aware via `unicode-width`). The query
  result is stored verbatim (insertion order); the optional client-
  side sort (`s`) mutates `current.rows` in place and snapshots the
  pre-sort order in `original_rows` so it can restore on cycle-off.
  EXPLAIN-shaped results (single column named `QUERY PLAN`) are
  detected at `set_result` time and rendered by `draw_explain` with
  per-line node-name + cost/timing styling.
- `editor/` is a self-contained modal SQL editor (no `tui-textarea`).
  `EditorState` owns `TextBuffer` + `UndoStack` + `ViewState` +
  `Mode` + pending-state slots (`pending_count`, `pending_chord`,
  `pending_op`, `pending_obj_scope`, `register`). Mode-aware key
  dispatch lives in `handle_key` → `handle_insert_key` /
  `handle_normal_key` / `handle_op_pending_key` / `handle_visual_key`.
  Helpers `take_count` / `take_gg_target_row` /
  `take_capital_g_target_row` / `matches_chord_resolution` collapse
  what would otherwise be triplicated chord/digit/G handling. Cursor
  jumps for error POSITIONs use `move_cursor_to_char_position`.
  Block indent / outdent uses `selected_line_range` +
  `indent_lines` / `outdent_lines`. Multi-buffer support is in
  `tab.rs` (`TabSlot` per buffer + `Tabs` container with two-strike
  dirty-close).
- `motion.rs` is a pure-function module — `apply(buf, motion, count)
  → Cursor`. Word semantics use a 3-class partition (whitespace /
  identifier / punct), with newlines counting as whitespace.
- `text_object.rs` provides `iw aw iW aW i" a" i' a' i( a(`. WORD
  (`iW`/`aW`) is whitespace-bounded so SQL dotted identifiers like
  `schema.table` travel together.
- `find.rs::FindState` powers both Ctrl+F (`enter_closes = false`)
  and vim `/?` (`enter_closes = true` + `anchor` so each typed char
  moves the active match relative to the cursor).
- `command_line.rs` is the `:` ex prompt — single-line, parser-
  separated-from-execution. Parser returns `Command` enum;
  `App::execute_command` dispatches into existing primitives
  (`EditorState::goto_line`, `replace_all`, `commit_open`,
  `commit_save`, tab management, `should_quit`).
- `autocomplete.rs` holds `AutocompletePopup` — a prefix-filtered
  candidate list overlaid on the editor. Opened by Tab when a word
  prefix sits at the cursor. The popup is owned by `App`, not by the
  editor, because it needs access to the tree.
  `autocomplete_context.rs::detect_context` classifies the cursor as
  `TableName` (after FROM/JOIN/INTO/UPDATE/TABLE), `Dotted { qualifier }`
  (right after `q.`), or `Default`. `App::completion_candidates` then
  feeds the popup a narrowed list:
    - TableName → `tree.relation_names()`
    - Dotted: alias resolved via `extract_aliases` → `columns_of_relation`,
      else direct relation match → columns, else schema match → relations
      in that schema
    - Default → hard-coded `SQL_KEYWORDS` + `tree.collect_identifiers()`
  Falls back to Default when the narrowed list is empty (tree not yet
  loaded), so completion still works pre-introspection.
- `row_detail.rs` is a centered overlay that lists every column of
  the currently-selected result row. Opened by Enter on a populated
  Results pane; absorbs its own Esc/Enter/arrow keys.
- `cheatsheet.rs` is a scrollable overlay listing every keybinding,
  opened by `F1` or `?` outside the editor / search / autocomplete.
  Reuses `Paragraph::scroll` + `clamp_scroll` so it behaves the same
  way as `row_detail`.
- `file_prompt.rs` is the `Ctrl+O` / `Ctrl+S` inline filename prompt.
  Owned by `App::file_prompt`; rendered as a 3-row overlay pinned to
  the bottom of the editor area. While `Some`, it is the highest-
  priority modal — every key (Esc/Enter/printable/Backspace) routes
  to it, so even F-keys and global shortcuts don't dismiss it. Path
  resolution: absolute paths pass through, relative paths are joined
  onto `std::env::current_dir()`. No `~`, no globbing, no picker.
  Open normalizes CRLF → LF; Save writes the buffer verbatim. I/O is
  synchronous (small SQL files); errors surface as toasts and leave
  the editor buffer unchanged.
- `path_hint.rs` (`DirHint` + `draw_above_prompt`) is the directory
  listing dropdown shown above `file_prompt` and the `:` command
  line when the user is typing a path. Re-reads `std::fs::read_dir`
  on every keystroke (no caching), filters by basename prefix,
  hides dotfiles unless prefix starts with `.`, sorts directories
  first then alphabetical. `Up` / `Down` select, `Tab` commits the
  selection (or falls back to LCP `path_complete` when nothing is
  selected — file_prompt only).
- `substitute_confirm.rs` (`SubstituteState`) is the modal for
  `:s/.../c`. Uses a cursor-walk model — `from` cursor walks past
  each replacement so the next match is found *after* the inserted
  text, never re-finding the replacement itself.
- `cell_edit.rs` (`CellEditState`) is the inline cell-edit input
  box. Opened only when the current `ResultSet` carries a
  `RelationRef` source (set by tree-preview path) and the table has
  a single-column PK. Pre-populates input with the original
  `CellValue::to_string()`; `Ctrl+U` clears (→ NULL).
- `confirm_update.rs` (`ConfirmUpdateState`) is the centred modal
  showing the generated `UPDATE` SQL and asking `y/n`. The SQL is
  built by `sql_format::format_update_one` and is what gets sent
  to the server verbatim — what you see is what runs.
- `sql_format.rs` is the shared module for SQL value / identifier
  quoting and the user-input → CellValue parser. Both
  `sql_export::write_inserts` (INSERT) and
  `cell_edit`/`confirm_update` (UPDATE) use it so the quoting rules
  are guaranteed identical across both surfaces.

Modal layering (highest priority first, all checked in
`App::on_key`):
1. Quit (`Ctrl+Q` / `Ctrl+C`) — always wins.
2. `file_prompt` — Ctrl+O / Ctrl+S / Ctrl+E inline prompt; absorbs
   every key (even F-keys) until Enter / Esc. Carries a live
   `path_hint::DirHint` dropdown (Up/Down select, Tab commits).
3. `command_line` — `:` ex prompt. Single-line, absorbs every key.
   Carries the same `DirHint` dropdown when the input is in
   `e <path>` / `w <path>` form.
4. `confirm_update` — UPDATE confirm modal (y/n/Esc). Outranks the
   cell edit so a dispatched confirm can't be re-opened.
5. `cell_edit` — Cell-edit input box (Enter / Esc / Backspace /
   printable / Ctrl+U). Opened by `e` on Results; only fires when
   the result has a `RelationRef` source and a single-PK table.
6. `subst_confirm` — `:s/.../c` interactive substitute confirm
   (y/n/a/q + Esc). Sits above `find` so Ctrl+F can't hijack.
7. `cheatsheet.open` — `F1` / `?` overlay; routes Up/Down/PageUp/
   PageDown into scroll position and Esc/Enter/?/q to close.
8. `row_detail.open` — full-row modal (Enter on Results).
9. `find.is_some()` — Ctrl+F / Ctrl+H / vim `/?` overlay. When the
   overlay carries `pre_find_cursor` (set by Visual-mode entry),
   every match jump uses `jump_caret_keep_selection` and Esc
   restores cursor to the pre-search position.
10. Autocomplete popup (while editor focused).
11. Tree incremental search (`tree.search.is_some()` while focused).
12. Running query (Esc → cancel only).
13. Pane-specific handler. Inside the editor pane the dispatcher
    branches further on `editor.mode()`.

Keybinding quick-ref (workspace). The full list lives in
`src/ui/cheatsheet.rs::ROWS` and is rendered scrollably by `F1`/`?`.

- `F5` / `Ctrl+Enter` run (selection if active, else whole buffer)
- `Esc` dismiss toast → cancel query → cancel connect (cascading);
  inside the editor it also flips Insert→Normal
- `F2`/`F3`/`F4` focus tree/editor/results (also `Alt+1/2/3` backup)
- `F1` or `?` open cheatsheet (scrollable: `j`/`k` / arrows /
  PageUp/Down / Home/End)
- Editor (Insert): `Tab` autocomplete/indent · `Shift+Tab` outdent
  (block-aware) · `Ctrl+Up/Down` history · `Ctrl+O`/`Ctrl+S`
  open/save · `Ctrl+F` find · `Ctrl+H` find/replace ·
  `Ctrl+G` opens the `:` command line as a goto-line alias
- Editor (Normal): `i a I A o O` enter Insert · `v` enter Visual ·
  motions `h j k l w b e 0 ^ $ gg G %` (count prefix) ·
  operators `d y c x s` + `dd yy cc` linewise · text objects
  `iw aw iW aW i" a" i' a' i( a(` · `p`/`P` paste ·
  `/`/`?`/`n`/`N` search · `:` open command line
- Editor (Visual): `/`/`?`/`n`/`N` extend selection to match
  (cursor only; selection_anchor preserved). Esc on `/` restores
  cursor to its pre-search position.
- Editor tabs (any mode): `Ctrl+T` new · `Ctrl+W` close (twice within
  3s if dirty) · `Ctrl+]`/`Ctrl+[` (or `Ctrl+PageDown`/`Up`) cycle ·
  `Ctrl+1..9` jump to tab N
- `:` command line: `:N` goto line · `:s/pat/repl/[gc]` /
  `:%s/pat/repl/[gc]` substitute (`c` = interactive confirm) ·
  `:w [path]` save · `:e <path>` open (both prompts get the
  Up/Down + Tab path-hint dropdown) · `:tabnew` / `:tabn` / `:tabp`
  / `:tabc` · `:q` quit · `:help` open cheatsheet
- Tree: `/` incremental search · `n`/`N` repeat · `p` / `Space`
  preview rows of selected table · `D` show synthesized DDL
- Results: `Enter` row detail · `s` sort current column
  (Asc→Desc→off) · `Ctrl+Left`/`Ctrl+Right` first/last column ·
  `y` / `Y` copy cell / row (OSC 52 clipboard) · `R` re-run last
  query · `e` cell edit (tree-preview results, single-PK tables)
- Workspace-wide: `Ctrl+E` export current result set to CSV
- Status bar TX badge: `[TX]` (yellow) inside `BEGIN`, `[TX!]`
  (red) when the transaction is in error
- `Ctrl+Q` / `Ctrl+C` quit. `F10` is NOT bound.

## Conventions

- `CellValue::Unsupported(String)` — the `Type::name()` method returns
  a non-`'static` slice for custom types, so we allocate. Don't revert
  to `&'static str`.
- Toasts auto-expire; set with `App::toast_info` / `toast_error`.
- No logging subscriber is installed. `tracing::*!` macros stay in the
  code as no-ops. If diagnostics are needed, install a file-based
  subscriber — never stdout (corrupts TUI).
- Passwords are zeroized on `ConnInfo::drop` via the `zeroize` crate.
  Keep it that way; if you add new secret fields, zeroize them too.
