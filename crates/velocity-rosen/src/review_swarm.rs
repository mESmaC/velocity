//! Review coverage and claims after landing (F7, #382). None of these calls participates
//! in the landing gate. A claim names a review, never a source path needed by builders.

use std::path::Path;

use rosen_client::protocol::{LogRequest, MarkReviewed};
use rosen_client::{ChangeState, ChannelMembership, Client};
use rosen_core::ReviewedState;

use crate::{ProjectServer, RosenError, duty, workspace};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct LandedChange {
    pub change: String,
    pub commit: String,
    pub sensitive: bool,
    pub landed_at: u64,
    pub summary: String,
}

/// Rosen returns oldest first; stable partitioning puts sensitive work first without
/// replacing age with a local discovery timestamp after an app restart.
pub async fn unreviewed(server: &ProjectServer) -> Result<Vec<LandedChange>, RosenError> {
    let client = Client::connect(server.endpoint()).await?;
    let records = client
        .review_landed(Some(ReviewedState::Unreviewed))
        .await?;
    let mut changes = Vec::new();
    for record in records {
        let log = client
            .log(LogRequest {
                at: Some(record.commit.to_string()),
                limit: Some(1),
                ..LogRequest::default()
            })
            .await?;
        let Some(commit) = log.entries.first() else {
            continue;
        };
        changes.push(LandedChange {
            change: record.change.to_string(),
            commit: record.commit.to_string(),
            sensitive: record.tier == "sensitive",
            landed_at: u64::try_from(commit.time).unwrap_or_default(),
            summary: commit.summary.clone(),
        });
    }
    changes.sort_by_key(|change| (!change.sensitive, change.landed_at));
    Ok(changes)
}

async fn landed_diff_record(
    server: &ProjectServer,
    target: &LandedChange,
) -> Result<rosen_client::protocol::Diff, RosenError> {
    use rosen_client::protocol::DiffRequest;
    let client = Client::connect(server.endpoint()).await?;
    let log = client
        .log(LogRequest {
            at: Some(target.commit.clone()),
            limit: Some(1),
            ..LogRequest::default()
        })
        .await?;
    let parent = log
        .entries
        .first()
        .and_then(|entry| entry.parents.first())
        .ok_or_else(|| RosenError::InvalidId(target.commit.clone()))?;
    Ok(client
        .diff(DiffRequest {
            from: Some(parent.to_string()),
            to: Some(target.commit.clone()),
            ..DiffRequest::default()
        })
        .await?)
}

pub async fn landed_diff(
    server: &ProjectServer,
    target: &LandedChange,
) -> Result<String, RosenError> {
    Ok(duty::unified(&landed_diff_record(server, target).await?))
}

pub async fn landed_paths(
    server: &ProjectServer,
    target: &LandedChange,
) -> Result<Vec<String>, RosenError> {
    Ok(landed_diff_record(server, target)
        .await?
        .files
        .into_iter()
        .map(|file| file.path)
        .collect())
}

/// A stable member identity lets a restarted worker recover its own claim. Other agents
/// see the same claim through their AIRC tools (Rosen R13).
pub async fn membership(
    server: &ProjectServer,
    nickname: &str,
) -> Result<ChannelMembership, RosenError> {
    Ok(workspace::session(server, nickname)
        .await?
        .join_channel(format!("duty-{nickname}"), "velocity")
        .await?)
}

pub async fn claim(
    member: &ChannelMembership,
    piece: &str,
    description: &str,
) -> Result<bool, RosenError> {
    use rosen_client::protocol::channel::PieceOutcome;
    Ok(matches!(
        member
            .claim_piece(Some(piece), description, "review-swarm")
            .await?,
        PieceOutcome::Granted { .. }
    ))
}

/// Mark only the exact land reviewed. R12 can reopen and reland the same change while
/// the reviewer works; that later incarnation must remain unreviewed.
pub async fn mark(
    server: &ProjectServer,
    nickname: &str,
    target: &LandedChange,
    issues_filed: bool,
) -> Result<bool, RosenError> {
    let client = Client::connect(server.endpoint()).await?;
    let records = client.review_landed(None).await?;
    if !records.iter().any(|record| {
        record.change.to_string() == target.change && record.commit.to_string() == target.commit
    }) {
        return Ok(false);
    }
    let state = if issues_filed {
        ReviewedState::IssuesFiled
    } else {
        ReviewedState::Reviewed
    };
    let marked = client
        .review_mark(MarkReviewed {
            change: target.change.clone(),
            state,
            by: rosen_client::identity::parse_agent(nickname)
                .map_err(|_| RosenError::AgentName(nickname.into()))?,
        })
        .await?;
    // ReviewMark names a change rather than a commit. If a reland raced the query,
    // undo only our mark, so it cannot count as coverage of code we never reviewed.
    if marked.commit.to_string() != target.commit {
        client
            .review_mark(MarkReviewed {
                change: target.change.clone(),
                state: ReviewedState::Unreviewed,
                by: marked
                    .by
                    .ok_or_else(|| RosenError::AgentName(nickname.into()))?,
            })
            .await?;
        return Ok(false);
    }
    Ok(true)
}

/// Reuses an interrupted worker's workspace without discarding its proof or fix. A
/// workspace owned by anyone else is never adopted.
pub async fn open_workspace(
    server: &ProjectServer,
    nickname: &str,
    root: &Path,
) -> Result<crate::AgentWorkspace, RosenError> {
    let marker = rosen_client::snapshot::WorkspaceMarker::path(root);
    if !marker.is_file() {
        return crate::create_workspace(server, nickname, root, None).await;
    }
    let marker =
        rosen_client::snapshot::WorkspaceMarker::read(root).map_err(|source| RosenError::Io {
            action: "read",
            path: root.into(),
            source,
        })?;
    let change = marker.change.to_string();
    let record = Client::connect(server.endpoint())
        .await?
        .change_show(&change)
        .await?
        .change;
    if record.owner.as_str() != nickname {
        return Err(RosenError::AgentName(format!(
            "workspace belongs to {}",
            record.owner
        )));
    }
    Ok(crate::AgentWorkspace::new(
        change,
        root.into(),
        "recovered",
        false,
    ))
}

/// Named proof and fix-forward work lands through Rosen's ordinary after-land policy.
/// An unfinished or conflicted change is kept for its worker, including after restart.
pub async fn finish_workspace(
    server: &ProjectServer,
    nickname: &str,
    change: &str,
    root: &Path,
    expected_head: Option<&str>,
) -> Result<bool, RosenError> {
    let session = workspace::session(server, nickname).await?;
    let record = session.client().change_show(change).await?.change;
    if record.state == ChangeState::Landed {
        return Ok(expected_head.is_none_or(|expected| record.head.to_string() == expected));
    }
    if record.state == ChangeState::Abandoned {
        return Ok(duty::changed_paths(server, change).await?.is_empty());
    }
    session.snapshot(root, None).await?;
    let record = session.client().change_show(change).await?.change;
    if expected_head.is_some_and(|expected| record.head.to_string() != expected) {
        return Ok(false);
    }
    if record.state == ChangeState::Ready {
        return Ok(matches!(
            duty::land(server, "velocity-checker", change).await?,
            duty::LandOutcome::Landed { .. }
        ));
    }
    if duty::changed_paths(server, change).await?.is_empty() {
        session.abandon(change).await?;
        return Ok(true);
    }
    Ok(false)
}

pub async fn snapshot_head(
    server: &ProjectServer,
    nickname: &str,
    change: &str,
    root: &Path,
) -> Result<String, RosenError> {
    let session = workspace::session(server, nickname).await?;
    session.snapshot(root, None).await?;
    Ok(session
        .client()
        .change_show(change)
        .await?
        .change
        .head
        .to_string())
}

/// Proof verification yields to every build in the machine queue. The closure runs only
/// after a verify turn is acquired; there is no receipt or landing dependency.
pub fn verify<T>(folder: &Path, check: impl FnOnce() -> T) -> Result<T, RosenError> {
    use rosen_client::run_queue::{RunClass, RunQueue};
    let source_error = |source| RosenError::Io {
        action: "queue proof verification for",
        path: folder.into(),
        source,
    };
    let queue = RunQueue::machine().map_err(source_error)?;
    let _turn = queue
        .turn(
            RunClass::Verify,
            &format!("review swarm in {}", folder.display()),
        )
        .map_err(source_error)?;
    Ok(check())
}
