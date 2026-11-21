//! Read models and workspace operations for the F6 Rosen thread view.

use std::path::Path;
use std::time::Duration;

use rosen_client::Client;
use rosen_client::protocol::LogRequest;
use rosen_core::{ChangeState, SnapshotKind};
use rusqlite::{Connection, OpenFlags};
use velocity_core::thread_view_model::{
    RosenChangeStage, RosenChangeView, RosenLandFailureView, RosenLandView,
};

use crate::{ProjectServer, RosenError, connection::requests};

pub struct ThreadChangeQuery {
    pub identifier: String,
    pub fixing: bool,
    pub imported_through: u64,
    pub previous: Option<RosenChangeView>,
}

/// Use the protocol for change state and commit trailers. The pinned protocol
/// exposes conflict IDs and the active land, so retained land attempts and conflict paths
/// come from Rosen's read-only metadata ledger.
pub async fn read(
    server: &ProjectServer,
    query: ThreadChangeQuery,
) -> Result<Option<RosenChangeView>, RosenError> {
    let mut results = read_batch(server, vec![query]).await?;
    Ok(results.pop().and_then(|(_, view)| view))
}

/// Reads several thread changes in one pass, reusing one shared Rosen connection and
/// opening `metadata.sqlite` once for the entire batch (#555).
pub async fn read_batch(
    server: &ProjectServer,
    queries: Vec<ThreadChangeQuery>,
) -> Result<Vec<(String, Option<RosenChangeView>)>, RosenError> {
    if queries.is_empty() {
        return Ok(Vec::new());
    }
    let client = requests(server.endpoint()).await?;
    let mut found = Vec::with_capacity(queries.len());
    let mut missing = Vec::new();
    for query in queries {
        match client.change_show(&query.identifier).await {
            Ok(response) => found.push((query, response.change)),
            Err(rosen_client::Error::Server(error))
                if error.code == rosen_client::protocol::ErrorCode::NotFound =>
            {
                missing.push((query.identifier, None));
            }
            Err(error) => return Err(error.into()),
        }
    }
    if found.is_empty() {
        return Ok(missing);
    }
    let ledger_path = server.data_dir().join("metadata.sqlite");
    let specs: Vec<(String, u64)> = found
        .iter()
        .map(|(query, _)| (query.identifier.clone(), query.imported_through))
        .collect();
    let mut facts_list =
        tokio::task::spawn_blocking(move || read_ledger_batch(&ledger_path, &specs))
            .await
            .map_err(|error| RosenError::ThreadChange(error.to_string()))??;
    let mut results = Vec::with_capacity(found.len() + missing.len());
    results.extend(missing);
    for ((query, record), facts) in found.into_iter().zip(facts_list.drain(..)) {
        let identifier = query.identifier.clone();
        let view = build_view(&client, query, record, facts).await?;
        results.push((identifier, Some(view)));
    }
    Ok(results)
}

async fn build_view(
    client: &Client,
    query: ThreadChangeQuery,
    record: rosen_core::Change,
    facts: LedgerFacts,
) -> Result<RosenChangeView, RosenError> {
    let name = facts
        .may_have_named_work
        .then(|| snapshot_name(&record))
        .flatten();
    let mut lands = Vec::new();
    let previous = query.previous.map_or_else(Vec::new, |view| view.lands);
    for land in facts.lands {
        let cached = previous.iter().find(|entry| entry.commit == land.commit);
        lands.push(land_view(client, land, cached).await?);
    }
    let stage = change_stage(
        record.state,
        &facts.conflicts,
        lands.last(),
        name.is_some(),
        query.fixing,
    );
    Ok(RosenChangeView {
        identifier: query.identifier,
        stage,
        name,
        conflicts: if record.state.is_closed() {
            Vec::new()
        } else {
            facts.conflicts
        },
        lands,
        resolution: None,
        follow_up_error: None,
    })
}

fn snapshot_name(record: &rosen_core::Change) -> Option<String> {
    record
        .snapshots
        .iter()
        .rev()
        .find_map(|snapshot| match &snapshot.kind {
            SnapshotKind::Named(message) => Some(message.clone()),
            SnapshotKind::Automatic => None,
        })
}

fn change_stage(
    state: ChangeState,
    conflicts: &[String],
    latest: Option<&RosenLandView>,
    named: bool,
    fixing: bool,
) -> RosenChangeStage {
    use RosenChangeStage::{Abandoned, Conflict, Editing, Green, Landed, Landing, Named, Reopened};
    match state {
        ChangeState::Abandoned => Abandoned,
        ChangeState::Landed
            if latest.is_some_and(|land| land.checks_passed && land.revert.is_none()) =>
        {
            Green
        }
        ChangeState::Landed => Landed,
        _ if !conflicts.is_empty() => Conflict,
        ChangeState::Ready if !fixing => Landing,
        _ if fixing && latest.is_some_and(|land| land.revert.is_some()) => Reopened,
        _ if named && !fixing => Named,
        _ => Editing,
    }
}

struct LandFact {
    commit: String,
    build_passed: bool,
    checks_passed: bool,
    revert: Option<String>,
    revert_output: Option<String>,
    failures: Vec<RosenLandFailureView>,
}

struct LedgerFacts {
    may_have_named_work: bool,
    conflicts: Vec<String>,
    lands: Vec<LandFact>,
}

fn ledger(path: &Path) -> Result<Connection, RosenError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(Duration::from_secs(2))?;
    Ok(connection)
}

fn read_ledger_batch(
    path: &Path,
    queries: &[(String, u64)],
) -> Result<Vec<LedgerFacts>, RosenError> {
    let mut connection = ledger(path)?;
    let transaction = connection.transaction()?;
    let mut results = Vec::with_capacity(queries.len());
    for (change, imported_through) in queries {
        let conflicts = conflict_paths(&transaction, change)?;
        // Imported Git commits carry names, but only a later Rosen snapshot means
        // the owner named this work for Velocity to land.
        let may_have_named_work = *imported_through == 0
            || transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM events WHERE seq>?1 AND change_id=?2
                 AND kind='snapshot' AND json_extract(detail,'$.named')=1)",
                (imported_through, change),
                |row| row.get(0),
            )?;
        let lands = read_lands(&transaction, change)?;
        results.push(LedgerFacts {
            may_have_named_work,
            conflicts,
            lands,
        });
    }
    Ok(results)
}

fn read_lands(connection: &Connection, change: &str) -> rusqlite::Result<Vec<LandFact>> {
    let mut query = connection.prepare(
        "SELECT h.commit_id, h.seq<=COALESCE(b.seq,0), h.seq<=COALESCE(g.seq,0),
                r.commit_id, r.output
         FROM base_history h
         LEFT JOIN post_land_build_green b ON b.ref_name=h.ref_name
         LEFT JOIN post_land_green g ON g.ref_name=h.ref_name
         LEFT JOIN auto_reverts r ON r.culprit=h.commit_id
         WHERE h.change_id=?1 ORDER BY h.seq",
    )?;
    query
        .query_map([change], |row| {
            let commit: String = row.get(0)?;
            Ok(LandFact {
                failures: read_failures(connection, &commit)?,
                commit,
                build_passed: row.get(1)?,
                checks_passed: row.get(2)?,
                revert: row.get(3)?,
                revert_output: row.get(4)?,
            })
        })?
        .collect()
}

fn read_failures(
    connection: &Connection,
    commit: &str,
) -> rusqlite::Result<Vec<RosenLandFailureView>> {
    let mut query = connection.prepare(
        "SELECT kind, output FROM post_land_failures WHERE commit_id=?1 ORDER BY session",
    )?;
    query
        .query_map([commit], |row| {
            let kind: String = row.get(0)?;
            Ok(RosenLandFailureView {
                label: if kind == "build" {
                    "Post-land build failed"
                } else {
                    "Post-land checks failed"
                }
                .into(),
                output: row.get(1)?,
            })
        })?
        .collect()
}

fn conflict_paths(connection: &Connection, change: &str) -> rusqlite::Result<Vec<String>> {
    let mut query = connection.prepare(
        "SELECT DISTINCT path FROM conflicts WHERE change_id=?1 AND state='open' ORDER BY path",
    )?;
    query.query_map([change], |row| row.get(0))?.collect()
}

/// Read the files still blocking this change, including resolutions made since the last event.
pub async fn open_conflicts(
    server: &ProjectServer,
    change: &str,
) -> Result<Vec<String>, RosenError> {
    let path = server.data_dir().join("metadata.sqlite");
    let change = change.to_owned();
    tokio::task::spawn_blocking(move || Ok(conflict_paths(&ledger(&path)?, &change)?))
        .await
        .map_err(|error| RosenError::ThreadChange(error.to_string()))?
}

pub async fn resolution_prompt(
    server: &ProjectServer,
    change: &str,
) -> Result<Option<String>, RosenError> {
    let paths = open_conflicts(server, change).await?;
    Ok((!paths.is_empty()).then(|| {
        format!(
            "Resolve the stored conflicts in Rosen change {change}. {}",
            crate::conflicts::conflict_instructions(&paths)
        )
    }))
}

async fn land_view(
    client: &Client,
    mut land: LandFact,
    cached: Option<&RosenLandView>,
) -> Result<RosenLandView, RosenError> {
    let (summary, change, receipt, approvers) = if let Some(view) = cached {
        (
            view.summary.clone(),
            view.change.clone(),
            view.receipt.clone(),
            view.approvers.clone(),
        )
    } else {
        let request = LogRequest {
            at: Some(land.commit.clone()),
            limit: Some(1),
            ..LogRequest::default()
        };
        let log = client.log(request).await?;
        let entry = log
            .entries
            .into_iter()
            .next()
            .ok_or_else(|| RosenError::InvalidId(land.commit.clone()))?;
        let change = entry
            .change
            .ok_or_else(|| RosenError::InvalidId(land.commit.clone()))?;
        (
            entry.summary,
            change.to_string(),
            entry.receipt.map(|value| value.to_string()),
            entry.approvers.iter().map(ToString::to_string).collect(),
        )
    };
    if let Some(output) = land.revert_output
        && !land.failures.iter().any(|failure| failure.output == output)
    {
        land.failures.push(RosenLandFailureView {
            label: "Revert output".into(),
            output,
        });
    }
    Ok(RosenLandView {
        commit: land.commit,
        summary,
        change,
        receipt,
        approvers,
        build_passed: land.build_passed,
        checks_passed: land.checks_passed,
        revert: land.revert,
        failures: land.failures,
    })
}

/// Stores the current workspace as a named snapshot, the same operation as `rosen commit -m`.
pub async fn name(
    server: &ProjectServer,
    root: &Path,
    change: &str,
    message: String,
) -> Result<(), RosenError> {
    let client = requests(server.endpoint()).await?;
    verify_workspace(root, change)?;
    let report = client.snapshot_workspace(root, Some(message)).await?;
    if !report.unreadable.is_empty() {
        return Err(RosenError::ThreadChange(format!(
            "Some files were held open and the snapshot kept their last stored content: {}",
            report.unreadable.join(", ")
        )));
    }
    Ok(())
}

/// Sends local writes before asking Rosen to recompute the change on the latest base.
pub async fn rebase(server: &ProjectServer, root: &Path, change: &str) -> Result<(), RosenError> {
    let client = requests(server.endpoint()).await?;
    verify_workspace(root, change)?;
    let report = client.snapshot_workspace(root, None).await?;
    if !report.unreadable.is_empty() {
        return Err(RosenError::ThreadChange(format!(
            "Save the workspace files before rebasing: {}",
            report.unreadable.join(", ")
        )));
    }
    client.rebase_change(change).await?;
    Ok(())
}

fn verify_workspace(root: &Path, change: &str) -> Result<(), RosenError> {
    let marker =
        rosen_client::snapshot::WorkspaceMarker::read(root).map_err(rosen_client::Error::from)?;
    if marker.change.to_string() != change {
        return Err(RosenError::ThreadChange(
            "the workspace belongs to a different change".into(),
        ));
    }
    Ok(())
}
