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
- Input: `Key`, `Resize`, `Tick` (250 ms)
- Completion: `ConnectResult`, `QueryResult`,
  `SchemasLoaded`, `RelationsLoaded`, `ColumnsLoaded`

### State machine (src/app.rs)

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
    timestamp, timestamptz, json/jsonb, uuid, bytea, unsupported).
    Capped at `ROW_LIMIT = 10_000`.
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
  between the two, or selection math will desync.
- `results.rs` computes column widths from the first 256 rows (`min
  4, max 40`, Unicode-width-aware via `unicode-width`). Don't sort —
  preserve insertion order so vertical scroll indices stay stable.
- `tui-textarea` owns its own undo stack and cursor; the editor module
  is a thin wrapper.

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
