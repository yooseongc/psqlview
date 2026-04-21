use zeroize::Zeroize;

use crate::types::SslMode;

#[derive(Clone)]
pub struct ConnInfo {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub database: String,
    pub password: String,
    pub ssl_mode: SslMode,
    pub application_name: String,
}

impl Default for ConnInfo {
    fn default() -> Self {
        Self {
            host: "localhost".into(),
            port: 5432,
            user: std::env::var("PGUSER").unwrap_or_else(|_| "postgres".into()),
            database: std::env::var("PGDATABASE").unwrap_or_else(|_| "postgres".into()),
            password: String::new(),
            ssl_mode: SslMode::Prefer,
            application_name: "psqlview".into(),
        }
    }
}

impl std::fmt::Debug for ConnInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnInfo")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("user", &self.user)
            .field("database", &self.database)
            .field("password", &"***")
            .field("ssl_mode", &self.ssl_mode.label())
            .field("application_name", &self.application_name)
            .finish()
    }
}

impl Drop for ConnInfo {
    fn drop(&mut self) {
        self.password.zeroize();
    }
}
