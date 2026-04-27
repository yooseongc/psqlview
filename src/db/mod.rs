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

    #[error("{0}")]
    Other(String),
}

impl DbError {
    /// Returns a multi-line human-readable rendering. For `Query` errors
    /// that originated on the server, pulls out SQLSTATE, severity, DETAIL,
    /// HINT, and POSITION. Falls back to the `Display` impl otherwise.
    pub fn format_detailed(&self) -> String {
        match self {
            DbError::Query(e) => format_pg_error(e),
            _ => self.to_string(),
        }
    }

    /// Returns the 1-based character position reported by Postgres for
    /// this error, when present. Used to jump the editor caret to where
    /// the user needs to fix.
    pub fn original_position(&self) -> Option<u32> {
        let DbError::Query(e) = self else {
            return None;
        };
        let db = e.as_db_error()?;
        match db.position()? {
            tokio_postgres::error::ErrorPosition::Original(p) => Some(*p),
            tokio_postgres::error::ErrorPosition::Internal { .. } => None,
        }
    }

    /// Same as `format_detailed` but, when the error has a POSITION, appends
    /// a snippet of the offending SQL with a caret pointing at it.
    pub fn format_detailed_with_sql(&self, sql: &str) -> String {
        let mut out = self.format_detailed();
        if let DbError::Query(e) = self {
            if let Some(db) = e.as_db_error() {
                if let Some(pos) = db.position() {
                    let p = match pos {
                        tokio_postgres::error::ErrorPosition::Original(p) => *p,
                        tokio_postgres::error::ErrorPosition::Internal { position, .. } => {
                            *position
                        }
                    };
                    if let Some(snippet) = position_snippet(sql, p) {
                        out.push('\n');
                        out.push_str(&snippet);
                    }
                }
            }
        }
        out
    }
}

/// Builds a two- or three-line snippet pointing at `position_1based` inside
/// `sql`. Returns None if the position falls outside the text.
fn position_snippet(sql: &str, position_1based: u32) -> Option<String> {
    let pos = (position_1based as usize).checked_sub(1)?;
    let mut char_idx = 0usize;
    for (line_idx, line) in sql.lines().enumerate() {
        let line_chars = line.chars().count();
        if pos <= char_idx + line_chars {
            let col = pos - char_idx;
            let caret_pad: String = " ".repeat(col);
            return Some(format!(
                "  {line}\n  {caret_pad}^  (line {}, col {})",
                line_idx + 1,
                col + 1
            ));
        }
        char_idx += line_chars + 1; // +1 for the newline separator
    }
    None
}

fn format_pg_error(e: &tokio_postgres::Error) -> String {
    let Some(db) = e.as_db_error() else {
        return e.to_string();
    };
    let mut out = format!("{} {}: {}", db.severity(), db.code().code(), db.message());
    if let Some(detail) = db.detail() {
        out.push_str("\nDETAIL: ");
        out.push_str(detail);
    }
    if let Some(hint) = db.hint() {
        out.push_str("\nHINT: ");
        out.push_str(hint);
    }
    if let Some(pos) = db.position() {
        use tokio_postgres::error::ErrorPosition;
        match pos {
            ErrorPosition::Original(p) => {
                out.push_str(&format!("\nPOSITION: {p}"));
            }
            ErrorPosition::Internal { position, query } => {
                out.push_str(&format!(
                    "\nPOSITION: {position} in internal query: {query}"
                ));
            }
        }
    }
    // Constraint violations expose the offending object.
    if let Some(schema) = db.schema() {
        if let Some(table) = db.table() {
            match db.column() {
                Some(col) => out.push_str(&format!("\nAT: {schema}.{table}.{col}")),
                None => out.push_str(&format!("\nAT: {schema}.{table}")),
            }
        }
    }
    if let Some(constraint) = db.constraint() {
        out.push_str(&format!("\nCONSTRAINT: {constraint}"));
    }
    out
}

/// Session-local view of the server's transaction state.
///
/// Tracked locally by scanning the user-submitted SQL for transaction
/// control keywords (BEGIN / COMMIT / END / ROLLBACK / ABORT) and
/// combining with query success/failure. tokio-postgres does not
/// expose the protocol-level ReadyForQuery indicator, and querying
/// the server every round-trip would (a) cost an extra round-trip and
/// (b) fail outright while in the InError state. Local tracking is
/// pragmatic and matches the user's mental model: "I typed BEGIN, so
/// I'm in a transaction".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TxStatus {
    /// Auto-commit mode — every statement is its own transaction.
    #[default]
    Idle,
    /// Inside an explicit transaction block.
    Active,
    /// Inside a transaction that has errored — only ROLLBACK / ABORT
    /// will recover.
    InError,
}

pub struct Session {
    pub(crate) client: Arc<Client>,
    pub(crate) cancel_token: CancelToken,
    pub server_version: ServerVersion,
    pub label: String,
    pub tx: TxStatus,
}

impl Session {
    pub fn client(&self) -> Arc<Client> {
        self.client.clone()
    }

    pub fn cancel_token(&self) -> CancelToken {
        self.cancel_token.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn position_snippet_points_caret_at_single_line_query() {
        let snip = position_snippet("select * from psqlview_test.user;", 15).expect("snippet");
        let lines: Vec<&str> = snip.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], "  select * from psqlview_test.user;");
        // 14 leading spaces (2 for indent + 14 for position 14 zero-based).
        assert!(lines[1].starts_with("                ^"));
        assert!(lines[1].contains("line 1"));
        assert!(lines[1].contains("col 15"));
    }

    #[test]
    fn position_snippet_handles_multi_line_query() {
        let sql = "SELECT\n  *\nFROM nope;";
        // "FROM" starts at pos 1-based 12 in overall text:
        //   S E L E C T \n     *   \n  F R O M
        //   1 2 3 4 5 6  7 8 9 10 11 12 13 14 15
        let snip = position_snippet(sql, 12).expect("snippet");
        let lines: Vec<&str> = snip.lines().collect();
        assert_eq!(lines[0], "  FROM nope;");
        assert!(lines[1].starts_with("  ^"));
        assert!(lines[1].contains("line 3"));
        assert!(lines[1].contains("col 1"));
    }

    #[test]
    fn position_snippet_returns_none_when_out_of_range() {
        assert!(position_snippet("short", 999).is_none());
        assert!(position_snippet("", 1).is_none());
    }

    #[test]
    fn position_snippet_handles_end_of_line_position() {
        // position one past last char of first line points at the newline.
        let snip = position_snippet("SELECT 1\nFROM t;", 9).expect("snippet");
        let lines: Vec<&str> = snip.lines().collect();
        assert_eq!(lines[0], "  SELECT 1");
        assert!(lines[1].contains("line 1"));
        assert!(lines[1].contains("col 9"));
    }

    #[test]
    fn position_snippet_handles_position_at_end_of_sql() {
        // position at the char just past the semicolon.
        let snip = position_snippet("SELECT 1;", 10).expect("snippet");
        let lines: Vec<&str> = snip.lines().collect();
        assert_eq!(lines[0], "  SELECT 1;");
        assert!(lines[1].contains("col 10"));
    }

    #[test]
    fn position_snippet_handles_unicode_identifier() {
        // Postgres POSITION is character-based (not byte). Identifier "éa"
        // has 2 chars but 3 bytes. Position 5 points at 'a' of the invalid
        // ref.
        let snip = position_snippet("FROM \u{00e9}a", 5).expect("snippet");
        let lines: Vec<&str> = snip.lines().collect();
        assert_eq!(lines[0], "  FROM \u{00e9}a");
        assert!(lines[1].contains("col 5"));
    }

    #[test]
    fn format_detailed_falls_back_to_display_for_non_query_variants() {
        assert_eq!(
            DbError::Connect("boom".into()).format_detailed(),
            "connection failed: boom"
        );
        assert_eq!(DbError::Timeout.format_detailed(), "timeout");
        assert_eq!(DbError::Cancelled.format_detailed(), "query cancelled");
        assert_eq!(
            DbError::Tls("bad cert".into()).format_detailed(),
            "tls setup: bad cert"
        );
    }
}
