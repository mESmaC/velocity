//! A project's Rosen event stream, which follows each thread's change through review and
//! landing in place of the GitHub PR watcher (Rosen R9, Velocity F6).

use rosen_client::Client;
use rosen_client::protocol::{Event, EventKind};
use tokio::sync::mpsc;

use crate::{ProjectServer, RosenError};

/// What happened to a change, as far as its thread is concerned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeEvent {
    /// A named snapshot (`rosen commit -m`): the agent says the work is ready.
    Named,
    Landed,
    /// Post-land verification reverted the change and reopened it (R12).
    Reverted,
    /// The server rebased the change onto a newer base (R2): its workspace takes the new
    /// head at the agent's next step boundary.
    Rebased,
    /// The change did not merge onto the moved base and stays on its old one; the
    /// conflicts are stored on the server and [`conflict_paths`] names their files (R5).
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
        EventKind::Reverted => ChangeEvent::Reverted,
        EventKind::Rebased => ChangeEvent::Rebased,
        EventKind::Conflict => ChangeEvent::Conflict,
        _ => return None,
    };
    Some((change, meaning))
}

/// The files a `conflict` event names (R5).
pub fn conflict_paths(event: &Event) -> Vec<String> {
    event
        .detail
        .get("paths")
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
