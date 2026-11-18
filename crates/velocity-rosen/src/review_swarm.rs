//! Review coverage and claims after landing (F7, #382). None of these calls participates
//! in the landing gate. A claim names a review, never a source path needed by builders.

use std::path::Path;

use rosen_client::protocol::{LogRequest, MarkReviewed};
use rosen_client::{ChangeState, ChannelMembership};
use rosen_core::ReviewedState;

use crate::{ProjectServer, RosenError, change_details, landing, workspace};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct LandedChange {
    pub change: String,
    pub commit: String,
    pub sensitive: bool,
    pub landed_at: u64,
    pub summary: String,
}

pub fn review_piece(change: &str, commit: &str) -> String {
    format!("review:{change}:{commit}")
}

pub enum IssueCloser {
    Reviewer(String),
    Watcher,
}

pub struct IssueReviews {
    claims: std::collections::HashSet<String>,
    lands: std::collections::HashMap<(String, String), IssueReview>,
}

struct IssueReview {
    state: ReviewedState,
    reviewer: Option<String>,
}

impl IssueReviews {
    pub fn closer(&self, change: &str, commit: &str, watcher_due: bool) -> Option<IssueCloser> {
        let review = self.lands.get(&(change.into(), commit.into()))?;
        match review.state {
            ReviewedState::Reviewed => review.reviewer.clone().map(IssueCloser::Reviewer),
            ReviewedState::Unreviewed
                if watcher_due && !self.claims.contains(&review_piece(change, commit)) =>
            {
                Some(IssueCloser::Watcher)
            }
            _ => None,
        }
    }
}

pub async fn issue_reviews(server: &ProjectServer) -> Result<IssueReviews, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    // A worker records its verdict before giving up its claim. Read in this order so
    // a worker completing between the two reads cannot look like an uncovered land.
    let claims = client
        .pieces()
        .await?
        .into_iter()
        .map(|claim| claim.piece)
        .collect();
    let lands = client
        .review_landed(None)
        .await?
        .into_iter()
        .map(|land| {
            (
                (land.change.to_string(), land.commit.to_string()),
                IssueReview {
                    state: land.state,
                    reviewer: land.by.map(|by| by.to_string()),
                },
            )
        })
        .collect();
    Ok(IssueReviews { claims, lands })
}

/// Rosen returns oldest first; stable partitioning puts sensitive work first without
/// replacing age with a local discovery timestamp after an app restart.
pub async fn unreviewed(server: &ProjectServer) -> Result<Vec<LandedChange>, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
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

pub async fn landed_diff_record(
    server: &ProjectServer,
    target: &LandedChange,
) -> Result<rosen_client::protocol::Diff, RosenError> {
    landed_commit_diff(server, &target.commit).await
}

async fn landed_commit_diff(
    server: &ProjectServer,
    commit: &str,
) -> Result<rosen_client::protocol::Diff, RosenError> {
    use rosen_client::protocol::DiffRequest;
    let client = crate::connection::requests(server.endpoint()).await?;
    let log = client
        .log(LogRequest {
            at: Some(commit.to_owned()),
            limit: Some(1),
            ..LogRequest::default()
        })
        .await?;
    let parent = log
        .entries
        .first()
        .and_then(|entry| entry.parents.first())
        .ok_or_else(|| RosenError::InvalidId(commit.to_owned()))?;
    Ok(client
        .diff(DiffRequest {
            from: Some(parent.to_string()),
            to: Some(commit.to_owned()),
            context: Some(0),
            ..DiffRequest::default()
        })
        .await?)
}

pub async fn landed_paths(
    server: &ProjectServer,
    target: &LandedChange,
) -> Result<Vec<String>, RosenError> {
    landed_commit_paths(server, &target.commit).await
}

pub async fn landed_commit_paths(
    server: &ProjectServer,
    commit: &str,
) -> Result<Vec<String>, RosenError> {
    Ok(landed_commit_diff(server, commit)
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
    let client = crate::connection::requests(server.endpoint()).await?;
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
    let record = crate::connection::requests(server.endpoint())
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
        return Ok(change_details::changed_paths(server, change)
            .await?
            .is_empty());
    }
    session.snapshot(root, None).await?;
    let record = session.client().change_show(change).await?.change;
    if expected_head.is_some_and(|expected| record.head.to_string() != expected) {
        return Ok(false);
    }
    if record.state == ChangeState::Ready {
        return Ok(matches!(
            landing::land(server, "velocity-checker", change).await?,
            landing::LandOutcome::Landed { .. }
        ));
    }
    if change_details::changed_paths(server, change)
        .await?
        .is_empty()
    {
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
    use rosen_client::run_queue::{RunClass, RunQueue, held};
    if held().is_some() {
        return Ok(check());
    }
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

#[cfg(test)]
mod issue_closure_tests {
    use super::*;

    // #441 / #416: a green build closed an issue before anyone verified its acceptance criteria.
    #[test]
    fn watcher_waits_for_review_and_respects_objections_to_this_land() {
        let land = ("change".into(), "first-land".into());
        let evidence = |state, claims| IssueReviews {
            claims,
            lands: [(
                land.clone(),
                IssueReview {
                    state,
                    reviewer: Some("scout-1".into()),
                },
            )]
            .into_iter()
            .collect(),
        };
        let unclaimed = evidence(ReviewedState::Unreviewed, Default::default());
        assert!(unclaimed.closer("change", "first-land", false).is_none());
        assert!(matches!(
            unclaimed.closer("change", "first-land", true),
            Some(IssueCloser::Watcher)
        ));
        let active = evidence(
            ReviewedState::Unreviewed,
            [review_piece("change", "first-land")].into_iter().collect(),
        );
        assert!(active.closer("change", "first-land", true).is_none());
        let objected = evidence(ReviewedState::IssuesFiled, Default::default());
        assert!(objected.closer("change", "first-land", true).is_none());
        let reviewed = evidence(ReviewedState::Reviewed, Default::default());
        assert!(
            matches!(reviewed.closer("change", "first-land", false), Some(IssueCloser::Reviewer(by)) if by == "scout-1")
        );
        assert!(reviewed.closer("change", "later-land", true).is_none());
    }
}
