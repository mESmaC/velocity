//! Deleting a thread (SPEC.md F12).
//!
//! Delete is immediate with no confirmation: the thread leaves the sidebar and a toast
//! offers Undo for 5 s. Nothing happens to the agent, Rosen workspace or saved state during
//! those seconds, so Undo restores everything exactly. When they are up, the deletion lands:
//! the agent stops, Rosen abandons its change and workspace under Rosen's deletion guard (R4),
//! and the thread's tasks go back to todo as its record leaves `state.json`.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::runtime::ThreadKey;

/// How long Undo stays on offer (F12).
pub const UNDO_WINDOW: Duration = Duration::from_secs(5);

/// How long a stopped agent gets to exit before Rosen abandons its workspace. Its process
/// holds the workspace as its working directory, which Windows refuses to delete.
pub const AGENT_EXIT_WAIT: Duration = Duration::from_secs(10);

/// What one Delete takes: a thread, or a swarm or goal swarm with its members (F10, F12).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Deletion {
    Thread(ThreadKey),
    Swarm {
        project: u32,
        swarm: u32,
        /// The members when Delete was chosen; landing reads them again.
        members: Vec<u32>,
    },
}

impl Deletion {
    pub fn project(&self) -> u32 {
        match self {
            Self::Thread((project, _)) | Self::Swarm { project, .. } => *project,
        }
    }

    /// The sidebar rows hidden while Undo is on offer.
    pub fn rows(&self) -> Vec<usize> {
        match self {
            Self::Thread((_, thread)) => Vec::from([*thread as usize]),
            Self::Swarm { swarm, members, .. } => std::iter::once(swarm)
                .chain(members)
                .map(|row| *row as usize)
                .collect(),
        }
    }

    /// Whether this deletion takes the thread or swarm `key` names.
    pub fn covers(&self, key: ThreadKey) -> bool {
        self.project() == key.0 && self.rows().contains(&(key.1 as usize))
    }
}

/// Deletions inside their undo window. Undo and landing each take the deletion out, so
/// whichever comes first wins and the other finds nothing.
#[derive(Debug, Default)]
pub struct PendingDeletions {
    next: u32,
    pending: BTreeMap<u32, Deletion>,
}

impl PendingDeletions {
    /// Starts the undo window for a deletion and returns its token.
    pub fn begin(&mut self, deletion: Deletion) -> u32 {
        self.next = self.next.wrapping_add(1);
        self.pending.insert(self.next, deletion);
        self.next
    }

    /// Whether a pending deletion takes this thread, swarm or swarm member.
    pub fn is_pending(&self, key: ThreadKey) -> bool {
        self.pending.values().any(|pending| pending.covers(key))
    }

    /// Undo: everything comes back untouched.
    pub fn undo(&mut self, token: u32) -> Option<Deletion> {
        self.pending.remove(&token)
    }

    /// The window is over: the deletion lands unless it was undone.
    pub fn land(&mut self, token: u32) -> Option<Deletion> {
        self.pending.remove(&token)
    }
}
