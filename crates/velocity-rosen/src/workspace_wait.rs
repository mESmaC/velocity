//! What a slow workspace creation waits on, read from this machine's run queue (#514).
//!
//! Rosen's warm pool prepares its ready workspaces in the machine's run queue. While one
//! prepare holds the queue, or the queue starves, a thread's workspace can take minutes,
//! and the thread used to show only "creating from main".

use std::time::SystemTime;

use rosen_client::run_queue::{Entry, RunQueue};

/// What holds a workspace creation up.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CreationWait {
    /// The warm pool is preparing a ready workspace, since the time given when it is known.
    WarmPool { since: Option<SystemTime> },
    /// A run has waited long enough to starve. `reason` is what the queue says it waits for.
    RunQueue { reason: String },
}

impl CreationWait {
    /// The line the thread shows, with `clock` giving a time of day such as `12:27`.
    pub fn describe(&self, clock: impl Fn(SystemTime) -> String) -> String {
        match self {
            Self::WarmPool { since: Some(since) } => format!(
                "waiting for the warm pool: prepare running since {}",
                clock(*since)
            ),
            Self::WarmPool { since: None } => "waiting for the warm pool: prepare running".into(),
            Self::RunQueue { reason } => format!("waiting for the run queue: {reason}"),
        }
    }
}

/// What the run queue shows a workspace creation could be waiting on now, if anything.
/// Reads the queue's folder, so call it off the async runtime.
pub fn creation_wait() -> Option<CreationWait> {
    let line = RunQueue::machine().and_then(|queue| queue.line()).ok()?;
    if let Some(note) = prepare_note(&line) {
        return Some(CreationWait::WarmPool {
            since: turn_started(note),
        });
    }
    starving(&line).map(|reason| CreationWait::RunQueue { reason })
}

/// The note of a warm pool prepare that holds a turn. Rosen writes it as `prepare <folder>`.
fn prepare_note(line: &[Entry]) -> Option<&str> {
    line.iter()
        .find(|entry| entry.running && entry.note.trim_start().starts_with("prepare "))
        .map(|entry| entry.note.as_str())
}

/// What the first starving waiter waits for. The queue posts it only after five minutes.
fn starving(line: &[Entry]) -> Option<String> {
    line.iter()
        .filter(|entry| !entry.running)
        .find_map(|entry| entry.waiting_for.clone())
}

/// When the run described by `note` took its turn. The queue writes a turn's note file as
/// the turn starts and never rewrites it, so the file's time is the turn's start.
fn turn_started(note: &str) -> Option<SystemTime> {
    let folder = crate::status::queue_root()?;
    std::fs::read_dir(folder)
        .ok()?
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|kind| kind == "note"))
        .find(|entry| std::fs::read_to_string(entry.path()).is_ok_and(|text| text == note))
        .and_then(|entry| entry.metadata().ok()?.modified().ok())
}
