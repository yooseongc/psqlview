# psqlview

A small, fast TUI for browsing PostgreSQL 14+ databases and running ad-hoc
SQL. Ships as a single statically-linked musl binary for Linux x86_64.

- **Drivers**: `tokio-postgres` with `rustls` TLS (no OpenSSL).
- **UI**: `ratatui` + `crossterm` + `tui-textarea`.
- **Runtime**: Tokio multi-threaded runtime, background workers dispatch
  events back to the draw loop through an `mpsc` channel.
- **Compatibility**: PostgreSQL 14, 15, 16, 17 (anything newer should also
  work — catalog queries avoid version-specific columns).

## Features

- Connect dialog with host / port / user / db / password / SSL mode.
- Lazy schema browser tree:
  `schema → tables | views | materialized | partitioned | foreign → columns`.
- SQL editor (multiline) with F5 to run.
- Streaming result table with per-column widths, vertical scroll
  (`j/k`, `PageUp/PageDown`, `Home/End`) and horizontal offset (`h/l`).
- **Cancel long-running queries with Esc** (libpq-style `cancel_query`).
- Sensible memory caps — results are capped at 10 000 rows with a
  `(truncated)` indicator.

## Keybindings

| Key | Action |
| --- | --- |
| `F5` | Execute editor contents |
| `Esc` | Cancel running query / exit connect dialog |
| `Tab` / `Shift+Tab` | Cycle focus Tree → Editor → Results |
| `Ctrl+Q` or `F10` | Quit |
| `Enter` / `→` | Expand tree node / edit field |
| `←` | Collapse tree node |
| `j` `k` or arrows | Move in tree / results |
| `h` `l` | Scroll results horizontally |

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
[docker/init.sql](docker/init.sql) — a `psqlview_test` schema with
`users`, `orders`, and a `paid_orders` view.

## Manual smoke test

```sh
docker compose up -d pg16
docker compose run --rm -it builder \
  /dist/psqlview
# then in the TUI: host=pg16  port=5432  user=postgres  db=postgres  password=test
```

- Submit the connection → status bar shows the pg version.
- Expand `psqlview_test` in the left pane → see tables & view.
- In the editor, `SELECT pg_sleep(30);` + `F5`, then `Esc` — should come
  back to idle within ~1 second.
- `SELECT * FROM psqlview_test.users;` and scroll with `j/k`.

## Project layout

```
src/
  main.rs             terminal setup/teardown, tokio runtime, tracing
  lib.rs              re-exports modules so tests can depend on them
  app.rs              state machine + event dispatch
  event.rs            crossterm → AppEvent pump (+ tick timer)
  config.rs           ConnInfo (zeroized password)
  types.rs            CellValue, ColumnMeta, ResultSet, ServerVersion, SslMode
  db/
    mod.rs            Session = {Arc<Client>, CancelToken, ServerVersion}
    connect.rs        Config builder + rustls MakeRustlsConnect
    query.rs          SELECT-vs-side-effect split, row streaming, type map
    catalog.rs        information_schema / pg_catalog browsing (PG14+ safe)
  ui/
    mod.rs            top-level draw(), focus styling helpers
    connect_dialog.rs 6-field form
    schema_tree.rs    lazy tree, flatten() for list rendering
    editor.rs         tui-textarea wrapper
    results.rs        paginated table, column sizing
    status.rs         one-line footer
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
