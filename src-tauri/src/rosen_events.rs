//! Follows each Rosen project's event stream (F6): a named snapshot moves its thread to
//! review, a land marks it merged and stops its agent, and a revert reopens it. This takes
//! the GitHub PR watcher's place for threads that work in Rosen workspaces.

use std::path::PathBuf;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use velocity_rosen::{ChangeEvent, EventStream};
use velocity_store::rosen::RosenProgress;
use velocity_threads::ThreadKey;

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::{lock, report};

/// How long a follower waits before connecting again after its stream ended, or while
/// the servers are stopped for an update.
const RECONNECT: Duration = Duration::from_secs(5);

/// Follows `project`'s events, unless a follower already does.
pub(crate) fn follow(application: &AppHandle, project: u32) {
    if !crate::rosen::begin_following(application, project) {
        return;
    }
    let handle = application.clone();
    tauri::async_runtime::spawn(async move { follow_project(&handle, project).await });
}

async fn follow_project(application: &AppHandle, project: u32) {
    let mut after = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .map(|store| store.rosen_event_cursor(project))
    .unwrap_or(0);
    loop {
        if !crate::rosen::uses_rosen(application, project) {
            crate::rosen::end_following(application, project);
            return;
        }
        if !crate::rosen::paused(application) {
            match open(application, project, after).await {
                Ok(mut stream) => {
                    while let Some(event) = stream.next().await {
                        after = after.max(event.seq);
                        if let Some((change, meaning)) = velocity_rosen::change_event(&event) {
                            report(take(application, project, event.seq, change, meaning).await);
                        }
                    }
                }
                Err(error) => eprintln!("Rosen events for project {project}: {error}"),
            }
        }
        tokio::time::sleep(RECONNECT).await;
    }
}

async fn open(application: &AppHandle, project: u32, after: u64) -> Result<EventStream, String> {
    let server = crate::rosen::server(application, project).await?;
    EventStream::open(&server, after)
        .await
        .map_err(|error| error.to_string())
}

/// Applies one event to its thread and shows the result.
async fn take(
    application: &AppHandle,
    project: u32,
    seq: u64,
    change: &str,
    meaning: ChangeEvent,
) -> Result<(), String> {
    let progress = match meaning {
        ChangeEvent::Named => RosenProgress::Named,
        ChangeEvent::Landed => RosenProgress::Landed,
        ChangeEvent::Reverted => RosenProgress::Reverted,
    };
    let thread = save(application, project, seq, change, progress)?;
    match (thread, meaning) {
        (Some(thread), ChangeEvent::Landed) => finish_landed(application, (project, thread)).await,
        _ => Ok(()),
    }
}

fn save(
    application: &AppHandle,
    project: u32,
    seq: u64,
    change: &str,
    progress: RosenProgress,
) -> Result<Option<u32>, String> {
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let thread = store
        .save_rosen_progress(project, seq, change, progress)
        .map_err(|error| error.to_string())?;
    let layout_state = application.state::<MainLayout>();
    let mut layout = lock(layout_state.inner(), "layout")?;
    store.refresh_follow_through_layout(&mut layout);
    let state = layout.snapshot();
    crate::layout::publish(application, &state, None)?;
    Ok(thread)
}

/// A landed thread's work is on the base, and Rosen deletes its workspace: its agent and
/// watcher stop, and its AIRC claims are released. Run again for a replayed event, so a
/// crash between the land and this cleanup is finished on the next start.
async fn finish_landed(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    let handle = application.clone();
    let stopped = tauri::async_runtime::spawn_blocking(move || {
        crate::threads::stop_follow_through(&handle, key)
    })
    .await
    .map_err(|error| error.to_string())??;
    if !stopped {
        return Err("The landed thread's agent is still starting; it was left running".into());
    }
    if let Some(root) = workspace(application, key) {
        crate::rosen::stop_watch(application, &root).await;
    }
    crate::airc_claims::release(application, key)
}

fn workspace(application: &AppHandle, key: ThreadKey) -> Option<PathBuf> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()?;
    let thread = store
        .project(key.0)?
        .threads
        .iter()
        .find(|thread| thread.id == key.1)?;
    thread.worktree_path.as_deref().map(PathBuf::from)
}
