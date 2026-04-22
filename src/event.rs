use std::time::Duration;

use crossterm::event::{Event as CtEvent, EventStream, KeyEvent};
use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::db::{DbError, Session};
use crate::types::ResultSet;

/// Every message the event loop can receive.
///
/// Produced both by terminal input (via `spawn_terminal_events`) and by
/// background tasks (connect, query execution, catalog loads).
pub enum AppEvent {
    Key(KeyEvent),
    Resize(u16, u16),
    Tick,

    ConnectResult(Result<Session, DbError>),
    QueryResult(Result<ResultSet, DbError>),

    SchemasLoaded(Result<Vec<String>, DbError>),
    RelationsLoaded {
        schema: String,
        result: Result<Vec<crate::db::catalog::Relation>, DbError>,
    },
    ColumnsLoaded {
        schema: String,
        table: String,
        result: Result<Vec<crate::db::catalog::Column>, DbError>,
    },
}

/// Spawn a background task that pumps crossterm events and a tick timer
/// into the application's mpsc channel.
///
/// The task exits cleanly when the receiver is dropped or the event
/// stream returns an error.
pub fn spawn_terminal_events(tx: mpsc::UnboundedSender<AppEvent>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut events = EventStream::new();
        let mut tick = tokio::time::interval(Duration::from_millis(200));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                maybe = events.next() => {
                    match maybe {
                        Some(Ok(CtEvent::Key(k))) => {
                            if tx.send(AppEvent::Key(k)).is_err() { break; }
                        }
                        Some(Ok(CtEvent::Resize(w, h))) => {
                            if tx.send(AppEvent::Resize(w, h)).is_err() { break; }
                        }
                        Some(Ok(_)) => {}
                        Some(Err(err)) => {
                            tracing::warn!(error = %err, "event stream error; terminating input task");
                            break;
                        }
                        None => break,
                    }
                }
                _ = tick.tick() => {
                    if tx.send(AppEvent::Tick).is_err() { break; }
                }
            }
        }
    })
}
