//! Integration: schema browser catalog queries against fixtures in
//! `docker/init.sql`.

mod integration_common;

use integration_common::{connect_plain, init_crypto, pg_url};
use psqlview::db::catalog::{self, RelationKind};

#[tokio::test]
#[ignore = "requires PSQLVIEW_PG_URL"]
async fn list_schemas_excludes_system_schemas() {
    init_crypto();
    if pg_url().is_none() {
        return;
    }
    let (client, _handle) = connect_plain().await;
    let schemas = catalog::list_schemas(&client).await.expect("list schemas");
    assert!(!schemas
        .iter()
        .any(|s| s.starts_with("pg_") || s == "information_schema"));
    assert!(schemas.iter().any(|s| s == "public"));
    assert!(
        schemas.iter().any(|s| s == "psqlview_test"),
        "fixture schema missing — did init.sql run? got {schemas:?}"
    );
}

#[tokio::test]
#[ignore = "requires PSQLVIEW_PG_URL"]
async fn list_relations_returns_fixture_tables_and_view() {
    init_crypto();
    if pg_url().is_none() {
        return;
    }
    let (client, _handle) = connect_plain().await;

    let relations = catalog::list_relations(&client, "psqlview_test")
        .await
        .expect("list relations");

    let users = relations
        .iter()
        .find(|r| r.name == "users")
        .expect("users table");
    let orders = relations
        .iter()
        .find(|r| r.name == "orders")
        .expect("orders table");
    let view = relations
        .iter()
        .find(|r| r.name == "paid_orders")
        .expect("paid_orders view");

    assert_eq!(users.kind, RelationKind::Table);
    assert_eq!(orders.kind, RelationKind::Table);
    assert_eq!(view.kind, RelationKind::View);
}

#[tokio::test]
#[ignore = "requires PSQLVIEW_PG_URL"]
async fn list_databases_includes_postgres() {
    init_crypto();
    if pg_url().is_none() {
        return;
    }
    let (client, _handle) = connect_plain().await;
    let dbs = catalog::list_databases(&client)
        .await
        .expect("list databases");
    assert!(dbs.iter().any(|d| d == "postgres"), "got: {dbs:?}");
    assert!(!dbs.iter().any(|d| d == "template0" || d == "template1"));
}

#[tokio::test]
#[ignore = "requires PSQLVIEW_PG_URL"]
async fn fetch_table_ddl_synthesizes_create_table() {
    init_crypto();
    if pg_url().is_none() {
        return;
    }
    let (client, _handle) = connect_plain().await;

    let ddl = catalog::fetch_table_ddl(&client, "psqlview_test", "orders")
        .await
        .expect("fetch ddl");

    assert!(
        ddl.contains("CREATE TABLE \"psqlview_test\".\"orders\""),
        "missing header: {ddl}"
    );
    assert!(ddl.contains("id bigint"), "id column missing: {ddl}");
    assert!(
        ddl.contains("user_id bigint NOT NULL"),
        "user_id NOT NULL missing: {ddl}"
    );
    // Primary key + foreign key + check constraint should all surface.
    assert!(ddl.contains("PRIMARY KEY (id)"), "PK missing: {ddl}");
    assert!(
        ddl.contains("FOREIGN KEY") && ddl.contains("REFERENCES"),
        "FK missing: {ddl}"
    );
    assert!(
        ddl.contains("CHECK") && ddl.contains("status"),
        "CHECK constraint missing: {ddl}"
    );
    // Standalone index lives after the closing paren.
    let close = ddl.find(");\n").expect("closing paren");
    let idx = ddl
        .find("CREATE INDEX")
        .unwrap_or_else(|| panic!("CREATE INDEX missing: {ddl}"));
    assert!(idx > close);
}

#[tokio::test]
#[ignore = "requires PSQLVIEW_PG_URL"]
async fn fetch_relation_ddl_for_view_returns_create_view() {
    init_crypto();
    if pg_url().is_none() {
        return;
    }
    let (client, _handle) = connect_plain().await;

    let ddl =
        catalog::fetch_relation_ddl(&client, "psqlview_test", "paid_orders", RelationKind::View)
            .await
            .expect("fetch view ddl");

    assert!(
        ddl.starts_with("CREATE VIEW \"psqlview_test\".\"paid_orders\" AS\n"),
        "header wrong: {ddl}"
    );
    // pg_get_viewdef returns the SELECT body with PG-canonical
    // spacing; we only assert on the keywords + reference, not the
    // exact whitespace.
    assert!(ddl.contains("SELECT"), "missing SELECT: {ddl}");
    assert!(
        ddl.contains("psqlview_test.orders"),
        "missing FROM target: {ddl}"
    );
    // Must NOT contain the wrong CREATE TABLE shape that the old code
    // would have emitted.
    assert!(
        !ddl.contains("CREATE TABLE"),
        "view rendered as CREATE TABLE: {ddl}"
    );
}

#[tokio::test]
#[ignore = "requires PSQLVIEW_PG_URL"]
async fn fetch_relation_ddl_for_table_matches_legacy_path() {
    init_crypto();
    if pg_url().is_none() {
        return;
    }
    let (client, _handle) = connect_plain().await;

    let ddl = catalog::fetch_relation_ddl(&client, "psqlview_test", "orders", RelationKind::Table)
        .await
        .expect("fetch table ddl via router");
    assert!(ddl.contains("CREATE TABLE \"psqlview_test\".\"orders\""));
    assert!(ddl.contains("PRIMARY KEY (id)"));
}

#[tokio::test]
#[ignore = "requires PSQLVIEW_PG_URL"]
async fn fetch_table_ddl_returns_error_for_unknown_relation() {
    init_crypto();
    if pg_url().is_none() {
        return;
    }
    let (client, _handle) = connect_plain().await;

    let err = catalog::fetch_table_ddl(&client, "psqlview_test", "no_such_table")
        .await
        .expect_err("expected not-found error");
    let msg = err.to_string();
    assert!(msg.contains("relation not found"), "got: {msg}");
}

#[tokio::test]
#[ignore = "requires PSQLVIEW_PG_URL"]
async fn list_columns_returns_ordered_schema() {
    init_crypto();
    if pg_url().is_none() {
        return;
    }
    let (client, _handle) = connect_plain().await;

    let columns = catalog::list_columns(&client, "psqlview_test", "users")
        .await
        .expect("list columns");

    let names: Vec<&str> = columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "id",
            "email",
            "display_name",
            "created_at",
            "balance",
            "metadata"
        ]
    );
    let email = columns.iter().find(|c| c.name == "email").unwrap();
    assert!(!email.nullable);
}
