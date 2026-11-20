//! Read the mirror's durable observation without changing Rosen's metadata.
//!
//! The pinned R9 stream omits event times and the last accepted commit. Its R1/R9
//! metadata supplies both, and lets startup recover a failure older than our cursor.
//! Keep these read-only queries aligned with the Rosen revision in Cargo.toml.

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde::Deserialize;
use velocity_core::rosen_mirror::MirrorState;

use crate::{ProjectServer, RosenError};

pub struct MirrorObservation {
    pub sequence: u64,
    pub state: Option<MirrorState>,
    attempted_commit: Option<String>,
}

/// Snapshot of a mirror that is behind the project's Rosen base (#509).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MirrorLag {
    pub remote: String,
    pub base_commit: String,
    pub pushed_commit: Option<String>,
    /// Unix seconds when the oldest unpushed commit landed (or the last mirror event).
    pub behind_since_secs: Option<u64>,
    pub rejected: bool,
    pub not_owner: bool,
    pub error: Option<String>,
}

#[derive(Deserialize)]
struct MirrorDetail {
    status: String,
    remote: Option<String>,
    commit: Option<String>,
    #[serde(default)]
    rejected: bool,
    error: Option<String>,
    retry_in_ms: Option<u64>,
}

pub async fn mirror_status(
    server: &ProjectServer,
) -> Result<Option<MirrorObservation>, RosenError> {
    let reading = server.clone();
    let mut observation = tokio::task::spawn_blocking(move || read_mirror(&reading))
        .await
        .map_err(|error| RosenError::MirrorData(error.to_string()))??;
    if let Some(observation) = &mut observation
        && recovered_after_restart(server, observation).await
    {
        observation.state = None;
    }
    Ok(observation)
}

/// The remote Rosen mirrors the base to, or `None` when the project has no mirror.
/// Rosen keeps the configured remote in its metadata, whichever way it was set.
pub async fn mirror_remote(server: &ProjectServer) -> Result<Option<String>, RosenError> {
    let reading = server.clone();
    tokio::task::spawn_blocking(move || read_mirror_remote(&reading))
        .await
        .map_err(|error| RosenError::MirrorData(error.to_string()))?
}

/// Returns how the mirror trails the base, or `None` when synced or unconfigured (#509).
pub async fn mirror_lag(server: &ProjectServer) -> Result<Option<MirrorLag>, RosenError> {
    let reading = server.clone();
    tokio::task::spawn_blocking(move || read_mirror_lag(&reading))
        .await
        .map_err(|error| RosenError::MirrorData(error.to_string()))?
}

fn read_mirror_lag(server: &ProjectServer) -> Result<Option<MirrorLag>, RosenError> {
    let path = server.data_dir().join("metadata.sqlite");
    if !path.is_file() {
        return Ok(None);
    }
    let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(std::time::Duration::from_secs(2))?;
    let transaction = connection.transaction()?;
    if !has_table(&transaction, "mirror")? || !has_table(&transaction, "base_refs")? {
        return Ok(None);
    }
    let Some((remote, pushed)): Option<(String, Option<String>)> = transaction
        .query_row(
            "SELECT remote, pushed FROM mirror WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let Some((base_ref, base_commit)): Option<(String, String)> = transaction
        .query_row("SELECT name, target FROM base_refs LIMIT 1", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?
    else {
        return Ok(None);
    };
    if pushed.as_deref() == Some(base_commit.as_str()) {
        return Ok(None);
    }
    let behind_since_ms = oldest_unpushed_ms(&transaction, &base_ref, pushed.as_deref())?;
    let (rejected, not_owner, error, event_ms) = latest_mirror_event(&transaction, &remote)?;
    Ok(Some(MirrorLag {
        remote,
        base_commit,
        pushed_commit: pushed,
        behind_since_secs: behind_since_ms.or(event_ms).map(|ms| ms / 1000),
        rejected,
        not_owner,
        error,
    }))
}

fn has_table(connection: &Connection, name: &str) -> Result<bool, rusqlite::Error> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |row| row.get(0),
    )
}

fn oldest_unpushed_ms(
    connection: &Connection,
    base_ref: &str,
    pushed: Option<&str>,
) -> Result<Option<u64>, rusqlite::Error> {
    if !has_table(connection, "base_history")? || !has_table(connection, "events")? {
        return Ok(None);
    }
    connection.query_row(
        "WITH unpushed AS (
             SELECT h.commit_id FROM base_history h
             WHERE h.ref_name = ?1
               AND h.seq > COALESCE(
                   (SELECT seq FROM base_history WHERE ref_name = ?1 AND commit_id = ?2),
                   0
               )
         )
         SELECT MIN(created_at) FROM events
         WHERE kind IN ('landed', 'reverted')
           AND json_extract(detail, '$.commit') IN (SELECT commit_id FROM unpushed)",
        rusqlite::params![base_ref, pushed],
        |row| row.get(0),
    )
}

fn latest_mirror_event(
    connection: &Connection,
    remote: &str,
) -> Result<(bool, bool, Option<String>, Option<u64>), rusqlite::Error> {
    if !has_table(connection, "events")? {
        return Ok((false, false, None, None));
    }
    let row: Option<(String, u64)> = connection
        .query_row(
            "SELECT detail, created_at FROM events WHERE kind = 'mirror' ORDER BY seq DESC LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((raw, created_at)) = row else {
        return Ok((false, false, None, None));
    };
    let Ok(detail) = serde_json::from_str::<MirrorDetail>(&raw) else {
        return Ok((false, false, None, Some(created_at)));
    };
    if detail
        .remote
        .as_deref()
        .is_some_and(|failed_remote| failed_remote != remote)
    {
        return Ok((false, false, None, None));
    }
    let not_owner = detail.status == "not_owner";
    let rejected = detail.status == "failed" && detail.rejected;
    let error = (detail.status == "failed")
        .then_some(detail.error)
        .flatten();
    Ok((rejected, not_owner, error, Some(created_at)))
}

fn read_mirror_remote(server: &ProjectServer) -> Result<Option<String>, RosenError> {
    let path = server.data_dir().join("metadata.sqlite");
    if !path.is_file() {
        return Ok(None);
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(std::time::Duration::from_secs(2))?;
    let has_table = connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'mirror'",
            [],
            |_| Ok(()),
        )
        .optional()?
        .is_some();
    if !has_table {
        return Ok(None);
    }
    Ok(connection
        .query_row("SELECT remote FROM mirror WHERE id = 1", [], |row| {
            row.get(0)
        })
        .optional()?)
}

/// A restarted mirror can push beyond the failed commit while more lands keep
/// arriving. An accepted descendant proves recovery even when the base is ahead.
async fn recovered_after_restart(server: &ProjectServer, observation: &MirrorObservation) -> bool {
    let Some(state) = &observation.state else {
        return false;
    };
    let (Some(attempted), Some(pushed)) =
        (&observation.attempted_commit, &state.last_pushed_commit)
    else {
        return false;
    };
    let Ok(attempted_identifier) = crate::policy::object(attempted) else {
        return false;
    };
    let Ok(client) = crate::connection::requests(server.endpoint()).await else {
        return false;
    };
    client
        .merge_base(attempted, pushed)
        .await
        .is_ok_and(|answer| answer.merge_base == Some(attempted_identifier))
}

/// Read one consistent snapshot. No database or table is created by this reader.
fn read_mirror(server: &ProjectServer) -> Result<Option<MirrorObservation>, RosenError> {
    let path = server.data_dir().join("metadata.sqlite");
    if !path.is_file() {
        return Ok(None);
    }
    let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(std::time::Duration::from_secs(2))?;
    let transaction = connection.transaction()?;
    let event: Option<(u64, String, u64)> = transaction.query_row(
        "SELECT seq, detail, created_at FROM events WHERE kind = 'mirror' ORDER BY seq DESC LIMIT 1",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).optional()?;
    let Some((sequence, detail, created_at)) = event else {
        return Ok(None);
    };
    let detail: MirrorDetail =
        serde_json::from_str(&detail).map_err(|error| RosenError::MirrorData(error.to_string()))?;
    let target: Option<(String, Option<String>)> = transaction
        .query_row(
            "SELECT remote, pushed FROM mirror WHERE id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let base: Option<String> = transaction
        .query_row("SELECT target FROM base_refs LIMIT 1", [], |row| row.get(0))
        .optional()?;
    let attempted_commit = detail.commit.clone();
    let state = failure_state(detail, created_at, target, base);
    Ok(Some(MirrorObservation {
        sequence,
        state,
        attempted_commit,
    }))
}

fn failure_state(
    detail: MirrorDetail,
    created_at: u64,
    target: Option<(String, Option<String>)>,
    base: Option<String>,
) -> Option<MirrorState> {
    let (remote, pushed) = target?;
    // A server restarted between failure and recovery emits no recovery event.
    // Its accepted commit still proves recovery, without re-raising an old rejection.
    let recovered = pushed.is_some() && (pushed == base || pushed == detail.commit);
    if detail.status != "failed"
        || recovered
        || detail
            .remote
            .as_ref()
            .is_some_and(|failed_remote| failed_remote != &remote)
    {
        return None;
    }
    let retry_at = detail
        .retry_in_ms
        .map(|delay| created_at.saturating_add(delay).to_string());
    Some(
        MirrorState::new(
            detail.rejected,
            detail
                .error
                .as_deref()
                .unwrap_or("Rosen reported no mirror error output."),
            retry_at,
            pushed,
        )
        .with_base_commit(base),
    )
}
