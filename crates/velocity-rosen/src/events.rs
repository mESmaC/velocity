//! A project's Rosen event stream, which follows each thread's change through review and
//! landing in place of the GitHub PR watcher (Rosen R9, Velocity F6).

use std::collections::BTreeMap;
use std::path::Path;
use std::time::UNIX_EPOCH;

use rosen_client::Client;
use rosen_client::protocol::{Event, EventKind};
use rusqlite::{Connection, OpenFlags};
use tokio::sync::mpsc;

use crate::{ProjectServer, RosenError};

/// A position is meaningful only in the metadata database that issued it.
pub struct EventHead {
    pub identity: String,
    pub latest: u64,
}

impl EventHead {
    pub async fn read(client: &Client) -> Result<Self, RosenError> {
        let status = client.status().await?;
        let database = Path::new(&status.data_dir).join("metadata.sqlite");
        let created = std::fs::metadata(&database)
            .and_then(|metadata| metadata.created())
            .map_err(|source| RosenError::Io {
                action: "identify the Rosen event database at",
                path: database.clone(),
                source,
            })?
            .duration_since(UNIX_EPOCH)
            .map_err(|error| RosenError::StatusData(error.to_string()))?;
        Ok(Self {
            identity: format!("{}:{}", database.display(), created.as_nanos()),
            latest: status.last_event,
        })
    }
}

/// During a rewind, reconstruct the latest lifecycle without rerunning superseded
/// lands, reverts or names. Keep a revert before a new name so it can restore work.
#[derive(Default)]
pub struct EventReplay {
    through: u64,
    changes: BTreeMap<String, ReplayChange>,
}

#[derive(Default)]
struct ReplayChange {
    terminal: u64,
    named: u64,
}

impl EventReplay {
    fn read(data_dir: &Path, through: u64) -> Result<Self, RosenError> {
        let mut replay = Self {
            through,
            ..Self::default()
        };
        if through == 0 {
            return Ok(replay);
        }
        let connection = Connection::open_with_flags(
            data_dir.join("metadata.sqlite"),
            OpenFlags::SQLITE_OPEN_READ_ONLY,
        )?;
        connection.busy_timeout(std::time::Duration::from_secs(2))?;
        let mut query = connection.prepare(
            "SELECT change_id, kind, MAX(seq) FROM events
             WHERE seq <= ?1 AND change_id IS NOT NULL AND
              (kind IN ('landed', 'reverted')
               OR (kind = 'updated' AND json_extract(detail, '$.state') = 'abandoned')
               OR (kind = 'snapshot' AND json_extract(detail, '$.named') = 1))
             GROUP BY change_id, kind",
        )?;
        let events = query.query_map([through], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, u64>(2)?,
            ))
        })?;
        for event in events {
            let (change, kind, sequence) = event?;
            let replay = replay.changes.entry(change).or_default();
            if kind == "snapshot" {
                replay.named = sequence;
            } else {
                replay.terminal = replay.terminal.max(sequence);
            }
        }
        Ok(replay)
    }

    pub fn boundary(&self) -> u64 {
        self.through
    }

    pub fn superseded(&self, event: &Event) -> bool {
        if event.seq > self.through {
            return false;
        }
        let Some((change, meaning)) = change_event(event) else {
            return false;
        };
        self.changes.get(change).is_some_and(|latest| {
            event.seq < latest.terminal
                || (meaning == ChangeEvent::Named && event.seq < latest.named)
        })
    }
}

/// Evidence used by the issue follower, separate from thread and checker state.
pub enum IssueEvent<'a> {
    Landed(&'a str),
    /// Independent build-prefix evidence; never decoded from a Green notification.
    BuildPassed(&'a str),
    Reverted {
        commit: Option<&'a str>,
        reason: &'a str,
    },
}

/// R12's green notifications certify tests, not builds. Build evidence comes only from
/// [`crate::issue_build_coverage`], which reads the separate durable build prefix.
pub fn issue_event(event: &Event) -> Option<(&str, IssueEvent<'_>)> {
    let change = event.change.as_deref()?;
    let commit = || event.detail.get("commit")?.as_str();
    let meaning = match event.kind {
        EventKind::Landed => IssueEvent::Landed(commit()?),
        EventKind::Reverted => IssueEvent::Reverted {
            commit: event.detail.get("culprit").and_then(|value| value.as_str()),
            reason: event
                .detail
                .get("output")
                .and_then(|value| value.as_str())
                .filter(|output| !output.trim().is_empty())
                .or_else(|| event.detail.get("reason").and_then(|value| value.as_str()))
                .unwrap_or("Rosen reverted the change after its build failed."),
        },
        _ => return None,
    };
    Some((change, meaning))
}

/// What happened to a change, as far as its thread is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeEvent {
    /// A named snapshot (`rosen commit -m`): the agent says the work is ready.
    Named,
    Landed,
    Abandoned,
    /// Post-land verification reverted the change and reopened it (R12).
    Reverted,
    /// The server rebased the change onto a newer base (R2): its workspace takes the new
    /// head at the agent's next step boundary.
    Rebased,
    /// The rebased head holds conflicts; [`conflict_paths`] names their files (R5).
    /// Apply the head at a turn boundary before asking the agent to resolve them.
    Conflict,
}

/// The change an event names and what it means for its thread; `None` for events a
/// thread doesn't move on, such as the watcher's automatic snapshots.
pub fn change_event(event: &Event) -> Option<(&str, ChangeEvent)> {
    let change = event.change.as_deref()?;
    let meaning = match event.kind {
        EventKind::Snapshot
            if event.detail.get("named").and_then(|named| named.as_bool()) == Some(true) =>
        {
            ChangeEvent::Named
        }
        EventKind::Landed => ChangeEvent::Landed,
        EventKind::Updated
            if event.detail.get("state").and_then(|value| value.as_str()) == Some("abandoned") =>
        {
            ChangeEvent::Abandoned
        }
        EventKind::Reverted => ChangeEvent::Reverted,
        EventKind::Rebased => ChangeEvent::Rebased,
        EventKind::Conflict => ChangeEvent::Conflict,
        _ => return None,
    };
    Some((change, meaning))
}

/// Server-assigned UTC millisecond timestamps (`events.created_at`) for events after
/// `after`, plus the latest timestamp at or before `after` as a monotonic floor (#540).
/// Returns `(None, empty)` when the server's metadata database is not on this machine
/// (for example when talking through a remote forwarding daemon).
pub fn event_timestamps(
    data_dir: &Path,
    after: u64,
) -> Result<(Option<i64>, BTreeMap<u64, i64>), RosenError> {
    let database = data_dir.join("metadata.sqlite");
    if !database.is_file() {
        return Ok((None, BTreeMap::new()));
    }
    let connection = Connection::open_with_flags(database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(std::time::Duration::from_secs(2))?;
    let from = i64::try_from(after).unwrap_or(i64::MAX);
    let floor: Option<i64> = connection.query_row(
        "SELECT MAX(created_at) FROM events WHERE seq <= ?1",
        [from],
        |row| row.get(0),
    )?;
    let mut query =
        connection.prepare("SELECT seq, created_at FROM events WHERE seq > ?1 ORDER BY seq")?;
    let rows = query.query_map([from], |row| {
        Ok((row.get::<_, u64>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut timestamps = BTreeMap::new();
    for row in rows {
        let (sequence, created_at) = row?;
        timestamps.insert(sequence, created_at);
    }
    Ok((floor, timestamps))
}

/// The files a `conflict` event names (R5).
pub fn conflict_paths(event: &Event) -> Vec<String> {
    event_paths(event, "paths")
}

/// Generated files whose base copies must be regenerated after a rebase (R5).
pub fn regenerate_paths(event: &Event) -> Vec<String> {
    event_paths(event, "regenerate")
}

fn event_paths(event: &Event, key: &str) -> Vec<String> {
    event
        .detail
        .get(key)
        .and_then(|paths| paths.as_array())
        .map(|paths| {
            paths
                .iter()
                .filter_map(|path| path.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// What the failing post-land check printed, as a `reverted` event carries it (R12).
pub fn revert_output(event: &Event) -> Option<String> {
    event
        .detail
        .get("output")
        .and_then(|output| output.as_str())
        .map(str::to_owned)
}

/// Every event of one project's server after a cursor: the project view, which stores no
/// cursor of its own, so Velocity keeps it.
pub struct EventStream {
    // The connection the stream arrives on; it ends when this is dropped.
    _client: crate::connection::Connection,
    events: mpsc::Receiver<Event>,
}

impl EventStream {
    /// Subscribes to the server's events after `after`. Connects to a running server only:
    /// the caller starts it.
    pub async fn open(server: &ProjectServer, after: u64) -> Result<Self, RosenError> {
        let client = crate::connection::Connection::dedicated(server.endpoint()).await?;
        Self::subscribe(client, after).await
    }

    /// Validate and open against the same connection so a replaced server cannot
    /// receive the cursor from a connection to its predecessor.
    pub async fn open_checked(
        server: &ProjectServer,
        position: impl FnOnce(&EventHead) -> Result<(u64, u64), String>,
    ) -> Result<(Self, EventReplay), String> {
        let client = crate::connection::Connection::dedicated(server.endpoint())
            .await
            .map_err(|error| error.to_string())?;
        let head = EventHead::read(&client)
            .await
            .map_err(|error| error.to_string())?;
        let (after, replay_through) = position(&head)?;
        let directory = server.data_dir().to_owned();
        let replay =
            tokio::task::spawn_blocking(move || EventReplay::read(&directory, replay_through))
                .await
                .map_err(|error| error.to_string())?
                .map_err(|error| error.to_string())?;
        let stream = Self::subscribe(client, after)
            .await
            .map_err(|error| error.to_string())?;
        Ok((stream, replay))
    }

    async fn subscribe(
        client: crate::connection::Connection,
        after: u64,
    ) -> Result<Self, RosenError> {
        let events = client.subscribe(Some(after)).await?;
        Ok(Self {
            _client: client,
            events,
        })
    }

    /// The next event; `None` once the connection has closed.
    pub async fn next(&mut self) -> Option<Event> {
        self.events.recv().await
    }

    pub(crate) fn queued(&mut self) -> Option<Event> {
        self.events.try_recv().ok()
    }
}
