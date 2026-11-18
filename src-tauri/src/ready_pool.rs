//! The warm worktree pool in the app (F5): which projects keep one, and the managed pool the
//! launch paths take worktrees from.
use std::collections::HashMap;
use std::path::Path;
use std::process::Child;
use std::sync::{Arc, Mutex};

use tauri::{AppHandle, Manager};
use velocity_core::worktree_settings::WorktreePreferences;
use velocity_threads::{PoolHost, PoolProject, ReadyPools};

use crate::state::ManagedStateStore;

pub type ManagedReadyPools = ReadyPools;

struct Host {
    application: AppHandle,
    /// Each project's running graph warming, so a merge that catches up four slots warms once.
    warming: Mutex<HashMap<u32, Child>>,
}

impl PoolHost for Host {
    /// Projects whose spec is locked: only they start build work (Rule 2), so only they
    /// need worktrees ready.
    fn projects(&self) -> Vec<(PoolProject, WorktreePreferences)> {
        let state = self.application.state::<ManagedStateStore>();
        let Ok(store) = state.lock() else {
            return Vec::new();
        };
        store
            .state()
            .projects
            .iter()
            .filter(|project| project.spec_locked && !project.repository_path.trim().is_empty())
            // A Rosen project's threads take workspaces from Rosen's own pool, which the
            // project's prepare commands build in (Rosen R3). Git worktrees kept ready here
            // would be prebuilt and never used; the rare fallback makes its worktree then.
            .filter(|project| project.version_control != velocity_store::VersionControl::Rosen)
            .filter_map(|project| {
                let preferences = store.worktree_preferences(project);
                let pool = PoolProject::new(project, &preferences).ok()?;
                Some((pool, preferences))
            })
            .collect()
    }

    /// Settings → Worktrees re-reads its rows every second; nothing to push.
    fn changed(&self) {}

    fn base_tip(&self, project: &PoolProject) -> Result<Option<String>, String> {
        let synchronized = crate::rosen::git_base(
            &self.application,
            project.identifier,
            &project.repository_root,
            &project.base_branch,
        )?;
        Ok(synchronized.or_else(|| {
            velocity_git::sweep_base(&project.repository_root, &project.base_branch)
                .map(|(_, commit)| commit)
        }))
    }

    /// Parses what the merge changed into the project's graph cache (F19 Warming), unless a
    /// warming run for the project is still going: it covers the same tip.
    fn slot_at_tip(&self, project: u32, slot: &Path) {
        let mut warming = self
            .warming
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if let Some(running) = warming.get_mut(&project)
            && matches!(running.try_wait(), Ok(None))
        {
            return;
        }
        let (Ok(executable), Ok(app_data)) = (
            std::env::current_exe(),
            self.application.path().app_data_dir(),
        ) else {
            return;
        };
        if !velocity_graph::usage::enabled(&app_data, &project.to_string()) {
            return;
        }
        match velocity_graph::warm(&executable, &app_data, &project.to_string(), slot) {
            Ok(child) => {
                warming.insert(project, child);
            }
            Err(error) => eprintln!("Velocity code graph warming of {}: {error}", slot.display()),
        }
    }
}

pub fn initialize(application: &AppHandle) -> anyhow::Result<()> {
    let pools = ReadyPools::new();
    application.manage(pools.clone());
    pools.start(Arc::new(Host {
        application: application.clone(),
        warming: Mutex::default(),
    }))?;
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
    if crate::rosen::uses_rosen(application, project) {
        // Launch callers run off the UI thread. Finish sizing before any member takes a
        // workspace, rather than racing a detached reservation against its first take.
        if let Err(error) =
            tauri::async_runtime::block_on(crate::rosen::reserve_pool(application, project, count))
        {
            eprintln!("Rosen warm pool reservation: {error}");
        }
        return;
    }
    if let Some(pools) = pools(application) {
        pools.reserve(project, u32::try_from(count).unwrap_or(u32::MAX));
    }
}

/// The base or workspace preferences may have changed: look again now.
pub(crate) fn wake(application: &AppHandle, project: Option<u32>) {
    if let Some(pools) = pools(application) {
        pools.wake(project);
    }
}
