//! The sidebar thread's right-click menu (SPEC.md F12).
//!
//! Which entries a thread gets, and what its Delete entry says it will do, follow the
//! reference's `ctxFor`: the worktree entries only while a worktree is on disk, and a red
//! Delete whose label and detail depend on the thread. Draft and duty rows have no menu.

use crate::state::ThreadState;

/// What the app knows about a thread when its menu opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadMenuFacts {
    pub state: ThreadState,
    /// Duty agents' threads are never deleted from the sidebar (F7, F12).
    pub duty: bool,
    pub worktree_path: Option<String>,
    /// The agent's process is set up or running in this app session.
    pub agent_running: bool,
    /// A failed Rosen launch that can retry its workspace setup.
    pub retry_workspace: bool,
    /// A read-only question thread has no workspace, change or claim (#533).
    pub read_only: bool,
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
            agent_running: false,
            retry_workspace: false,
            read_only: false,
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
    RetryWorkspace,
    OpenWorkspace,
    CopyWorkspacePath,
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
    worktree_path.is_some() && !matches!(state, ThreadState::Landed | ThreadState::Spec)
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
    if facts.retry_workspace && facts.state == ThreadState::Error && !facts.agent_running {
        entries.push(item(
            ThreadMenuAction::RetryWorkspace,
            "Retry Rosen workspace",
            None,
        ));
    }
    if let Some(path) = worktree {
        entries.push(item(
            ThreadMenuAction::OpenWorkspace,
            "Open workspace in Explorer",
            Some(path.to_owned()),
        ));
        entries.push(item(
            ThreadMenuAction::CopyWorkspacePath,
            "Copy workspace path",
            None,
        ));
    }
    if !entries.is_empty() {
        entries.push(ThreadMenuEntry::Separator);
    }
    entries.push(ThreadMenuEntry::Item {
        action: ThreadMenuAction::Delete,
        label: if worktree.is_some() {
            "Delete thread and workspace"
        } else {
            "Delete thread"
        }
        .into(),
        detail: Some(delete_detail(facts, worktree.is_some())),
        danger: true,
    });
    Some(entries)
}

/// What the app knows about a swarm's or goal swarm's members when its row's menu opens.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SwarmMenuFacts {
    /// Members whose agent is set up or running in this app session.
    pub agents_running: usize,
    /// Members that still have their own worktree on disk.
    pub worktrees: usize,
    /// Bytes on disk under those worktrees, as far as they could be measured.
    pub worktree_bytes: u64,
}

impl SwarmMenuFacts {
    pub fn new() -> Self {
        Self::default()
    }
}

/// A swarm row's menu: one red Delete that takes the swarm and all its members, as the
/// reference's `ctxFor` builds it for a swarm (F10, F12).
pub fn swarm_menu(facts: &SwarmMenuFacts) -> Vec<ThreadMenuEntry> {
    let mut parts = Vec::new();
    match facts.agents_running {
        0 => {}
        1 => parts.push("stops the agent".to_owned()),
        count => parts.push(format!("stops {count} agents")),
    }
    if facts.worktrees > 0 {
        parts.push(format!("{} freed", size_label(facts.worktree_bytes)));
    }
    let worktrees = facts.worktrees;
    vec![ThreadMenuEntry::Item {
        action: ThreadMenuAction::Delete,
        label: format!(
            "Delete swarm and its {worktrees} workspace{}",
            if worktrees == 1 { "" } else { "s" }
        ),
        detail: Some(if parts.is_empty() {
            "workspace already deleted".into()
        } else {
            parts.join(" · ")
        }),
        danger: true,
    }]
}

fn item(action: ThreadMenuAction, label: &str, detail: Option<String>) -> ThreadMenuEntry {
    ThreadMenuEntry::Item {
        action,
        label: label.into(),
        detail,
        danger: false,
    }
}

/// "stops the agent · 247 MB freed", or why nothing is freed.
fn delete_detail(facts: &ThreadMenuFacts, worktree: bool) -> String {
    let mut parts = Vec::new();
    if facts.agent_running {
        parts.push("stops the agent".to_owned());
    }
    if worktree && let Some(bytes) = facts.worktree_bytes {
        parts.push(format!("{} freed", size_label(bytes)));
    }
    if !parts.is_empty() {
        parts.join(" · ")
    } else if facts.read_only {
        "read-only · no workspace".into()
    } else if facts.state == ThreadState::Spec {
        "no workspace · SPEC.md stays".into()
    } else {
        "workspace already deleted".into()
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
