//! Tauri commands for the Goal swarm dialog (SPEC.md §3.1, F16). The dialog is drawn in the
//! crates; this file reads the work pool off the UI thread and keeps the last read.

use std::sync::Mutex;

use tauri::{AppHandle, Manager};
use velocity_core::goal_swarm::PoolCounts;
use velocity_store::goal_swarm_dialog::{GoalPool, GoalSwarmDialog, GoalSwarmDialogQuery};

use crate::state::ManagedStateStore;
use crate::threads::lock;

/// Starting the agents is its own piece (SPEC.md §7 M9 Launch); the dialog says so until then.
const LAUNCH_NOT_BUILT: &str = "Launch is not built yet (SPEC.md §7 M9)";

/// The work pool as the dialog last read it, and the project it belongs to.
#[derive(Default)]
pub struct GoalSwarmPool(Mutex<Option<(u32, GoalPool)>>);

/// The Goal swarm dialog for the user's choices with one edit applied; asked again after
/// every edit, and after the pool is read.
#[tauri::command]
#[specta::specta]
pub fn get_goal_swarm_dialog(
    application: AppHandle,
    query: GoalSwarmDialogQuery,
) -> Result<GoalSwarmDialog, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let active = store.active_project().map(|project| project.id);
    let pool = lock(&application.state::<GoalSwarmPool>().0, "the work pool")?
        .as_ref()
        .filter(|(project, _)| Some(*project) == active)
        .map_or(GoalPool::Reading, |(_, pool)| pool.clone());
    let mut dialog = store
        .goal_swarm_dialog(&query, &pool)
        .ok_or("Select or create a project first")?;
    dialog.launch_note = Some(LAUNCH_NOT_BUILT.into());
    Ok(dialog)
}

/// Reads the active project's work pool for the pool line and the PR duty check. Issues
/// come from GitHub and sections from SPEC.md, so the read runs off the UI thread; the
/// dialog shows the previous read meanwhile.
#[tauri::command]
#[specta::specta]
pub async fn read_goal_swarm_pool(application: AppHandle) -> Result<(), String> {
    let project = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .active_project()
    .cloned()
    .ok_or("Select or create a project first")?;
    let identifier = project.id;
    let read = tauri::async_runtime::spawn_blocking(move || {
        velocity_threads::work_pool::read_work_pool(&project)
    })
    .await
    .map_err(|error| error.to_string())?;
    let pool = match read {
        Ok(pool) => GoalPool::Read(PoolCounts {
            tasks: pool.available_tasks,
            issues: pool.available_issues,
            sections: pool.available_sections,
        }),
        Err(error) => GoalPool::Failed(format!("Could not read the work pool: {error}")),
    };
    *lock(&application.state::<GoalSwarmPool>().0, "the work pool")? = Some((identifier, pool));
    Ok(())
}
