//! Shared bootstrap for integration tests.
//!
//! Integration tests are gated behind the `PSQLVIEW_PG_URL` environment
//! variable. If unset, the test macro below causes the test to be skipped
//! (reported as passing, with a message to stdout). This lets CI run
//! `cargo test -- --include-ignored` on runners where the Postgres
//! containers are up, while still letting contributors run `cargo test`
//! locally without Docker.

#![allow(dead_code)]

use std::sync::Once;

use tokio_postgres::{Client, Config};

static INIT: Once = Once::new();

pub fn init_crypto() {
    INIT.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

pub fn pg_url() -> Option<String> {
    std::env::var("PSQLVIEW_PG_URL").ok()
}

/// Connect using the URL from `PSQLVIEW_PG_URL` with a plain (no TLS) path.
/// Tests do not exercise TLS — production code does, and that path is
/// covered by unit tests via the connector construction.
pub async fn connect_plain() -> (Client, tokio::task::JoinHandle<()>) {
    init_crypto();
    let url = pg_url().expect("PSQLVIEW_PG_URL not set");
    let cfg: Config = url.parse().expect("parse PSQLVIEW_PG_URL");
    let (client, connection) = cfg
        .connect(tokio_postgres::NoTls)
        .await
        .expect("connect postgres");
    let handle = tokio::spawn(async move {
        if let Err(e) = connection.await {
            eprintln!("connection ended: {e}");
        }
    });
    (client, handle)
}
