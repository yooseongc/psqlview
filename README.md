# psqlview

A small, fast TUI for browsing PostgreSQL 14+ databases and running ad-hoc
SQL. Ships as a single statically-linked musl binary for Linux x86_64.

- **Drivers**: `tokio-postgres` with `rustls` TLS (no OpenSSL).
- **UI**: `ratatui` + `crossterm`, with a custom in-tree SQL editor
  (no `tui-textarea`) so syntax highlighting can run per-token.
- **Runtime**: Tokio multi-threaded runtime, background workers dispatch
  events back to the draw loop through an `mpsc` channel.
- **Compatibility**: PostgreSQL 14, 15, 16, 17 (anything newer should also
  work — catalog queries avoid version-specific columns).

## Features

### Connect

- Connect dialog with host / port / user / db / password / SSL mode and
  validated input (invalid port is reported instead of silently
  falling back to 5432).
- Passwords are zeroized in memory on drop.

### Schema browser

- Lazy tree
  (`schema → tables | views | materialized | partitioned | foreign → columns`).
- Incremental `/` search with `n` / `N` repeat.
- `p` (or `Space`) on a relation runs `SELECT * FROM "schema"."relation" LIMIT 200`
  and routes the rows to the results pane without touching the editor.
- `D` on a relation synthesizes a `CREATE TABLE` (or `pg_get_viewdef`
  for views / matviews) from `pg_catalog`. Result lands in the results
  pane as a single-column `ddl` text view that you can scroll, copy,
  and re-run.

### SQL editor

- Custom buffer with multi-level undo / redo (`Ctrl+Z` / `Ctrl+Y`).
- **Syntax highlighting** via a per-token lexer: keyword, string,
  number, comment, quoted-identifier — multi-line strings and block
  comments carry state across lines.
- **Bracket-pair matching**: brackets at the cursor and their match
  are highlighted in reverse video; brackets inside strings or
  comments are skipped.
- **Context-aware autocomplete** (`Tab`):
  - After `FROM` / `JOIN` / `INTO` / `UPDATE` / `TABLE` → relation names.
  - After `qualifier.` → columns of `qualifier`, with alias resolution
    (`SELECT u.| FROM users u` lists columns of `users`).
  - Otherwise → keywords + all loaded identifiers.
- **Block indent / outdent** for multi-line selections.
- **Run-selection**: `F5` / `Ctrl+Enter` execute the highlighted text
  when a selection is active, otherwise the whole buffer.
- **Query history** (`Ctrl+Up` / `Ctrl+Down`) — memory-only, 50 entries.
- **File open / save**: `Ctrl+O` and `Ctrl+S` open an inline filename
  prompt anchored to the bottom of the editor pane. Paths are
  cwd-relative; absolute paths pass through; CRLF is normalized to LF
  on open.
- On a query error with a reported POSITION, the caret jumps to the
  offending character.

### Results

- Streaming table with per-column widths, vertical and horizontal
  scroll.
- **Client-side sort** (`s` cycles asc / desc / off on the current
  column).
- **Row detail modal** (`Enter`) showing every column of the selected row.
- **EXPLAIN pretty-print**: `QUERY PLAN` results render as a tree
  with bold node names; slow nodes (≥10 ms) get a yellow accent,
  ≥100 ms get red.
- **CSV export** (`Ctrl+E`): writes the current result set as RFC 4180
  CSV through the inline file prompt — quotes fields with commas /
  quotes / newlines, doubles internal `"`, NULL renders as empty.
- **OSC 52 clipboard copy**: `y` copies the cell at the leftmost
  visible column of the selected row, `Y` copies the whole row as
  TSV. Works in any terminal that honours OSC 52 (kitty, Windows
  Terminal, iTerm2, Tabby, ghostty, wezterm, recent xterm) — no
  native clipboard library is linked.
- **Re-run** (`R`): re-issues the last query, or refreshes the DDL
  view via the catalog when the last "query" was a `D` shortcut.
- Cap at 10 000 rows with a `(truncated)` indicator.

### Other

- **Cancel long-running queries with `Esc`** (libpq-style `cancel_query`).
- **Mouse**: click to focus, wheel to scroll, bracketed-paste support.
  Hold Shift while dragging to bypass mouse capture for native text
  selection.
- **Cheatsheet overlay** (`F1` or `?`) lists every keybinding.

## Keybindings

| Scope | Key | Action |
| --- | --- | --- |
| Global | `F1` / `?` | Show keybinding cheatsheet |
| Global | `Ctrl+Q` / `Ctrl+C` | Quit |
| Global | `F2` / `F3` / `F4` | Focus tree / editor / results |
| Global | `Alt+1` / `Alt+2` / `Alt+3` | Same (terminal-fallback) |
| Global | `Tab` / `Shift+Tab` | Cycle focus (outside editor) |
| Global | `Esc` | Dismiss toast → cancel query → cancel connect |
| Global | `Ctrl+E` | Export current result set to CSV |
| Editor | `F5` / `Ctrl+Enter` | Run query (selection or whole buffer) |
| Editor | `Tab` | Context-aware autocomplete or 2-space indent |
| Editor | `Shift+Tab` | Outdent (block-aware on selection) |
| Editor | `Ctrl+Z` / `Ctrl+Y` | Undo / redo |
| Editor | `Ctrl+Up` / `Ctrl+Down` | Recall previous / next query |
| Editor | `Ctrl+O` / `Ctrl+S` | Open / save file (cwd-relative path) |
| Editor | `Ctrl+Shift+V` (terminal) | Bracketed paste |
| Schema tree | `j k` / arrows | Move |
| Schema tree | `PageUp` / `PageDown` | Page (screenful) |
| Schema tree | `Home` / `End` | First / last entry |
| Schema tree | `Enter` / `→` / `l` | Expand / load |
| Schema tree | `←` / `h` | Collapse |
| Schema tree | `/` | Incremental search; `n` / `N` repeat |
| Schema tree | `p` / `Space` | Preview rows of selected table (`SELECT *  LIMIT 200`) |
| Schema tree | `D` | Show DDL of selected relation |
| Results | `j k` / arrows | Move row |
| Results | `PageUp` / `PageDown` | Page (screenful) |
| Results | `Home` / `End` | First / last row |
| Results | `h l` / arrows | Scroll columns |
| Results | `Ctrl+Left` / `Ctrl+Right` | First / last column |
| Results | `s` | Sort current column (Asc → Desc → off) |
| Results | `y` / `Y` | Copy current cell / row to clipboard (OSC 52) |
| Results | `R` | Re-run last query (or refresh DDL view) |
| Results | `Enter` | Open row detail modal |
| Connect | `Tab` / arrows | Move between fields |
| Connect | `Enter` (last field) / `Ctrl+Enter` | Submit |

## Build

Development is on Windows; production builds target
`x86_64-unknown-linux-musl` inside Docker so we always produce a static
binary.

```sh
docker compose run --rm builder
# produces ./dist/psqlview
```

Under the hood this mounts the repo into a `rust:1.93-alpine` container,
installs `musl-dev`, targets `x86_64-unknown-linux-musl`, and copies the
resulting binary to `./dist/psqlview`. Both cargo registry and `target/`
are cached in named Docker volumes (`cargo-cache`, `target-cache`) so
rebuilds are incremental.

Verify the output is truly static:

```sh
docker run --rm -v "$PWD/dist:/b" alpine file /b/psqlview
# → ELF ... statically linked
docker run --rm -v "$PWD/dist:/b" alpine ldd /b/psqlview
# → Not a valid dynamic program
```

There is also a multi-stage `docker/builder.Dockerfile` that produces a
tiny runtime image (`alpine:3.20` + `ca-certificates`). Use it for
`docker build -t psqlview .` style pipelines.

## Tests

Unit tests do not need any infrastructure:

```sh
docker compose run --rm builder cargo test --lib --target x86_64-unknown-linux-musl
```

Integration tests are gated behind `PSQLVIEW_PG_URL`. Spin up the
multi-version compose stack first:

```sh
docker compose up -d pg14 pg15 pg16 pg17
docker compose run --rm tester
# → cargo test -- --include-ignored against pg16 (default)
```

To exercise a different PostgreSQL version, override the env var:

```sh
docker compose run --rm -e PSQLVIEW_PG_URL=postgres://postgres:test@pg14:5432/postgres tester
docker compose run --rm -e PSQLVIEW_PG_URL=postgres://postgres:test@pg17:5432/postgres tester
```

The fixtures loaded by all four images live in
[docker/init.sql](docker/init.sql):

- `psqlview_test` schema — `users`, `orders`, `paid_orders` view, plus
  a per-type `all_types` table for round-trip coverage.
- `psqlview_bulk` schema — 50 small tables (`t001`..`t050`), a 500-row
  table, a 2000-row table, and an `events` table with 1000 rows × 12
  columns. Useful for paging, search, sort, and EXPLAIN smoke tests.

## Manual smoke test

```sh
docker compose up -d pg16
docker compose run --rm -it builder \
  /dist/psqlview
# then in the TUI: host=pg16  port=5432  user=postgres  db=postgres  password=test
```

- Submit the connection → status bar shows the pg version.
- Expand `psqlview_test` in the left pane → see tables & view.
- `F1` opens the keybinding cheatsheet.
- In the editor, `SELECT pg_sleep(30);` + `F5`, then `Esc` — should come
  back to idle within ~1 second.
- `SELECT * FROM psqlview_test.users;` then `j`/`k` to move rows,
  `Enter` to open the row-detail modal.
- `EXPLAIN ANALYZE SELECT * FROM psqlview_bulk.events WHERE flagged;`
  to see the pretty-printed plan.
- Move to the schema tree, select `psqlview_test.orders`, press `p`
  (preview) and `D` (DDL).
- In the results pane, `y` copies the current cell, `Ctrl+E` exports
  the table as CSV.
- See [TEST.md](TEST.md) for a longer paging / search / history
  checklist.

## Project layout

```
src/
  main.rs             terminal setup/teardown, tokio runtime,
                      bracketed-paste + mouse capture wiring
  lib.rs              re-exports modules so tests can depend on them
  app.rs              state machine + event dispatch
  event.rs            crossterm → AppEvent pump (+ tick timer)
  config.rs           ConnInfo (zeroized password)
  types.rs            CellValue, ColumnMeta, ResultSet, ServerVersion, SslMode
  db/
    mod.rs            Session = {Arc<Client>, CancelToken, ServerVersion}
    connect.rs        Config builder + rustls MakeRustlsConnect
    query.rs          SELECT-vs-side-effect split, row streaming, type map
    catalog.rs        information_schema / pg_catalog browsing,
                      synthesized DDL (PG14+ safe)
  ui/
    mod.rs                    top-level draw(), focus styling, PaneRects
    connect_dialog.rs         6-field form with terminal caret
    schema_tree.rs            lazy tree, flatten(), `/` search, paging
    sql_lexer.rs              per-token lexer feeding editor + bracket-pair
    editor/
      mod.rs                  EditorState, public surface
      buffer.rs               TextBuffer + cursor + selection
      edit.rs                 key → buffer mutation
      undo.rs                 bounded undo / redo stack
      bracket.rs              bracket-pair finder
      render.rs               syntax-coloured render + caret + match
    autocomplete.rs           prefix-filtered candidate popup
    autocomplete_context.rs   classifies cursor (TableName / Dotted / Default)
    results.rs                paginated table, column sizing, sort, EXPLAIN
    row_detail.rs             full-row modal (Enter on Results)
    cheatsheet.rs             keybinding overlay (F1 / ?)
    file_prompt.rs            inline filename prompt (Ctrl+O / Ctrl+S / Ctrl+E)
    csv_export.rs             RFC 4180 CSV serializer
    clipboard.rs              OSC 52 escape + hand-rolled base64
    status.rs                 one-line footer with cursor pos and hints
tests/
  integration_common.rs   shared bootstrap + PSQLVIEW_PG_URL gate
  integration_connect.rs
  integration_query.rs
  integration_catalog.rs
docker/
  builder.Dockerfile       multi-stage cross-compile image
  init.sql                 shared test fixtures (loaded by pg14-17)
docker-compose.yml         pg14|pg15|pg16|pg17|builder|tester services
```

## License

Dual-licensed under MIT or Apache-2.0.
