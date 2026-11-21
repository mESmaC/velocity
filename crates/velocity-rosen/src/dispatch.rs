//! Confirm a turn ended without work before continuous dispatch frees its slot.
use crate::{ProjectServer, RosenError};
use rosen_client::{ChangeState, protocol::DiffRequest};

/// Whether `change` is still open on Rosen with no named snapshot (#544).
/// When `root` is supplied, snapshots final edits before inspecting the change.
pub async fn is_unnamed_open_change(
    server: &ProjectServer,
    change: &str,
    root: Option<&std::path::Path>,
) -> Result<bool, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let record = client.change_show(change).await?.change;
    if record.state != ChangeState::Open {
        return Ok(false);
    }
    if let Some(root) = root {
        crate::workspace::session(server, &record.owner.to_string())
            .await?
            .snapshot(root, None)
            .await?;
    }
    let record = client.change_show(change).await?.change;
    Ok(record.state == ChangeState::Open
        && !record
            .snapshots
            .iter()
            .any(|snapshot| matches!(snapshot.kind, rosen_core::SnapshotKind::Named(_))))
}

/// Named work still belongs to the landing lifecycle, including a prose-only change.
/// Snapshot the final files first: the watcher may still be uploading the last step.
pub async fn ended_without_changes(
    server: &ProjectServer,
    change: &str,
    root: &std::path::Path,
) -> Result<bool, RosenError> {
    if !is_unnamed_open_change(server, change, Some(root)).await? {
        return Ok(false);
    }
    let client = crate::connection::requests(server.endpoint()).await?;
    let diff = client
        .diff(DiffRequest {
            to: Some(change.into()),
            context: Some(0),
            ..DiffRequest::default()
        })
        .await?;
    Ok(diff.files.is_empty())
}

/// Idempotent cleanup: a crash after abandoning but before saving its acknowledgement
/// must not leave a completed thread holding claims forever.
pub async fn abandon_finished(server: &ProjectServer, change: &str) -> Result<(), RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let record = client.change_show(change).await?.change;
    if !record.state.is_closed() {
        crate::abandon_change(server, &record.owner.to_string(), change).await?;
    }
    Ok(())
}

/// The server's authoritative state for an agent continuing in the same workspace.
pub async fn handoff_state(server: &ProjectServer, change: &str) -> Result<String, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let record = client.change_show(change).await?.change;
    let conflicts = record
        .conflicts
        .iter()
        .take(30)
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    Ok(format!(
        "Rosen change: {}\nState: {}\nStored head: {}\nParent: {:?}\nStored conflicts ({} total): {}",
        record.id,
        record.state,
        record.head,
        record.parent,
        record.conflicts.len(),
        if conflicts.is_empty() {
            "none".into()
        } else {
            conflicts.join(", ")
        },
    ))
}
