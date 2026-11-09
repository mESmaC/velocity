//! The sidebar thread's right-click menu (SPEC.md F12).
//!
//! Which entries a thread gets, and what its Delete entry says it will do, follow the
//! reference's `ctxFor`: the worktree entries only while a worktree is on disk, the PR
//! entry only while the PR is not merged, and a red Delete whose label and detail depend on
//! the thread. Draft and duty rows have no menu.

use crate::state::ThreadState;

/// What the app knows about a thread when its menu opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadMenuFacts {
    pub state: ThreadState,
    /// Duty agents' threads are never deleted from the sidebar (F7, F12).
    pub duty: bool,
    pub worktree_path: Option<String>,
    pub pull_request_number: Option<u32>,
    /// The agent's process is set up or running in this app session.
    pub agent_running: bool,
    /// Bytes on disk under the worktree, when it exists and could be measured.
    pub worktree_bytes: Option<u64>,
}

impl ThreadMenuFacts {
    /// A thread that is not a duty agent's, with nothing measured on disk yet.
    pub fn new(state: ThreadState) -> Self {
        ThreadMenuFacts {
            state,
            duty: false,
            worktree_path: None,
            pull_request_number: None,
            agent_running: false,
            worktree_bytes: None,
        }
    }
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadMenuAction {
    OpenWorktree,
    CopyWorktreePath,
    OpenPullRequest,
    Delete,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThreadMenuEntry {
    Item {
        action: ThreadMenuAction,
        label: String,
        detail: Option<String>,
        danger: bool,
    },
    Separator,
}

/// Whether the thread still has its own worktree. A spec thread runs in the project's main
/// checkout, so its path, if any, is never a worktree Velocity may remove (F3, F5).
pub fn owns_worktree(state: ThreadState, worktree_path: Option<&str>) -> bool {
    worktree_path.is_some() && !matches!(state, ThreadState::Merged | ThreadState::Spec)
}

/// Whether the thread's PR is still open, so deleting the thread closes it.
pub fn has_open_pull_request(state: ThreadState, pull_request_number: Option<u32>) -> bool {
    pull_request_number.is_some() && !matches!(state, ThreadState::Merged | ThreadState::Closed)
}

/// The menu for one thread, or `None` when the row has no menu.
pub fn thread_menu(facts: &ThreadMenuFacts) -> Option<Vec<ThreadMenuEntry>> {
    if facts.duty || facts.state == ThreadState::Draft {
        return None;
    }
    let worktree = facts
        .worktree_path
        .as_deref()
        .filter(|path| owns_worktree(facts.state, Some(path)) && facts.worktree_bytes.is_some());
    let mut entries = Vec::new();
    if let Some(path) = worktree {
        entries.push(item(
            ThreadMenuAction::OpenWorktree,
            "Open worktree in Explorer",
            Some(path.to_owned()),
        ));
        entries.push(item(
            ThreadMenuAction::CopyWorktreePath,
            "Copy worktree path",
            None,
        ));
    }
    if let Some(number) = facts.pull_request_number
        && facts.state != ThreadState::Merged
    {
        entries.push(item(
            ThreadMenuAction::OpenPullRequest,
            &format!("Open PR #{number} on GitHub"),
            None,
        ));
    }
    if !entries.is_empty() {
        entries.push(ThreadMenuEntry::Separator);
    }
    entries.push(ThreadMenuEntry::Item {
        action: ThreadMenuAction::Delete,
        label: if worktree.is_some() {
            "Delete thread and worktree"
        } else {
            "Delete thread"
        }
        .into(),
        detail: Some(delete_detail(facts, worktree.is_some())),
        danger: true,
    });
    Some(entries)
}

fn item(action: ThreadMenuAction, label: &str, detail: Option<String>) -> ThreadMenuEntry {
    ThreadMenuEntry::Item {
        action,
        label: label.into(),
        detail,
        danger: false,
    }
}

/// "stops the agent · closes PR #n · 247 MB freed", or why nothing is freed.
fn delete_detail(facts: &ThreadMenuFacts, worktree: bool) -> String {
    let mut parts = Vec::new();
    if facts.agent_running {
        parts.push("stops the agent".to_owned());
    }
    if has_open_pull_request(facts.state, facts.pull_request_number)
        && let Some(number) = facts.pull_request_number
    {
        parts.push(format!("closes PR #{number}"));
    }
    if worktree && let Some(bytes) = facts.worktree_bytes {
        parts.push(format!("{} freed", size_label(bytes)));
    }
    if !parts.is_empty() {
        parts.join(" · ")
    } else if facts.state == ThreadState::Spec {
        "no worktree · SPEC.md stays".into()
    } else {
        "worktree already deleted".into()
    }
}

/// The reference's `fmtSize`: whole megabytes below 1000 MB, else gigabytes to one decimal.
pub fn size_label(bytes: u64) -> String {
    let megabytes = bytes.div_ceil(1_000_000);
    if megabytes >= 1000 {
        // `toFixed(1)` rounds to the nearest tenth.
        let tenths = (megabytes + 50) / 100;
        format!("{}.{} GB", tenths / 10, tenths % 10)
    } else {
        format!("{megabytes} MB")
    }
}
