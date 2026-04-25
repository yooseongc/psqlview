//! Integration: query execution, streaming, cancellation.

mod integration_common;

use std::time::{Duration, Instant};

use integration_common::{connect_plain, init_crypto, pg_url};
use psqlview::db;
use psqlview::types::{CellValue, ResultSet};

#[tokio::test]
#[ignore = "requires PSQLVIEW_PG_URL"]
async fn select_roundtrip_produces_typed_cells() {
    init_crypto();
    if pg_url().is_none() {
        return;
    }
    let (client, _handle) = connect_plain().await;
    let client = std::sync::Arc::new(client);

    let set: ResultSet = db::query::execute(
        client,
        "SELECT 1::int4 AS one, 'hi'::text AS greeting, NULL::text AS missing",
    )
    .await
    .expect("execute select");

    assert_eq!(set.columns.len(), 3);
    assert_eq!(set.rows.len(), 1);
    assert!(matches!(set.rows[0][0], CellValue::Int(1)));
    assert!(matches!(&set.rows[0][1], CellValue::Text(t) if t == "hi"));
    assert!(matches!(set.rows[0][2], CellValue::Null));
}

#[tokio::test]
#[ignore = "requires PSQLVIEW_PG_URL"]
async fn non_select_returns_command_tag() {
    init_crypto();
    if pg_url().is_none() {
        return;
    }
    let (client, _handle) = connect_plain().await;
    let client = std::sync::Arc::new(client);

    let set = db::query::execute(client, "SET application_name = 'psqlview-test'")
        .await
        .expect("execute set");
    assert!(set.columns.is_empty());
    assert!(set.command_tag.is_some());
}

#[tokio::test]
#[ignore = "requires PSQLVIEW_PG_URL"]
async fn convert_cell_covers_all_supported_types() {
    init_crypto();
    if pg_url().is_none() {
        return;
    }
    let (client, _handle) = connect_plain().await;
    let client = std::sync::Arc::new(client);

    let set = db::query::execute(
        client,
        "SELECT id, c_bool, c_int2, c_int4, c_int8, c_float4, c_float8, c_numeric, \
            c_text, c_date, c_time, c_timestamp, c_timestamptz, c_json, c_jsonb, \
            c_uuid, c_bytea, c_inet FROM psqlview_test.all_types ORDER BY id",
    )
    .await
    .expect("execute select");

    assert_eq!(set.rows.len(), 2, "fixture should have exactly 2 rows");
    let vals = &set.rows[0];

    assert!(matches!(vals[0], CellValue::Int(1)));
    assert!(matches!(vals[1], CellValue::Bool(true)));
    assert!(matches!(vals[2], CellValue::Int(-32768)));
    assert!(matches!(vals[3], CellValue::Int(-2147483648)));
    assert!(matches!(vals[4], CellValue::Int(9223372036854775807)));
    assert!(matches!(vals[5], CellValue::Float(v) if (v - 1.5).abs() < 1e-5));
    assert!(matches!(vals[6], CellValue::Float(v) if (v - 2.5).abs() < 1e-12));
    assert!(
        matches!(&vals[7], CellValue::Numeric(d) if d.to_string() == "123.45"),
        "numeric: {:?}",
        vals[7]
    );
    assert!(matches!(&vals[8], CellValue::Text(s) if s == "hello"));
    assert!(matches!(&vals[9], CellValue::Date(d) if d.to_string() == "2026-04-22"));
    assert!(matches!(&vals[10], CellValue::Time(t) if t.to_string() == "12:34:56"));
    assert!(
        matches!(&vals[11], CellValue::Timestamp(ts) if ts.to_string().starts_with("2026-04-22")),
        "timestamp: {:?}",
        vals[11]
    );
    assert!(
        matches!(&vals[12], CellValue::TimestampTz(ts) if ts.to_string().starts_with("2026-04-22")),
        "timestamptz: {:?}",
        vals[12]
    );
    assert!(matches!(&vals[13], CellValue::Json(j) if j.contains("\"k\":1")));
    assert!(matches!(&vals[14], CellValue::Json(j) if j.contains("\"k\":1")));
    assert!(
        matches!(&vals[15], CellValue::Text(s) if s == "00000000-0000-0000-0000-000000000001"),
        "uuid: {:?}",
        vals[15]
    );
    assert!(matches!(vals[16], CellValue::Bytes(3)));
    assert!(
        matches!(&vals[17], CellValue::Text(s) if s == "10.0.0.1"),
        "inet: {:?}",
        vals[17]
    );

    // id=2 row: every supported column is NULL (except id itself).
    let nulls = &set.rows[1];
    assert!(matches!(nulls[0], CellValue::Int(2)));
    for (i, cell) in nulls.iter().enumerate().skip(1) {
        assert!(
            matches!(cell, CellValue::Null),
            "col {i} should be Null, got {cell:?}"
        );
    }
}

#[tokio::test]
#[ignore = "requires PSQLVIEW_PG_URL"]
async fn row_limit_truncates_at_10000_with_tag() {
    init_crypto();
    if pg_url().is_none() {
        return;
    }
    let (client, _handle) = connect_plain().await;
    let client = std::sync::Arc::new(client);

    let set = db::query::execute(client, "SELECT generate_series(1, 10001)")
        .await
        .expect("execute generate_series");

    assert_eq!(set.rows.len(), 10_000);
    assert_eq!(set.truncated_at, Some(10_000));
    let tag = set.command_tag.as_deref().unwrap_or_default();
    assert!(tag.contains("(truncated)"), "tag: {tag}");
}

#[tokio::test]
#[ignore = "requires PSQLVIEW_PG_URL"]
async fn explain_returns_rows_via_select_path() {
    init_crypto();
    if pg_url().is_none() {
        return;
    }
    let (client, _handle) = connect_plain().await;
    let client = std::sync::Arc::new(client);

    let set = db::query::execute(client, "EXPLAIN SELECT 1")
        .await
        .expect("execute explain");

    assert!(!set.columns.is_empty());
    assert!(!set.rows.is_empty());
    assert!(
        set.rows
            .iter()
            .all(|r| r.iter().all(|c| matches!(c, CellValue::Text(_)))),
        "EXPLAIN rows should all be text"
    );
}

#[tokio::test]
#[ignore = "requires PSQLVIEW_PG_URL"]
async fn multi_statement_ddl_uses_simple_query_path() {
    init_crypto();
    if pg_url().is_none() {
        return;
    }
    let (client, _handle) = connect_plain().await;
    let client = std::sync::Arc::new(client);

    let set = db::query::execute(
        client,
        "CREATE TEMP TABLE t_multi(id int); \
         INSERT INTO t_multi VALUES (1),(2); \
         DROP TABLE t_multi;",
    )
    .await
    .expect("execute multi-statement");

    assert!(set.columns.is_empty());
    let tag = set.command_tag.expect("command tag");
    assert!(tag.contains("row(s) affected"), "tag: {tag}");
}

#[tokio::test]
#[ignore = "requires PSQLVIEW_PG_URL"]
async fn syntax_error_returns_db_error_not_panic() {
    init_crypto();
    if pg_url().is_none() {
        return;
    }
    let (client, _handle) = connect_plain().await;
    let client = std::sync::Arc::new(client);

    let err = db::query::execute(client, "SELEKT 1")
        .await
        .expect_err("syntax error expected");
    match err {
        psqlview::db::DbError::Query(e) => {
            assert!(e.code().is_some(), "expected a SQLSTATE code");
        }
        other => panic!("expected DbError::Query, got {other:?}"),
    }
}

#[tokio::test]
#[ignore = "requires PSQLVIEW_PG_URL"]
async fn undefined_table_error_renders_sqlstate_and_position_snippet() {
    init_crypto();
    if pg_url().is_none() {
        return;
    }
    let (client, _handle) = connect_plain().await;
    let client = std::sync::Arc::new(client);

    let sql = "SELECT * FROM no_such_relation_xyz";
    let err = db::query::execute(client, sql)
        .await
        .expect_err("undefined table expected");
    let detailed = err.format_detailed_with_sql(sql);
    // SQLSTATE 42P01 = undefined_table.
    assert!(
        detailed.contains("42P01"),
        "want SQLSTATE 42P01, got: {detailed}"
    );
    assert!(detailed.contains("ERROR"), "want severity: {detailed}");
    // Snippet: the offending SQL on one line and a caret on the next.
    let lines: Vec<&str> = detailed.lines().collect();
    let sql_line = lines
        .iter()
        .position(|l| l.trim() == sql.trim())
        .expect("sql snippet present");
    let caret_line = lines[sql_line + 1];
    assert!(
        caret_line.contains('^'),
        "want caret line, got: {caret_line}"
    );
    assert!(
        caret_line.contains("line 1"),
        "want line info, got: {caret_line}"
    );
}

#[tokio::test]
#[ignore = "requires PSQLVIEW_PG_URL"]
async fn long_running_query_can_be_cancelled() {
    init_crypto();
    if pg_url().is_none() {
        return;
    }
    let (client, _handle) = connect_plain().await;
    let cancel = client.cancel_token();
    let client = std::sync::Arc::new(client);

    let client_clone = client.clone();
    let query =
        tokio::spawn(async move { db::query::execute(client_clone, "SELECT pg_sleep(30)").await });

    tokio::time::sleep(Duration::from_millis(200)).await;

    let started = Instant::now();
    cancel
        .cancel_query(tokio_postgres::NoTls)
        .await
        .expect("cancel");
    let result = tokio::time::timeout(Duration::from_secs(3), query)
        .await
        .expect("join within 3s")
        .expect("task join");
    let elapsed = started.elapsed();

    assert!(
        result.is_err(),
        "expected cancellation error, got: {result:?}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "cancel took too long: {elapsed:?}"
    );
}
