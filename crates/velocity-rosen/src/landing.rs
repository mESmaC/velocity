//! Requests to land a change under the project's Rosen policy.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use rosen_client::Endpoint;
use rosen_client::protocol::LandStatus;
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use tokio::sync::RwLock;

use crate::workspace::session;
use crate::{ProjectServer, RosenError};

/// What a land request came to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LandOutcome {
    Landed { commit: String },
    Refused(LandRefusal),
}

/// What a read-only `land_check` reports for a change on the current base (#509).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LandCheckOutcome {
    Landable,
    AlreadyLanded { commit: String },
    Refused(LandRefusal),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LandRefusal {
    Regenerate { paths: Vec<String> },
    Conflict { paths: Vec<String>, message: String },
    Other { code: String, message: String },
}

fn map_refusal(reason: String, message: String, paths: Vec<String>) -> LandRefusal {
    match reason.as_str() {
        "regeneration_required" => LandRefusal::Regenerate { paths },
        "merge_conflict" | "conflicts" => LandRefusal::Conflict { paths, message },
        _ => LandRefusal::Other {
            code: reason,
            message,
        },
    }
}

/// One gate per server, by endpoint. Every land holds it shared while Rosen works on it;
/// replacing the server takes it alone (#515). Rosen's shutdown waits two seconds for
/// requests in flight, which a land that rebases and checks can outlast, so a server is
/// only stopped between lands, and a land that arrives meanwhile waits for the new one.
static LAND_GATES: LazyLock<Mutex<HashMap<String, Arc<RwLock<()>>>>> =
    LazyLock::new(Mutex::default);

pub(crate) fn land_gate(endpoint: &Endpoint) -> Arc<RwLock<()>> {
    let mut gates = LAND_GATES.lock().unwrap_or_else(|error| error.into_inner());
    Arc::clone(gates.entry(endpoint.to_string()).or_default())
}

/// Asks Rosen to land `change`.
pub async fn land(
    server: &ProjectServer,
    actor: &str,
    change: &str,
) -> Result<LandOutcome, RosenError> {
    let gate = land_gate(server.endpoint());
    let _landing = gate.read().await;
    let answer = session(server, actor).await?.land(change).await?;
    Ok(match answer.outcome {
        LandStatus::Landed { commit, .. } | LandStatus::AlreadyLanded { commit } => {
            LandOutcome::Landed {
                commit: commit.to_string(),
            }
        }
        LandStatus::Refused {
            reason,
            message,
            paths,
            ..
        } => LandOutcome::Refused(map_refusal(reason, message, paths)),
    })
}

/// Asks Rosen why `change` would not land on the current base, without mutating state (#509).
pub async fn land_check(
    server: &ProjectServer,
    change: &str,
) -> Result<LandCheckOutcome, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let check = client.land_check(change).await?;
    if let Some(commit) = check.landed {
        return Ok(LandCheckOutcome::AlreadyLanded {
            commit: commit.to_string(),
        });
    }
    if let Some(refusal) = check.refusal {
        return Ok(LandCheckOutcome::Refused(map_refusal(
            refusal.reason,
            refusal.message,
            refusal.paths,
        )));
    }
    Ok(LandCheckOutcome::Landable)
}

/// Unix seconds when `change` was last named, read from Rosen's event log (#509).
pub async fn named_since(server: &ProjectServer, change: &str) -> Result<Option<u64>, RosenError> {
    let reading = server.clone();
    let change = change.to_owned();
    tokio::task::spawn_blocking(move || read_named_since(&reading, &change))
        .await
        .map_err(|error| RosenError::StatusData(error.to_string()))?
}

fn read_named_since(server: &ProjectServer, change: &str) -> Result<Option<u64>, RosenError> {
    let path = server.data_dir().join("metadata.sqlite");
    if !path.is_file() {
        return Ok(None);
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(std::time::Duration::from_secs(2))?;
    let has_events: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'events')",
        [],
        |row| row.get(0),
    )?;
    if !has_events {
        return Ok(None);
    }
    let created_ms: Option<u64> = connection
        .query_row(
            "SELECT created_at FROM events
             WHERE change_id = ?1 AND kind = 'snapshot'
               AND json_extract(detail, '$.named') = 1
             ORDER BY seq DESC LIMIT 1",
            [change],
            |row| row.get(0),
        )
        .optional()?;
    Ok(created_ms.map(|ms| ms / 1000))
}

/// Server-assigned UTC millisecond timestamp for a change's `landed` event matching `commit`,
/// read from Rosen's event log (#644).
pub async fn landed_at_ms(
    server: &ProjectServer,
    change: &str,
    commit: &str,
) -> Result<Option<i64>, RosenError> {
    let reading = server.clone();
    let change = change.to_owned();
    let commit = commit.to_owned();
    tokio::task::spawn_blocking(move || read_landed_at_ms(&reading, &change, &commit))
        .await
        .map_err(|error| RosenError::StatusData(error.to_string()))?
}

fn read_landed_at_ms(
    server: &ProjectServer,
    change: &str,
    commit: &str,
) -> Result<Option<i64>, RosenError> {
    let path = server.data_dir().join("metadata.sqlite");
    if !path.is_file() {
        return Ok(None);
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(std::time::Duration::from_secs(2))?;
    let has_events: bool = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'events')",
        [],
        |row| row.get(0),
    )?;
    if !has_events {
        return Ok(None);
    }
    let created_ms: Option<i64> = connection
        .query_row(
            "SELECT created_at FROM events
             WHERE change_id = ?1 AND kind = 'landed'
               AND json_extract(detail, '$.commit') = ?2
             ORDER BY seq DESC LIMIT 1",
            [change, commit],
            |row| row.get(0),
        )
        .optional()?;
    Ok(created_ms.filter(|&millis| millis > 0))
}

/// Whether a queued notice still concerns a change that can be edited.
pub async fn is_open(server: &ProjectServer, change: &str) -> Result<bool, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    Ok(!client.change_show(change).await?.change.state.is_closed())
}
