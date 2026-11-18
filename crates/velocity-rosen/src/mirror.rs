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
    Some(MirrorState::new(
        detail.rejected,
        detail
            .error
            .as_deref()
            .unwrap_or("Rosen reported no mirror error output."),
        retry_at,
        pushed,
    ))
}
