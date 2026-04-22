//! Integration: connect + server_version_num read against docker-compose PG.

mod integration_common;

use integration_common::{init_crypto, pg_url};
use psqlview::config::ConnInfo;
use psqlview::db;
use psqlview::types::SslMode;

fn parse_url_to_conninfo(url: &str) -> ConnInfo {
    // Minimal parser: postgres://user:pw@host:port/db
    let tail = url.strip_prefix("postgres://").expect("postgres:// prefix");
    let (creds, rest) = tail.split_once('@').expect("missing @");
    let (user, password) = creds.split_once(':').unwrap_or((creds, ""));
    let (host_port, db) = rest.split_once('/').expect("missing /db");
    let (host, port) = host_port.split_once(':').unwrap_or((host_port, "5432"));
    ConnInfo {
        host: host.into(),
        port: port.parse().expect("port"),
        user: user.into(),
        database: db.into(),
        password: password.into(),
        ssl_mode: SslMode::Disable,
        application_name: "psqlview-tests".into(),
    }
}

#[tokio::test]
#[ignore = "requires PSQLVIEW_PG_URL"]
async fn connects_and_detects_pg14_plus() {
    init_crypto();
    let Some(url) = pg_url() else {
        eprintln!("skipping: PSQLVIEW_PG_URL not set");
        return;
    };
    let info = parse_url_to_conninfo(&url);
    let session = db::connect::connect(&info).await.expect("connect");
    assert!(
        session.server_version.is_supported(),
        "target server should be PG14+ (got {})",
        session.server_version.display()
    );
    assert!(session.label.contains("postgres"));
}
