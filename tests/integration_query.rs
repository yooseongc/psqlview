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
async fn long_running_query_can_be_cancelled() {
    init_crypto();
    if pg_url().is_none() {
        return;
    }
    let (client, _handle) = connect_plain().await;
    let cancel = client.cancel_token();
    let client = std::sync::Arc::new(client);

    let client_clone = client.clone();
    let query = tokio::spawn(async move {
        db::query::execute(client_clone, "SELECT pg_sleep(30)").await
    });

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
