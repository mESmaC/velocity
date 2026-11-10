//! The warm worktree pool in the app (F5): which projects keep one, and the managed pool the
//! launch paths take worktrees from.
use std::sync::Arc;

use tauri::{AppHandle, Manager};
use velocity_core::ready_pool::SlotState;
use velocity_core::worktree_settings::{WorktreeDisplayStatus, WorktreePreferences};
use velocity_core::worktree_view::WorktreeOwner;
use velocity_threads::{PoolHost, PoolProject, ReadyPools};

use crate::state::ManagedStateStore;

pub type ManagedReadyPools = ReadyPools;

/// The owner name ready worktrees show under in Settings → Worktrees.
const POOL_OWNER: &str = "warm pool";

struct Host(AppHandle);

impl PoolHost for Host {
    /// Projects whose spec is locked: only they start build work (Rule 2), so only they
    /// need worktrees ready.
    fn projects(&self) -> Vec<(PoolProject, WorktreePreferences)> {
        let state = self.0.state::<ManagedStateStore>();
        let Ok(store) = state.lock() else {
            return Vec::new();
        };
        store
            .state()
            .projects
            .iter()
            .filter(|project| project.spec_locked && !project.repository_path.trim().is_empty())
            .filter_map(|project| {
                let preferences = store.worktree_preferences(project);
                let pool = PoolProject::new(project, &preferences).ok()?;
                Some((pool, preferences))
            })
            .collect()
    }

    /// Settings → Worktrees re-reads its rows every second; nothing to push.
    fn changed(&self) {}
}

pub fn initialize(application: &AppHandle) -> anyhow::Result<()> {
    let pools = ReadyPools::new();
    application.manage(pools.clone());
    pools.start(Arc::new(Host(application.clone())))?;
    Ok(())
}

/// The pool, once [`initialize`] ran.
pub(crate) fn pools(application: &AppHandle) -> Option<ReadyPools> {
    application
        .try_state::<ManagedReadyPools>()
        .map(|pools| pools.inner().clone())
}

/// A swarm or goal swarm is starting `count` agents in `project` (F5 Swarm launches).
pub(crate) fn reserve(application: &AppHandle, project: u32, count: usize) {
    if let Some(pools) = pools(application) {
        pools.reserve(project, u32::try_from(count).unwrap_or(u32::MAX));
    }
}

/// `master` may have moved, or Settings → Worktrees changed: look again now.
pub(crate) fn wake(application: &AppHandle, project: Option<u32>) {
    if let Some(pools) = pools(application) {
        pools.wake(project);
    }
}

/// The project's ready worktrees as Settings → Worktrees rows. Slots still being checked
/// out are left out until their folder exists.
pub(crate) fn owners(application: &AppHandle, project: u32) -> Vec<WorktreeOwner> {
    let Some(pools) = pools(application) else {
        return Vec::new();
    };
    pools
        .rows(project)
        .into_iter()
        .filter_map(|row| {
            let status = match row.state {
                SlotState::Ready { .. } => WorktreeDisplayStatus::Ready,
                SlotState::Building { .. } => WorktreeDisplayStatus::Prebuilding,
                SlotState::Preparing | SlotState::Taken => return None,
            };
            Some(WorktreeOwner::new(
                0,
                POOL_OWNER.into(),
                row.path.display().to_string(),
                status,
            ))
        })
        .collect()
}
