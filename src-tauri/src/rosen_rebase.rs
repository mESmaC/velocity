//! Rosen's server rebases written into agents' workspaces (F5, #271). Rosen rebases a
//! change on the server when its base moves (R2); the workspace takes the rebased head only
//! when its owner asks. Velocity asks at the agent's step boundary: between turns, or while
//! no session runs. Files never change under a running step, as with `airc_rebase`.
//!
//! Whether a rebase waits is read from Rosen each time, not remembered: a `rebased` event,
//! the end of each turn and the 30-second check all ask, so one Rosen refused (an edit not
//! stored yet) or one that arrived while Velocity was closed is applied at the next ask.

use tauri::{AppHandle, Manager};
use velocity_store::ThreadState;
use velocity_threads::ThreadKey;

use crate::state::ManagedStateStore;
use crate::threads::{ManagedThreads, lock};

/// Rosen rebased `change`: its thread's workspace takes the new head at the agent's next
/// step boundary, which may be now.
pub(crate) async fn rebased(application: &AppHandle, project: u32, change: &str) {
    if let Some(key) = thread_of(application, project, change) {
        apply_at_boundary(application, key).await;
    }
}

/// Applies the waiting rebase of every open Rosen thread whose agent is at a step boundary.
pub(crate) async fn apply_pending(application: &AppHandle) {
    for key in open_threads(application) {
        apply_at_boundary(application, key).await;
    }
}

/// Applies `key`'s waiting rebase, if there is one and its agent is at a step boundary.
pub(crate) async fn apply_at_boundary(application: &AppHandle, key: ThreadKey) -> bool {
    if crate::rosen::paused(application) {
        return false;
    }
    let Some(root) = workspace(application, key) else {
        return false;
    };
    if !lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .is_ok_and(|mut runtimes| runtimes.begin_workspace_update(key))
    {
        return false;
    }
    let applied = apply_current_head(application, key, &root).await;
    let boundary = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .is_ok_and(|mut runtimes| {
        runtimes.finish_workspace_update(key);
        runtimes.is_at_step_boundary(key)
    });
    applied && boundary
}

async fn apply_current_head(
    application: &AppHandle,
    key: ThreadKey,
    root: &std::path::Path,
) -> bool {
    // Unstored edits, or the server is away: the workspace is as it was, and the rebase
    // still waits on the server for the next ask.
    if let Err(error) = crate::rosen::apply_rebase(application, key.0, root).await {
        eprintln!("Rosen rebase for thread {}: {error}", key.1);
        return false;
    }
    // None from apply_rebase can mean another application or watcher startup held it.
    // Do not promise the agent it can edit until the marker actually names the head.
    let current = async {
        let server = crate::rosen::server(application, key.0).await?;
        let agent = velocity_rosen::agent_name(root);
        velocity_rosen::rebase_pending(&server, &agent, root)
            .await
            .map(|pending| !pending)
            .map_err(|error| error.to_string())
    }
    .await;
    match current {
        Ok(current) => current,
        Err(error) => {
            eprintln!("Rosen workspace basis for thread {}: {error}", key.1);
            false
        }
    }
}

/// Record a conflict before acknowledging its event. Delivery waits in the durable
/// follow-up queue until the rebased head is visible at a turn boundary.
pub(crate) fn conflicted(
    application: &AppHandle,
    project: u32,
    change: &str,
    sequence: u64,
    paths: Vec<String>,
) -> Result<(), String> {
    let message = velocity_rosen::conflict_message(sequence, &paths);
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .save_rosen_conflict(project, sequence, change, &message)
    .map_err(|error| error.to_string())
}

/// The open thread working on `change` in `project`.
fn thread_of(application: &AppHandle, project: u32, change: &str) -> Option<ThreadKey> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()?;
    store
        .project(project)?
        .threads
        .iter()
        .find(|thread| {
            thread.rosen_change.as_deref() == Some(change) && thread.state != ThreadState::Merged
        })
        .map(|thread| (project, thread.id))
}

/// Every open thread that works on a Rosen change.
fn open_threads(application: &AppHandle) -> Vec<ThreadKey> {
    let Ok(store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) else {
        return Vec::new();
    };
    store
        .state()
        .projects
        .iter()
        .flat_map(|project| {
            project
                .threads
                .iter()
                .filter(|thread| {
                    thread.rosen_change.is_some() && thread.state != ThreadState::Merged
                })
                .map(move |thread| (project.id, thread.id))
        })
        .collect()
}

/// The thread's Rosen workspace, while it is still on disk. A thread in a git worktree has
/// none: every turn's end asks, and only Rosen threads have a rebase to take.
fn workspace(application: &AppHandle, key: ThreadKey) -> Option<std::path::PathBuf> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()?;
    let thread = store.project(key.0)?.threads.iter().find(|thread| {
        thread.id == key.1 && thread.rosen_change.is_some() && thread.state != ThreadState::Merged
    })?;
    let root = std::path::PathBuf::from(thread.worktree_path.as_deref()?);
    root.is_dir().then_some(root)
}
