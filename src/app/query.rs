use std::time::{Duration, Instant};

use super::{App, FocusPane, QueryStatus};
use crate::db::catalog::RelationKind;
use crate::db::query::TxAction;
use crate::db::{self, catalog, TxStatus};
use crate::event::AppEvent;
use crate::types::ResultSet;

/// Row cap on the synthesized `SELECT *` issued by the tree-preview
/// shortcut (`p` on a relation). Kept low because the user is browsing,
/// not querying.
const PREVIEW_ROW_LIMIT: u32 = 200;

/// Quotes a Postgres identifier per the standard rules: wrap in double
/// quotes and double any internal quote.
pub(super) fn quote_ident(name: &str) -> String {
    let escaped = name.replace('"', "\"\"");
    format!("\"{escaped}\"")
}

/// Builds a preview `SELECT * FROM "schema"."relation" LIMIT n` query.
pub(super) fn build_preview_sql(schema: &str, relation: &str, limit: u32) -> String {
    format!(
        "SELECT * FROM {}.{} LIMIT {}",
        quote_ident(schema),
        quote_ident(relation),
        limit
    )
}

/// Wraps a multi-line DDL string in a synthetic single-column `ResultSet`
/// so the existing results pane can render and scroll it like any other
/// query output.
pub(super) fn ddl_to_resultset(text: &str, elapsed_ms: u128) -> ResultSet {
    let rows: Vec<Vec<crate::types::CellValue>> = text
        .split('\n')
        .map(|line| vec![crate::types::CellValue::Text(line.to_string())])
        .collect();
    ResultSet {
        columns: vec![crate::types::ColumnMeta {
            name: "ddl".into(),
            type_name: "text".into(),
        }],
        rows,
        truncated_at: None,
        command_tag: None,
        elapsed_ms,
    }
}

impl App {
    /// Re-runs the most recent query. When the last action was a `D`
    /// shortcut (DDL view), refreshes via the catalog using the stored
    /// `(schema, relation, kind)` rather than parsing the placeholder
    /// SQL — quoted identifiers with embedded dots survive correctly.
    pub(super) fn rerun_last_query(&mut self) {
        if let Some((schema, relation, kind)) = self.last_ddl_target.clone() {
            self.dispatch_ddl_fetch(schema, relation, kind);
            return;
        }
        let Some(sql) = self.last_run_sql.clone() else {
            self.toast_info("no previous query".into());
            return;
        };
        self.dispatch_sql(sql);
    }

    pub(super) fn run_current_query(&mut self) {
        // Run just the selected portion when one exists, so users can
        // execute a single statement from a buffer of many.
        let sql = self
            .editor()
            .selected_text()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| self.editor().text());
        if sql.trim().is_empty() {
            self.toast_info("editor is empty".into());
            return;
        }
        self.dispatch_sql(sql);
    }

    /// Spawns a query task for `sql` and updates the running-query state.
    /// No-op when there is no live session. Used by `run_current_query`,
    /// the tree-preview shortcut, and any other shortcut that wants to
    /// run a synthesized query without touching the editor.
    fn dispatch_sql(&mut self, sql: String) {
        let (client, cancel) = match self.session.as_ref() {
            Some(s) => (s.client(), s.cancel_token()),
            None => return,
        };
        self.autocomplete = None;
        self.push_history(&sql);
        self.query_status = QueryStatus::Running {
            started_at: Instant::now(),
            cancel,
        };
        self.last_run_sql = Some(sql.clone());
        // A fresh SQL dispatch invalidates the DDL re-run target — the
        // user now has actual rows in the result pane, not a synthetic
        // DDL view.
        self.last_ddl_target = None;
        self.results.begin_running();
        self.focus = FocusPane::Results;

        let tx = self.tx.clone();
        tokio::spawn(async move {
            let r = db::query::execute(client, &sql).await;
            let _ = tx.send(AppEvent::QueryResult(r));
        });
    }

    /// Builds a `SELECT * FROM "schema"."relation" LIMIT N` query and
    /// dispatches it. Identifier quoting protects against schemas /
    /// relation names containing special chars or reserved words.
    pub(super) fn run_preview_for_selected_relation(&mut self) {
        let Some(node) = self.tree.current_node() else {
            return;
        };
        if let crate::ui::schema_tree::NodeRef::Relation { schema, name, .. } = node {
            let sql = build_preview_sql(&schema, &name, PREVIEW_ROW_LIMIT);
            self.toast_info(format!("preview: {schema}.{name}"));
            self.dispatch_sql(sql);
        }
    }

    /// Fetches the DDL for the selected relation (CREATE TABLE for
    /// tables, pg_get_viewdef for views/matviews) and routes the text
    /// into the results pane as a single-column `ddl` result. Reuses
    /// the query lifecycle so the Running spinner / Done elapsed time
    /// / error toast all apply.
    pub(super) fn show_ddl_for_selected_relation(&mut self) {
        let Some(node) = self.tree.current_node() else {
            return;
        };
        let crate::ui::schema_tree::NodeRef::Relation {
            schema, name, kind, ..
        } = node
        else {
            return;
        };
        self.dispatch_ddl_fetch(schema, name, kind);
    }

    /// Spawns the DDL fetch task and primes the result pane / query
    /// status. Used by both the initial `D` shortcut and `R` re-runs.
    fn dispatch_ddl_fetch(&mut self, schema: String, name: String, kind: RelationKind) {
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let client = session.client();
        let cancel = session.cancel_token();
        self.autocomplete = None;
        self.query_status = QueryStatus::Running {
            started_at: Instant::now(),
            cancel,
        };
        self.last_run_sql = Some(format!("-- DDL of {schema}.{name}"));
        self.last_ddl_target = Some((schema.clone(), name.clone(), kind));
        self.results.begin_running();
        self.focus = FocusPane::Results;

        let tx = self.tx.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            let r = catalog::fetch_relation_ddl(&client, &schema, &name, kind).await;
            let event = match r {
                Ok(text) => Ok(ddl_to_resultset(&text, started.elapsed().as_millis())),
                Err(e) => Err(e),
            };
            let _ = tx.send(AppEvent::QueryResult(event));
        });
    }

    pub(super) fn cancel_running_query(&mut self) {
        if let QueryStatus::Running { cancel, .. } = &self.query_status {
            let cancel = cancel.clone();
            tokio::spawn(async move {
                let mut roots = rustls::RootCertStore::empty();
                roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
                let cfg = rustls::ClientConfig::builder()
                    .with_root_certificates(roots)
                    .with_no_client_auth();
                let tls = tokio_postgres_rustls::MakeRustlsConnect::new(cfg);
                let _ = cancel.cancel_query(tls).await;
            });
            self.toast_info("cancelling…".into());
        }
    }

    pub(super) fn on_query_result(&mut self, r: Result<ResultSet, db::DbError>) {
        let sql = self.last_run_sql.clone().unwrap_or_default();
        match r {
            Ok(set) => {
                self.query_status = QueryStatus::Done {
                    elapsed: Duration::from_millis(set.elapsed_ms as u64),
                };
                self.results.set_result(set);
                self.update_tx_after_query(&sql, true);
            }
            Err(db::DbError::Query(e))
                if e.code() == Some(&tokio_postgres::error::SqlState::QUERY_CANCELED)
                    || e.to_string().contains("canceling statement") =>
            {
                self.query_status = QueryStatus::Cancelled;
                self.results.clear();
                self.toast_info("query cancelled".into());
                // Cancellation doesn't itself change tx state — the
                // statement was aborted on the wire, server may still
                // be Active or have moved to InError. Treat as failure
                // for tx purposes so an Active session goes InError
                // (matches what the server will report on next query).
                self.update_tx_after_query(&sql, false);
            }
            Err(err) => {
                let detailed = err.format_detailed_with_sql(&sql);
                tracing::warn!(error = %detailed, "query failed");
                // Jump the editor caret to the offending position so the
                // user can start typing the fix without hunting for it.
                if let Some(pos) = err.original_position() {
                    self.editor_mut().move_cursor_to_char_position(pos);
                }
                self.query_status = QueryStatus::Failed(detailed.clone());
                self.results.clear();
                self.toast_error(detailed);
                self.update_tx_after_query(&sql, false);
            }
        }
    }

    /// Applies a `TxStatus` transition derived from the SQL keyword and
    /// success outcome. Toasts on transitions so the user sees that
    /// `BEGIN;` actually opened a transaction even when the result pane
    /// is empty.
    pub(super) fn update_tx_after_query(&mut self, sql: &str, ok: bool) {
        let action = db::query::tx_action(sql);
        let prev = match self.session.as_ref() {
            Some(s) => s.tx,
            None => return,
        };
        let new = compute_tx_transition(prev, action, ok);
        if new == prev {
            return;
        }
        if let Some(s) = self.session.as_mut() {
            s.tx = new;
        }
        if let Some(msg) = transition_toast(prev, new) {
            self.toast_info(msg.into());
        }
    }
}

/// Pure transition function — separated so it can be unit tested
/// without spinning up a session or running real queries.
pub(super) fn compute_tx_transition(
    prev: TxStatus,
    action: Option<TxAction>,
    ok: bool,
) -> TxStatus {
    match (prev, action, ok) {
        // Successful transitions driven by the keyword.
        (TxStatus::Idle, Some(TxAction::Begin), true) => TxStatus::Active,
        (TxStatus::Active, Some(TxAction::Commit), true) => TxStatus::Idle,
        (TxStatus::Active, Some(TxAction::Rollback), true) => TxStatus::Idle,
        (TxStatus::InError, Some(TxAction::Rollback), true) => TxStatus::Idle,
        (TxStatus::InError, Some(TxAction::Commit), true) => TxStatus::Idle,
        // Failure inside an active tx flips to InError. Failures in Idle
        // stay Idle (stand-alone statements don't open a tx).
        (TxStatus::Active, _, false) => TxStatus::InError,
        // Anything else: hold the previous state.
        _ => prev,
    }
}

pub(super) fn transition_toast(prev: TxStatus, new: TxStatus) -> Option<&'static str> {
    match (prev, new) {
        (TxStatus::Idle, TxStatus::Active) => Some("transaction started"),
        (TxStatus::Active, TxStatus::Idle) => Some("transaction ended"),
        (TxStatus::Active, TxStatus::InError) => {
            Some("transaction in error \u{2014} ROLLBACK to recover")
        }
        (TxStatus::InError, TxStatus::Idle) => Some("transaction recovered"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idle_plus_begin_success_becomes_active() {
        assert_eq!(
            compute_tx_transition(TxStatus::Idle, Some(TxAction::Begin), true),
            TxStatus::Active
        );
    }

    #[test]
    fn active_plus_commit_or_rollback_success_returns_to_idle() {
        assert_eq!(
            compute_tx_transition(TxStatus::Active, Some(TxAction::Commit), true),
            TxStatus::Idle
        );
        assert_eq!(
            compute_tx_transition(TxStatus::Active, Some(TxAction::Rollback), true),
            TxStatus::Idle
        );
    }

    #[test]
    fn active_plus_any_failure_flips_to_in_error() {
        assert_eq!(
            compute_tx_transition(TxStatus::Active, None, false),
            TxStatus::InError
        );
        // BEGIN failure mid-tx: still flips to InError.
        assert_eq!(
            compute_tx_transition(TxStatus::Active, Some(TxAction::Begin), false),
            TxStatus::InError
        );
    }

    #[test]
    fn idle_plus_failure_stays_idle() {
        // Stand-alone failing statement doesn't open a tx.
        assert_eq!(
            compute_tx_transition(TxStatus::Idle, None, false),
            TxStatus::Idle
        );
    }

    #[test]
    fn in_error_plus_rollback_recovers_to_idle() {
        assert_eq!(
            compute_tx_transition(TxStatus::InError, Some(TxAction::Rollback), true),
            TxStatus::Idle
        );
    }

    #[test]
    fn in_error_plus_any_failure_stays_in_error() {
        assert_eq!(
            compute_tx_transition(TxStatus::InError, None, false),
            TxStatus::InError
        );
    }

    #[test]
    fn transition_toast_only_fires_on_real_change() {
        // No-op transitions (Idle→Idle, Active→Active) silent.
        assert_eq!(transition_toast(TxStatus::Idle, TxStatus::Idle), None);
        // Real transitions emit a one-line toast.
        assert!(transition_toast(TxStatus::Idle, TxStatus::Active).is_some());
        assert!(transition_toast(TxStatus::Active, TxStatus::Idle).is_some());
        assert!(transition_toast(TxStatus::Active, TxStatus::InError).is_some());
        assert!(transition_toast(TxStatus::InError, TxStatus::Idle).is_some());
    }
}
