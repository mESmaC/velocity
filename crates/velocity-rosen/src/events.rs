//! A project's Rosen event stream, which follows each thread's change through review and
//! landing in place of the GitHub PR watcher (Rosen R9, Velocity F6).

use rosen_client::Client;
use rosen_client::protocol::{Event, EventKind};
use tokio::sync::mpsc;

use crate::{ProjectServer, RosenError};

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
    _client: Client,
    events: mpsc::Receiver<Event>,
}

impl EventStream {
    /// Subscribes to the server's events after `after`. Connects to a running server only:
    /// the caller starts it.
    pub async fn open(server: &ProjectServer, after: u64) -> Result<Self, RosenError> {
        let client = Client::connect(server.endpoint()).await?;
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
}
