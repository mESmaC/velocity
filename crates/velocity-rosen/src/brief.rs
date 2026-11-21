//! Read-only Rosen snapshots used to assemble task briefs at dispatch and in `next_work` (#539).

use std::collections::BTreeMap;

use rosen_client::Endpoint;
use rosen_client::protocol::{DiffRequest, LogRequest, OverlapQuery};

use crate::{ProjectServer, RosenError};

const MAX_OPEN_CHANGES: usize = 20;
const MAX_FAILURES: usize = 10;
const MAX_SESSION_CLAIMS: usize = 6;
const MAX_SESSION_OVERLAPS: usize = 5;

/// Live Rosen facts for a thread's opening session card (#534).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionRosenState {
    pub base_tip: Option<String>,
    pub held_path_claims: Vec<String>,
    pub overlapping_changes: Vec<String>,
}

/// Fetches current base tip, path claims held by `change`, and active overlapping changes
/// from the Rosen server at `endpoint` (#534).
pub async fn session_state(endpoint: &str, change: &str) -> Result<SessionRosenState, RosenError> {
    let client = crate::connection::requests(&Endpoint::new(endpoint)).await?;
    let log_request = LogRequest {
        at: Some("base".to_owned()),
        limit: Some(1),
        ..LogRequest::default()
    };
    let overlap_query = OverlapQuery {
        change: Some(change.to_owned()),
        paths: Vec::new(),
    };
    let (log, claims, overlaps) = tokio::try_join!(
        client.log(log_request),
        client.claim_list(Default::default()),
        client.overlaps(overlap_query),
    )?;
    let base_tip = log.entries.first().map(|entry| entry.commit.to_string());
    let mut held_path_claims: Vec<String> = claims
        .into_iter()
        .filter(|claim| claim.change.to_string() == change)
        .map(|claim| claim.path)
        .collect();
    held_path_claims.sort();
    held_path_claims.dedup();
    held_path_claims.truncate(MAX_SESSION_CLAIMS);
    let overlapping_changes: Vec<String> = overlaps
        .overlaps
        .into_iter()
        .take(MAX_SESSION_OVERLAPS)
        .map(|overlap| {
            let id = overlap.change.to_string();
            let short = id.get(..8).unwrap_or(&id);
            let paths = overlap
                .paths
                .into_iter()
                .take(3)
                .collect::<Vec<_>>()
                .join(", ");
            if paths.is_empty() {
                format!("{short} ({})", overlap.owner)
            } else {
                format!("{short} ({}): {paths}", overlap.owner)
            }
        })
        .collect();
    Ok(SessionRosenState {
        base_tip,
        held_path_claims,
        overlapping_changes,
    })
}

/// Summary of an active Rosen change and the paths it touches or claims.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenChangeBrief {
    pub id: String,
    pub owner: String,
    pub state: String,
    pub edited_paths: Vec<String>,
    pub claimed_paths: Vec<String>,
}

/// Summary of a recent post-land failure recorded on the Rosen server.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostLandFailureBrief {
    pub commit: String,
    pub change: String,
    pub kind: String,
    pub reverted: bool,
    pub output: String,
}

/// Lists open and ready Rosen changes (excluding `exclude_change`) with their edited and
/// claimed paths. Bounded to keep dispatch fast on busy projects.
pub async fn open_changes(
    server: &ProjectServer,
    exclude_change: Option<&str>,
) -> Result<Vec<OpenChangeBrief>, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let (records, claims) = tokio::try_join!(
        client.change_list(Default::default()),
        client.claim_list(Default::default()),
    )?;
    let mut claims_by_change: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for claim in claims {
        claims_by_change
            .entry(claim.change.to_string())
            .or_default()
            .push(claim.path);
    }
    let mut open = Vec::new();
    for record in records
        .into_iter()
        .filter(|record| !record.change.state.is_closed())
    {
        let id = record.change.id.to_string();
        if exclude_change == Some(id.as_str()) {
            continue;
        }
        let edited_paths = match client
            .diff(DiffRequest {
                to: Some(id.clone()),
                context: Some(0),
                ..Default::default()
            })
            .await
        {
            Ok(diff) => diff.files.into_iter().map(|file| file.path).collect(),
            Err(_) => Vec::new(),
        };
        let mut claimed_paths = claims_by_change.remove(&id).unwrap_or_default();
        claimed_paths.sort();
        claimed_paths.dedup();
        open.push(OpenChangeBrief {
            id,
            owner: record.change.owner.to_string(),
            state: record.change.state.as_str().to_owned(),
            edited_paths,
            claimed_paths,
        });
        if open.len() >= MAX_OPEN_CHANGES {
            break;
        }
    }
    Ok(open)
}

/// Returns recent post-land failures from Rosen, newest last reversed to newest first.
pub async fn recent_post_land_failures(
    server: &ProjectServer,
) -> Result<Vec<PostLandFailureBrief>, RosenError> {
    let client = crate::connection::requests(server.endpoint()).await?;
    let failures = client.post_land_failures(None).await?;
    Ok(failures
        .into_iter()
        .rev()
        .take(MAX_FAILURES)
        .map(|failure| PostLandFailureBrief {
            commit: failure.commit.to_string(),
            change: failure.change,
            kind: format!("{:?}", failure.kind).to_lowercase(),
            reverted: failure.revert.is_some(),
            output: failure.output,
        })
        .collect())
}
