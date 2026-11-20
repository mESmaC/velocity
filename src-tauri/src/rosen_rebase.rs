//! Rosen's server rebases written into agents' workspaces (F5, #271). Rosen rebases a
//! change on the server when its base moves (R2); the workspace takes the rebased head only
//! when its owner asks. Velocity asks at the agent's step boundary: between turns, or while
//! no session runs. Files never change under a running step, as with `airc_rebase`.
//!
//! Whether a rebase waits is read from Rosen each time, not remembered: a `rebased` event,
//! the end of each turn and the 30-second check all ask, so one Rosen refused (an edit not
//! stored yet) or one that arrived while Velocity was closed is applied at the next ask.

use std::path::{Path, PathBuf};

use tauri::{AppHandle, Manager};
use velocity_store::ThreadState;
use velocity_threads::ThreadKey;

use crate::state::ManagedStateStore;
use crate::threads::{ManagedThreads, lock};

/// Outcome of preparing a thread's Rosen workspace at a step boundary (#522).
pub(crate) enum BoundaryOutcome {
    /// Workspace is on disk at the server's current head and the thread is at a boundary.
    Ready,
    /// Temporary wait (agent mid-step, Rosen paused, workspace syncing); retry later.
    Waiting(String),
    /// Workspace folder is missing and could not be recreated; offer Abandon.
    WorkspaceMissing(String),
    /// Applying the server's head or checking the workspace failed.
    Failed(String),
}

/// Rosen rebased `change`: its thread's workspace takes the new head at the agent's next
/// step boundary, which may be now.
pub(crate) async fn rebased(
    application: &AppHandle,
    project: u32,
    change: &str,
    sequence: u64,
    regenerate: &[String],
) -> Result<(), String> {
    {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        if regenerate.is_empty() {
            store.save_rosen_cursor(project, sequence)
        } else {
            let message = velocity_rosen::regeneration_message(sequence, regenerate);
            store.save_rosen_regeneration(project, sequence, change, &message)
        }
        .map_err(|error| error.to_string())?;
    }
    if let Some(key) = thread_of(application, project, change) {
        apply_at_boundary(application, key).await;
    }
    Ok(())
}

/// Applies the waiting rebase of every open Rosen thread whose agent is at a step boundary.
pub(crate) async fn apply_pending(application: &AppHandle) {
    for key in open_threads(application) {
        apply_at_boundary(application, key).await;
    }
}

/// Applies `key`'s waiting rebase, if there is one and its agent is at a step boundary.
pub(crate) async fn apply_at_boundary(application: &AppHandle, key: ThreadKey) -> bool {
    matches!(
        prepare_at_boundary(application, key, None, false).await,
        BoundaryOutcome::Ready
    )
}

/// Ensures `key`'s workspace is on disk and synced to the server's current head at a step
/// boundary. When `recreate_missing` is true and `change` is given, a missing workspace folder
/// is recreated from `change` before applying or delivering (#522).
pub(crate) async fn prepare_at_boundary(
    application: &AppHandle,
    key: ThreadKey,
    change: Option<&str>,
    recreate_missing: bool,
) -> BoundaryOutcome {
    if crate::rosen::paused(application) {
        return BoundaryOutcome::Waiting("Rosen is paused".into());
    }
    let root = match workspace_path(application, key) {
        WorkspaceLookup::Found(root) => root,
        WorkspaceLookup::MissingPath => {
            let reason = "workspace missing — this thread has no workspace folder".to_owned();
            return if recreate_missing {
                BoundaryOutcome::WorkspaceMissing(reason)
            } else {
                BoundaryOutcome::Waiting(reason)
            };
        }
        WorkspaceLookup::NotOpen => {
            return BoundaryOutcome::Waiting(
                "this thread no longer has an open Rosen change".into(),
            );
        }
    };
    let reserved = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .map(|mut runtimes| runtimes.begin_rebase_update(key));
    match reserved {
        Ok(Ok(())) => {}
        Ok(Err(reason)) => return BoundaryOutcome::Waiting(reason.to_owned()),
        Err(error) => return BoundaryOutcome::Failed(error),
    }
    let outcome = apply_or_recreate(application, key, &root, change, recreate_missing).await;
    let post_blocker = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .map(|mut runtimes| {
        runtimes.finish_workspace_update(key);
        runtimes.rebase_boundary_blocker(key)
    });
    match outcome {
        BoundaryOutcome::Ready => match post_blocker {
            Ok(None) => BoundaryOutcome::Ready,
            Ok(Some(reason)) => BoundaryOutcome::Waiting(reason.to_owned()),
            Err(error) => BoundaryOutcome::Failed(error),
        },
        other => other,
    }
}

async fn apply_or_recreate(
    application: &AppHandle,
    key: ThreadKey,
    root: &Path,
    change: Option<&str>,
    recreate_missing: bool,
) -> BoundaryOutcome {
    if !velocity_rosen::has_workspace(root) {
        if !recreate_missing {
            return BoundaryOutcome::WorkspaceMissing("workspace missing".into());
        }
        let Some(change) = change else {
            return BoundaryOutcome::WorkspaceMissing("workspace missing".into());
        };
        if let Some(outcome) = recreate_workspace(application, key.0, change, root).await {
            return outcome;
        }
    }
    apply_current_head(application, key, root).await
}

async fn recreate_workspace(
    application: &AppHandle,
    project: u32,
    change: &str,
    root: &Path,
) -> Option<BoundaryOutcome> {
    let server = match crate::rosen::server(application, project).await {
        Ok(server) => server,
        Err(error) => return Some(BoundaryOutcome::Failed(error)),
    };
    let agent = velocity_rosen::agent_name(root);
    if let Err(error) = velocity_rosen::reopen_workspace(&server, &agent, change, root).await {
        return Some(BoundaryOutcome::WorkspaceMissing(format!(
            "workspace missing and could not be recreated: {error}"
        )));
    }
    if let Err(error) = crate::rosen::watch(application, &server, root).await {
        eprintln!(
            "Rosen watch for recreated workspace {}: {error}",
            root.display()
        );
    }
    None
}

async fn apply_current_head(
    application: &AppHandle,
    key: ThreadKey,
    root: &Path,
) -> BoundaryOutcome {
    // Unstored edits, or the server is away: the workspace is as it was, and the rebase
    // still waits on the server for the next ask.
    if let Err(error) = crate::rosen::apply_rebase(application, key.0, root).await {
        eprintln!("Rosen rebase for thread {}: {error}", key.1);
        return BoundaryOutcome::Failed(format!("Could not apply rebased head: {error}"));
    }
    // None from apply_rebase can mean another application or watcher startup held it.
    // Do not promise the agent it can edit until the marker actually names the head.
    let current = async {
        let server = crate::rosen::server(application, key.0).await?;
        let agent = velocity_rosen::agent_name(root);
        velocity_rosen::rebase_pending(&server, &agent, root)
            .await
            .map_err(|error| error.to_string())
    }
    .await;
    match current {
        Ok(false) => BoundaryOutcome::Ready,
        Ok(true) => BoundaryOutcome::Waiting("waiting for the workspace to finish syncing".into()),
        Err(error) => {
            eprintln!("Rosen workspace basis for thread {}: {error}", key.1);
            BoundaryOutcome::Failed(format!("Could not check workspace basis: {error}"))
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

enum WorkspaceLookup {
    Found(PathBuf),
    MissingPath,
    NotOpen,
}

/// The thread's recorded Rosen workspace path, whether or not the folder is still on disk.
fn workspace_path(application: &AppHandle, key: ThreadKey) -> WorkspaceLookup {
    let Ok(store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) else {
        return WorkspaceLookup::NotOpen;
    };
    let Some(thread) = store.project(key.0).and_then(|project| {
        project.threads.iter().find(|thread| {
            thread.id == key.1
                && thread.rosen_change.is_some()
                && thread.state != ThreadState::Merged
        })
    }) else {
        return WorkspaceLookup::NotOpen;
    };
    match thread.worktree_path.as_deref() {
        Some(path) if !path.is_empty() => WorkspaceLookup::Found(PathBuf::from(path)),
        _ => WorkspaceLookup::MissingPath,
    }
}
