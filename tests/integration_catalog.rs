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
    assert!(!schemas.iter().any(|s| s.starts_with("pg_") || s == "information_schema"));
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

    let users = relations.iter().find(|r| r.name == "users").expect("users table");
    let orders = relations.iter().find(|r| r.name == "orders").expect("orders table");
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
        vec!["id", "email", "display_name", "created_at", "balance", "metadata"]
    );
    let email = columns.iter().find(|c| c.name == "email").unwrap();
    assert!(!email.nullable);
}
