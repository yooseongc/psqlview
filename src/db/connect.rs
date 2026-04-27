use std::sync::Arc;
use std::time::Duration;

use tokio::time::timeout;
use tokio_postgres::{config::SslMode as PgSslMode, Config};

use crate::config::ConnInfo;
use crate::types::{ServerVersion, SslMode};

use super::{DbError, Session, TxStatus};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn connect(info: &ConnInfo) -> Result<Session, DbError> {
    let mut cfg = Config::new();
    cfg.host(&info.host)
        .port(info.port)
        .user(&info.user)
        .dbname(&info.database)
        .password(&info.password)
        .application_name(&info.application_name)
        .connect_timeout(Duration::from_secs(5))
        .ssl_mode(to_pg_sslmode(info.ssl_mode));

    let tls = build_tls_connector()?;

    let (client, connection) = timeout(CONNECT_TIMEOUT, cfg.connect(tls))
        .await
        .map_err(|_| DbError::Timeout)?
        .map_err(|e| DbError::Connect(e.to_string()))?;

    let cancel_token = client.cancel_token();

    tokio::spawn(async move {
        if let Err(err) = connection.await {
            tracing::warn!(error = %err, "postgres connection terminated");
        }
    });

    let version_row = client
        .query_one("SHOW server_version_num", &[])
        .await
        .map_err(|e| DbError::Connect(format!("server_version_num lookup failed: {e}")))?;
    let version_text: String = version_row.get(0);
    let version_num: u32 = version_text.trim().parse().unwrap_or(0);
    let server_version = ServerVersion::from_num(version_num);

    let label = format!(
        "{}@{}:{}/{}",
        info.user, info.host, info.port, info.database
    );

    Ok(Session {
        client: Arc::new(client),
        cancel_token,
        server_version,
        label,
        tx: TxStatus::Idle,
    })
}

fn to_pg_sslmode(mode: SslMode) -> PgSslMode {
    match mode {
        SslMode::Disable => PgSslMode::Disable,
        SslMode::Prefer => PgSslMode::Prefer,
        SslMode::Require => PgSslMode::Require,
    }
}

fn build_tls_connector() -> Result<tokio_postgres_rustls::MakeRustlsConnect, DbError> {
    let mut root_store = rustls::RootCertStore::empty();
    root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let client_config = rustls::ClientConfig::builder()
        .with_root_certificates(root_store)
        .with_no_client_auth();

    Ok(tokio_postgres_rustls::MakeRustlsConnect::new(client_config))
}
