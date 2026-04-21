pub mod catalog;
pub mod connect;
pub mod query;

use std::sync::Arc;

use tokio_postgres::{CancelToken, Client};

use crate::types::ServerVersion;

#[derive(thiserror::Error, Debug)]
pub enum DbError {
    #[error("connection failed: {0}")]
    Connect(String),

    #[error("query error: {0}")]
    Query(#[from] tokio_postgres::Error),

    #[error("query cancelled")]
    Cancelled,

    #[error("timeout")]
    Timeout,

    #[error("tls setup: {0}")]
    Tls(String),
}

pub struct Session {
    pub(crate) client: Arc<Client>,
    pub(crate) cancel_token: CancelToken,
    pub server_version: ServerVersion,
    pub label: String,
}

impl Session {
    pub fn client(&self) -> Arc<Client> {
        self.client.clone()
    }

    pub fn cancel_token(&self) -> CancelToken {
        self.cancel_token.clone()
    }
}
