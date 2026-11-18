//! Build culprits that need a fix forward (F5/F7, #406). R12 owns reversions;
//! Velocity reads its durable evidence and starts ordinary build threads.

use std::collections::BTreeMap;

use rosen_client::{Client, protocol::CheckKind};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;

use crate::{ProjectServer, RosenError, integration};

#[derive(Clone, Debug)]
pub struct BuildFailure {
    pub commit: String,
    pub change: String,
    pub output: String,
    pub reverted: bool,
    pub recorded_at: u64,
    pub refusal: Option<String>,
    pub blocking_commits: Vec<String>,
}

impl BuildFailure {
    pub fn new(commit: String, change: String, output: String, recorded_at: u64) -> Self {
        Self {
            commit,
            change,
            output,
            recorded_at,
            reverted: false,
            refusal: None,
            blocking_commits: Vec::new(),
        }
    }

    pub fn with_refusal(mut self, reason: String, blocking_commits: Vec<String>) -> Self {
        self.refusal = Some(reason);
        self.blocking_commits = blocking_commits;
        self
    }

    pub fn needs_repair(&self, now: u64) -> bool {
        !self.reverted
            && (self.refusal.is_some() || now.saturating_sub(self.recorded_at) >= 120_000)
    }
}

/// The protocol has no failure timestamps or refusal inventory. As with mirror status,
/// read those from the pinned server's metadata, without modifying its database.
pub async fn build_failures(server: &ProjectServer) -> Result<Vec<BuildFailure>, RosenError> {
    let client = Client::connect(server.endpoint()).await?;
    let failures = client.post_land_failures(None).await?;
    let (integrations, builds): (Vec<_>, Vec<_>) = failures
        .into_iter()
        .filter(|failure| {
            (failure.kind == CheckKind::Build && failure.requires_revert) || is_integration(failure)
        })
        .partition(is_integration);
    let mut repairs: Vec<_> = integrations
        .into_iter()
        .map(|failure| {
            BuildFailure::new(
                failure.commit.to_string(),
                failure.change,
                failure.output,
                0,
            )
            .with_refusal("Integration failures are fixed forward.".into(), Vec::new())
        })
        .collect();
    if builds.is_empty() {
        return Ok(repairs);
    }
    let reading = server.clone();
    let mut evidence = tokio::task::spawn_blocking(move || read_evidence(&reading))
        .await
        .map_err(|error| RosenError::BuildRepairData(error.to_string()))??;
    let legacy = builds
        .into_iter()
        .map(|failure| {
            let commit = failure.commit.to_string();
            let evidence = evidence.remove(&commit).ok_or_else(|| {
                RosenError::BuildRepairData(format!("No recorded time for build culprit {commit}"))
            })?;
            Ok(BuildFailure {
                commit,
                change: failure.change,
                output: failure.output,
                reverted: failure.revert.is_some(),
                recorded_at: evidence.recorded_at,
                refusal: evidence.refusal,
                blocking_commits: evidence.blocking_commits,
            })
        })
        .collect::<Result<Vec<_>, RosenError>>()?;
    repairs.extend(legacy);
    Ok(repairs)
}

fn is_integration(failure: &rosen_client::protocol::PostLandFailure) -> bool {
    failure.kind == CheckKind::Test
        && failure.revert.is_none()
        && failure.output.starts_with(integration::FAILURE_HEADING)
}

#[derive(Default)]
struct Evidence {
    recorded_at: u64,
    refusal: Option<String>,
    blocking_commits: Vec<String>,
}

#[derive(Deserialize)]
struct Detail {
    culprit: String,
    kind: Option<CheckKind>,
    reason: Option<String>,
    #[serde(default)]
    overlapping_commits: Vec<String>,
}

fn read_evidence(server: &ProjectServer) -> Result<BTreeMap<String, Evidence>, RosenError> {
    let connection = Connection::open_with_flags(
        server.data_dir().join("metadata.sqlite"),
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    connection.busy_timeout(std::time::Duration::from_secs(2))?;
    let mut query = connection.prepare(
        "SELECT kind, detail, created_at FROM events
         WHERE kind IN ('post_land_failure', 'revert_refused') ORDER BY seq",
    )?;
    let rows = query.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, u64>(2)?,
        ))
    })?;
    let mut evidence: BTreeMap<String, Evidence> = BTreeMap::new();
    for row in rows {
        let (kind, detail, created_at) = row?;
        let detail: Detail = serde_json::from_str(&detail)
            .map_err(|error| RosenError::BuildRepairData(error.to_string()))?;
        if kind == "post_land_failure" && detail.kind != Some(CheckKind::Build) {
            continue;
        }
        let entry = evidence.entry(detail.culprit).or_default();
        if kind == "post_land_failure" {
            if entry.recorded_at == 0 {
                entry.recorded_at = created_at;
            }
        } else {
            entry.refusal = Some(
                detail
                    .reason
                    .unwrap_or_else(|| "Rosen refused the revert".into()),
            );
            entry.blocking_commits = detail.overlapping_commits;
        }
    }
    Ok(evidence)
}
