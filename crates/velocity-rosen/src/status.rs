use std::collections::BTreeMap;
use std::time::Duration;

use rosen_client::protocol::{ChangeRecord, DiffRequest, EventKind, OverlapQuery};
use rosen_core::{ChangeParent, ChangeState, ObjectId, SnapshotKind};
use rosen_core::{ProjectConfig, RecheckMode, ReviewMode};
use serde::Serialize;

use crate::{ProjectServer, RosenError};

mod local;
mod metadata;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct Status {
    pub server: Option<String>,
    pub machines: Vec<String>,
    pub base: BaseStatus,
    pub landing: LandingStatus,
    pub mirror: MirrorStatus,
    pub local: LocalStatus,
    pub changes: Vec<ChangeRow>,
    pub claims: Vec<PathActivity>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct BaseStatus {
    pub tip: Option<String>,
    pub green: Option<String>,
    pub bisecting: bool,
    pub culprit: Option<String>,
    pub revert: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct LandingStatus {
    pub lands_last_hour: u32,
    pub median_ms: Option<u32>,
    pub builds_pending: u32,
    pub tests_pending: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct MirrorStatus {
    pub collapse: bool,
    pub remote: Option<String>,
    pub pushed: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct LocalStatus {
    pub holders: Vec<String>,
    pub waiting: u32,
    pub ready: u32,
    pub target: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct PathActivity {
    pub change: String,
    pub owner: String,
    pub kind: String,
    pub paths: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Editing,
    Named,
    Landing,
    Landed,
    Green,
    Reverted,
    Abandoned,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct ChangeRow {
    pub id: String,
    pub owner: String,
    pub phase: Phase,
    pub parent: Option<String>,
    pub added: Option<u32>,
    pub removed: Option<u32>,
    pub overlaps: Vec<PathActivity>,
    pub conflicts: Vec<String>,
    pub landed: Option<String>,
    pub output: Option<String>,
}

#[derive(Default)]
struct DiffCache(BTreeMap<String, (ObjectId, ChangeParent, u32, u32)>);

/// The two connections keep a burst on the subscription from blocking command replies.
pub struct StatusFeed {
    server: ProjectServer,
    client: crate::connection::Connection,
    stream: crate::EventStream,
    notifications: local::Notifications,
    cache: DiffCache,
    live: Vec<(String, String)>,
    conflicts: BTreeMap<String, Vec<String>>,
    config: ProjectConfig,
}

impl StatusFeed {
    pub async fn open(server: ProjectServer) -> Result<Self, RosenError> {
        let client = crate::connection::Connection::dedicated(server.endpoint()).await?;
        let cursor = client.status().await?.last_event;
        let stream = crate::EventStream::open(&server, cursor).await?;
        let notifications = local::Notifications::watch(&server);
        let config = client.base_config().await.unwrap_or_default();
        Ok(Self {
            server,
            client,
            stream,
            notifications,
            cache: DiffCache::default(),
            live: Vec::new(),
            conflicts: BTreeMap::new(),
            config,
        })
    }

    /// Waits for a change, an auxiliary notification or an expiry. The result
    /// says whether to refresh rows as well as the status summary.
    pub async fn next(&mut self, expires: Duration) -> Option<bool> {
        tokio::select! {
            biased;
            event = self.stream.next() => {
                let mut changes = changes_rows(event?.kind);
                // One read reflects all of the queued events. Repeatedly
                // re-reading a burst can put later lifecycle events behind it.
                for _ in 0..128 {
                    let Some(event) = self.stream.queued() else {
                        break;
                    };
                    changes |= changes_rows(event.kind);
                }
                Some(changes)
            }
            () = self.notifications.next() => Some(false),
            () = tokio::time::sleep(expires) => Some(false),
        }
    }

    pub async fn read(
        &mut self,
        status: &mut Status,
        changes: bool,
        target: u32,
    ) -> Result<Duration, RosenError> {
        let server = self.server.clone();
        let local = tokio::task::spawn_blocking(move || {
            let metadata = metadata::read(&server)?;
            Ok::<_, RosenError>((metadata, local::read(&server)))
        });
        if status.server.is_none() {
            status.server = Some(self.client.status().await?.server);
        }
        let (metadata, local) = local
            .await
            .map_err(|error| RosenError::StatusData(error.to_string()))??;
        if status.base.tip != metadata.base.tip
            && let Ok(config) = self.client.base_config().await
        {
            self.config = config;
        }
        // The pinned daemon sends no R9 event when it creates a change or marks
        // it ready. Its WAL notification covers those live-state transitions.
        if changes || self.live != metadata.live || self.conflicts != metadata.conflicts {
            self.read_changes(status, &metadata).await?;
        }
        self.live = metadata.live;
        self.conflicts = metadata.conflicts;
        status.base = metadata.base;
        status.landing = metadata.landing;
        if self.config.review != ReviewMode::AfterLand && self.config.recheck != RecheckMode::Merge
        {
            status.landing.builds_pending = 0;
        }
        status.mirror = metadata.mirror;
        status.machines = metadata.machines;
        status.local = local;
        status.local.target = self.config.warm_pool.size.max(target);
        Ok(metadata.expiry)
    }

    async fn read_changes(
        &mut self,
        status: &mut Status,
        metadata: &metadata::Metadata,
    ) -> Result<(), RosenError> {
        let (mut records, claims) = tokio::try_join!(
            self.client.change_list(Default::default()),
            self.client.claim_list(Default::default()),
        )?;
        records.sort_by_key(|record| {
            (
                record.change.state.is_closed(),
                std::cmp::Reverse(record.updated_at),
            )
        });
        let mut closed = 0;
        records.retain(|record| {
            if !record.change.state.is_closed() {
                return true;
            }
            closed += 1;
            closed <= 30
        });
        let mut rows = Vec::with_capacity(records.len());
        for record in &records {
            rows.push(self.row(record, metadata).await?);
        }
        self.cache
            .0
            .retain(|id, _| rows.iter().any(|row| row.id == *id));
        status.changes = rows;
        status.claims = claims
            .into_iter()
            .map(|claim| PathActivity {
                change: claim.change.to_string(),
                owner: claim.owner.to_string(),
                kind: format!("{:?}", claim.kind).to_lowercase(),
                paths: vec![claim.path],
            })
            .collect();
        Ok(())
    }

    async fn row(
        &mut self,
        record: &ChangeRecord,
        metadata: &metadata::Metadata,
    ) -> Result<ChangeRow, RosenError> {
        let change = &record.change;
        let id = change.id.to_string();
        let history = metadata.changes.get(&id);
        let (landed, output) = history.map_or((None, None), |entry| {
            (entry.landed.clone(), entry.output.clone())
        });
        let phase = phase(record, history);
        let (counts, overlaps) = if change.state.is_closed() {
            (self.cache.counts(&id), Vec::new())
        } else {
            let query = OverlapQuery {
                change: Some(id.clone()),
                ..Default::default()
            };
            let answer = self.client.overlaps(query).await?;
            let overlaps = answer
                .overlaps
                .into_iter()
                .map(|overlap| PathActivity {
                    change: overlap.change.to_string(),
                    owner: overlap.owner.to_string(),
                    kind: "overlap".into(),
                    paths: overlap.paths,
                })
                .collect();
            (self.line_counts(record).await, overlaps)
        };
        Ok(ChangeRow {
            parent: match change.parent {
                ChangeParent::Base(_) => None,
                ChangeParent::Change(parent) => Some(parent.to_string()),
            },
            conflicts: metadata.conflicts.get(&id).cloned().unwrap_or_default(),
            id,
            owner: change.owner.to_string(),
            phase,
            added: counts.map(|(added, _)| added),
            removed: counts.map(|(_, removed)| removed),
            overlaps,
            landed,
            output,
        })
    }

    async fn line_counts(&mut self, record: &ChangeRecord) -> Option<(u32, u32)> {
        use rosen_client::protocol::LineKind;
        let change = &record.change;
        let id = change.id.to_string();
        if let Some(&(head, parent, added, removed)) = self.cache.0.get(&id)
            && head == change.head
            && parent == change.parent
        {
            return Some((added, removed));
        }
        let request = DiffRequest {
            to: Some(id.clone()),
            context: Some(0),
            ..Default::default()
        };
        let diff = match self.client.diff(request).await {
            Ok(diff) => diff,
            Err(error) => {
                eprintln!("Rosen status diff for {id}: {error}");
                return None;
            }
        };
        let mut added = 0u32;
        let mut removed = 0u32;
        for line in diff
            .files
            .into_iter()
            .flat_map(|file| file.hunks)
            .flat_map(|hunk| hunk.lines)
        {
            match line.kind {
                LineKind::Added => added = added.saturating_add(1),
                LineKind::Removed => removed = removed.saturating_add(1),
                LineKind::Context => {}
            }
        }
        self.cache
            .0
            .insert(id, (change.head, change.parent, added, removed));
        Some((added, removed))
    }
}

impl DiffCache {
    fn counts(&self, id: &str) -> Option<(u32, u32)> {
        self.0
            .get(id)
            .map(|&(_, _, added, removed)| (added, removed))
    }
}

fn phase(record: &ChangeRecord, history: Option<&metadata::ChangeHistory>) -> Phase {
    match record.change.state {
        ChangeState::Abandoned => Phase::Abandoned,
        ChangeState::Landed if history.is_some_and(|entry| entry.green) => Phase::Green,
        ChangeState::Landed => Phase::Landed,
        ChangeState::Ready => Phase::Landing,
        ChangeState::Open if history.is_some_and(|entry| entry.reverted) => Phase::Reverted,
        ChangeState::Open
            if record
                .change
                .snapshots
                .iter()
                .any(|snapshot| matches!(snapshot.kind, SnapshotKind::Named(_))) =>
        {
            Phase::Named
        }
        ChangeState::Open => Phase::Editing,
    }
}

fn changes_rows(kind: EventKind) -> bool {
    matches!(
        kind,
        EventKind::Snapshot
            | EventKind::Landed
            | EventKind::Rebased
            | EventKind::Conflict
            | EventKind::Updated
            | EventKind::Claim
            | EventKind::Released
            | EventKind::Reverted
            | EventKind::Green
            | EventKind::PostLandFailure
    )
}
