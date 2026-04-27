use std::sync::Arc;
use std::time::Instant;

use futures_util::StreamExt;
use tokio_postgres::{
    types::{ToSql, Type},
    Client, Row, SimpleQueryMessage,
};

use crate::types::{CellValue, ColumnMeta, ResultSet};

use super::DbError;

/// Upper bound on rows we'll buffer in memory for a single query.
pub const ROW_LIMIT: usize = 10_000;

pub async fn execute(client: Arc<Client>, sql: &str) -> Result<ResultSet, DbError> {
    let start = Instant::now();
    let trimmed = strip_leading_noise(sql);

    if trimmed.is_empty() {
        return Ok(ResultSet::empty_with_tag("empty", 0));
    }

    if returns_rows(trimmed) {
        run_select(&client, trimmed, start).await
    } else {
        run_simple(&client, trimmed, start).await
    }
}

async fn run_select(client: &Client, sql: &str, start: Instant) -> Result<ResultSet, DbError> {
    let params: [&(dyn ToSql + Sync); 0] = [];
    let stream = client.query_raw(sql, params).await?;
    futures_util::pin_mut!(stream);

    let mut set = ResultSet::default();
    let mut count = 0usize;

    while let Some(row) = stream.next().await {
        let row = row?;
        if set.columns.is_empty() {
            set.columns = row
                .columns()
                .iter()
                .map(|c| ColumnMeta {
                    name: c.name().to_string(),
                    type_name: c.type_().name().to_string(),
                })
                .collect();
        }
        if count >= ROW_LIMIT {
            set.truncated_at = Some(ROW_LIMIT);
            break;
        }
        set.rows.push(row_to_cells(&row));
        count += 1;
    }

    set.elapsed_ms = start.elapsed().as_millis();
    set.command_tag = Some(format!(
        "{count} row{}{}",
        if count == 1 { "" } else { "s" },
        if set.truncated_at.is_some() {
            " (truncated)"
        } else {
            ""
        }
    ));
    Ok(set)
}

async fn run_simple(client: &Client, sql: &str, start: Instant) -> Result<ResultSet, DbError> {
    let msgs = client.simple_query(sql).await?;
    let mut last_tag = None;
    for msg in msgs {
        if let SimpleQueryMessage::CommandComplete(n) = msg {
            last_tag = Some(n);
        }
    }
    let tag = match last_tag {
        Some(n) => format!("{n} row(s) affected"),
        None => "OK".to_string(),
    };
    Ok(ResultSet::empty_with_tag(tag, start.elapsed().as_millis()))
}

fn row_to_cells(row: &Row) -> Vec<CellValue> {
    row.columns()
        .iter()
        .enumerate()
        .map(|(i, c)| convert_cell(row, i, c.type_()))
        .collect()
}

fn convert_cell(row: &Row, idx: usize, ty: &Type) -> CellValue {
    let name = ty.name().to_string();
    macro_rules! opt {
        ($t:ty, $map:expr) => {
            match row.try_get::<usize, Option<$t>>(idx) {
                Ok(Some(v)) => $map(v),
                Ok(None) => CellValue::Null,
                Err(_) => CellValue::Unsupported(name.clone()),
            }
        };
    }

    match *ty {
        Type::BOOL => opt!(bool, CellValue::Bool),
        Type::INT2 => opt!(i16, |v: i16| CellValue::Int(v as i64)),
        Type::INT4 => opt!(i32, |v: i32| CellValue::Int(v as i64)),
        Type::INT8 => opt!(i64, CellValue::Int),
        Type::OID => opt!(u32, |v: u32| CellValue::Int(v as i64)),
        Type::FLOAT4 => opt!(f32, |v: f32| CellValue::Float(v as f64)),
        Type::FLOAT8 => opt!(f64, CellValue::Float),
        Type::NUMERIC => opt!(rust_decimal::Decimal, CellValue::Numeric),
        Type::TEXT | Type::VARCHAR | Type::NAME | Type::BPCHAR | Type::UNKNOWN => {
            opt!(String, CellValue::Text)
        }
        Type::DATE => opt!(chrono::NaiveDate, CellValue::Date),
        Type::TIME => opt!(chrono::NaiveTime, CellValue::Time),
        Type::TIMESTAMP => opt!(chrono::NaiveDateTime, CellValue::Timestamp),
        Type::TIMESTAMPTZ => opt!(chrono::DateTime<chrono::Utc>, CellValue::TimestampTz),
        Type::JSON | Type::JSONB => opt!(serde_json::Value, |v: serde_json::Value| {
            CellValue::Json(v.to_string())
        }),
        Type::UUID => opt!(uuid::Uuid, |v: uuid::Uuid| CellValue::Text(v.to_string())),
        Type::BYTEA => match row.try_get::<usize, Option<Vec<u8>>>(idx) {
            Ok(Some(v)) => CellValue::Bytes(v.len()),
            Ok(None) => CellValue::Null,
            Err(_) => CellValue::Unsupported(name.clone()),
        },
        Type::INET => opt!(std::net::IpAddr, |v: std::net::IpAddr| CellValue::Text(
            v.to_string()
        )),
        _ => CellValue::Unsupported(name),
    }
}

/// Strip leading whitespace and SQL comments. Used to classify the statement.
pub fn strip_leading_noise(sql: &str) -> &str {
    let mut s = sql.trim_start();
    loop {
        if let Some(rest) = s.strip_prefix("--") {
            s = match rest.find('\n') {
                Some(nl) => rest[nl + 1..].trim_start(),
                None => return "",
            };
        } else if let Some(rest) = s.strip_prefix("/*") {
            s = match rest.find("*/") {
                Some(end) => rest[end + 2..].trim_start(),
                None => return "",
            };
        } else {
            return s;
        }
    }
}

pub fn returns_rows(sql: &str) -> bool {
    let first: String = strip_leading_noise(sql)
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    matches!(
        first.to_ascii_uppercase().as_str(),
        "SELECT" | "WITH" | "VALUES" | "TABLE" | "SHOW" | "EXPLAIN" | "FETCH"
    )
}

/// Transaction-control action that the App layer applies to its
/// `TxStatus` after a successful query. Determined by scanning the
/// user-submitted SQL — no extra round-trip to the server.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxAction {
    Begin,
    Commit,
    Rollback,
}

/// Returns the *last* transaction-control keyword across all
/// statements in `sql`. Caller applies the result if the query
/// succeeded; on error we don't consult this (the App handles the
/// Active → InError transition independently).
///
/// Splits on `;` naively — does not understand `;` inside string
/// literals or block comments. Adequate for typical interactive
/// SQL where tx-control statements stand alone.
pub fn tx_action(sql: &str) -> Option<TxAction> {
    let mut last: Option<TxAction> = None;
    for stmt in sql.split(';') {
        let stripped = strip_leading_noise(stmt);
        let first_word: String = stripped
            .chars()
            .take_while(|c| c.is_ascii_alphabetic())
            .collect();
        let action = match first_word.to_ascii_uppercase().as_str() {
            "BEGIN" | "START" => Some(TxAction::Begin),
            "COMMIT" | "END" => Some(TxAction::Commit),
            "ROLLBACK" | "ABORT" => Some(TxAction::Rollback),
            _ => None,
        };
        if action.is_some() {
            last = action;
        }
    }
    last
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_handles_line_and_block_comments() {
        assert_eq!(strip_leading_noise("  -- hi\nSELECT 1"), "SELECT 1");
        assert_eq!(strip_leading_noise("/* x */ SELECT 1"), "SELECT 1");
        assert_eq!(
            strip_leading_noise("  \n\t  INSERT INTO t VALUES (1)"),
            "INSERT INTO t VALUES (1)"
        );
    }

    #[test]
    fn returns_rows_classifies_selectish() {
        assert!(returns_rows("SELECT 1"));
        assert!(returns_rows("  with x as (select 1) select * from x"));
        assert!(returns_rows("VALUES (1), (2)"));
        assert!(returns_rows("EXPLAIN ANALYZE SELECT 1"));
        assert!(!returns_rows("INSERT INTO t VALUES (1)"));
        assert!(!returns_rows("BEGIN"));
        assert!(!returns_rows("CREATE TABLE x (id int)"));
    }

    #[test]
    fn strip_handles_unterminated_comments() {
        assert_eq!(strip_leading_noise("-- no newline"), "");
        assert_eq!(strip_leading_noise("/* not closed"), "");
        // Still handles multiple comment prefixes that eventually stop at real SQL.
        assert_eq!(
            strip_leading_noise("/* a */ -- b\n/* c */SELECT 1"),
            "SELECT 1"
        );
    }

    #[test]
    fn returns_rows_is_case_insensitive_and_handles_whitespace() {
        assert!(returns_rows("  \nSelect 1"));
        assert!(returns_rows("\t\tshow TIMEZONE"));
        assert!(returns_rows("fetch 10 FROM c"));
        assert!(returns_rows("Table foo"));
        assert!(!returns_rows("deallocate all"));
        assert!(!returns_rows("copy t to stdout"));
        assert!(!returns_rows(""));
    }

    #[test]
    fn tx_action_recognises_each_keyword_case_insensitively() {
        assert_eq!(tx_action("BEGIN"), Some(TxAction::Begin));
        assert_eq!(
            tx_action("begin transaction read only"),
            Some(TxAction::Begin)
        );
        assert_eq!(tx_action("START TRANSACTION"), Some(TxAction::Begin));
        assert_eq!(tx_action("COMMIT"), Some(TxAction::Commit));
        assert_eq!(tx_action("end"), Some(TxAction::Commit));
        assert_eq!(tx_action("ROLLBACK"), Some(TxAction::Rollback));
        assert_eq!(tx_action("abort"), Some(TxAction::Rollback));
    }

    #[test]
    fn tx_action_returns_none_for_non_tx_statements() {
        assert_eq!(tx_action("SELECT 1"), None);
        assert_eq!(tx_action("INSERT INTO t VALUES (1)"), None);
        assert_eq!(tx_action(""), None);
        assert_eq!(tx_action("-- begin in a comment"), None);
    }

    #[test]
    fn tx_action_returns_last_keyword_across_multiple_statements() {
        // The user typed the whole BEGIN/SELECT/COMMIT block in one
        // F5 — net state should be Idle (last keyword = COMMIT).
        assert_eq!(
            tx_action("BEGIN; SELECT 1; COMMIT;"),
            Some(TxAction::Commit)
        );
        // Rollback within a sequence wins as the final action.
        assert_eq!(
            tx_action("BEGIN; INSERT INTO t VALUES (1); ROLLBACK"),
            Some(TxAction::Rollback)
        );
        // A trailing BEGIN after a Rollback should still be Begin.
        assert_eq!(tx_action("ROLLBACK; BEGIN"), Some(TxAction::Begin));
    }

    #[test]
    fn tx_action_skips_leading_comments_per_statement() {
        assert_eq!(tx_action("-- starting tx\nBEGIN"), Some(TxAction::Begin));
        assert_eq!(
            tx_action("/* setup */ BEGIN; SELECT 1; /* done */ COMMIT;"),
            Some(TxAction::Commit)
        );
    }
}
