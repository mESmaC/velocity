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
pub(crate) async fn apply_at_boundary(application: &AppHandle, key: ThreadKey) {
    if crate::rosen::paused(application) || !at_step_boundary(application, key) {
        return;
    }
    let Some(root) = workspace(application, key) else {
        return;
    };
    // Unstored edits, or the server is away: the workspace is as it was, and the rebase
    // still waits on the server for the next ask.
    if let Err(error) = crate::rosen::apply_rebase(application, key.0, &root).await {
        eprintln!("Rosen rebase for thread {}: {error}", key.1);
    }
}

/// Rosen could not merge `change` onto the moved base: `paths` conflict. Nothing is
/// written to the workspace; the change stays on its old base until its agent makes those
/// files merge, so the agent is told what the base changed in them, as its next message.
pub(crate) async fn conflicted(
    application: &AppHandle,
    project: u32,
    change: &str,
    paths: Vec<String>,
) {
    let Some(key) = thread_of(application, project, change) else {
        return;
    };
    let message = match crate::rosen::server(application, project).await {
        Ok(server) => velocity_rosen::conflict_message(&server, change, &paths)
            .await
            .map_err(|error| error.to_string()),
        Err(error) => Err(error),
    };
    let message = message
        .inspect_err(|error| eprintln!("Rosen conflict diff for thread {}: {error}", key.1))
        .ok();
    // Without the base's diff the agent still hears which files conflict.
    let message = message.unwrap_or_else(|| velocity_rosen::conflict_message_without_diff(&paths));
    crate::threads::report(crate::threads::steer(application, key, &message).map(|_| ()));
}

fn at_step_boundary(application: &AppHandle, key: ThreadKey) -> bool {
    lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .is_ok_and(|runtimes| runtimes.is_at_step_boundary(key))
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

/// The thread's workspace, while it is still on disk.
fn workspace(application: &AppHandle, key: ThreadKey) -> Option<std::path::PathBuf> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()?;
    let thread = store
        .project(key.0)?
        .threads
        .iter()
        .find(|thread| thread.id == key.1 && thread.state != ThreadState::Merged)?;
    let root = std::path::PathBuf::from(thread.worktree_path.as_deref()?);
    root.is_dir().then_some(root)
}
