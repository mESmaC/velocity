//! Follows each Rosen project's event stream (F6): a named snapshot moves its thread to
//! review, a land marks it merged and stops its agent, and a revert reopens it, with its
//! workspace and its agent. This takes
//! the GitHub PR watcher's place for threads that work in Rosen workspaces.
//!
//! An event is taken in one save: the thread's new state, the stream's cursor, and what the
//! event leaves to do (a `RosenFollowUp`). The follow-ups run after that save and each is
//! removed only when it is done, so a crash or a failed step never acknowledges unfinished
//! work: the next pass, at start, on reconnect or every half minute, does it again. Every
//! step can be repeated: a workspace that exists is kept, and a message already in the
//! thread's saved transcript is not sent twice.

use std::path::PathBuf;
use std::time::Duration;

use tauri::{AppHandle, Manager};
use velocity_rosen::{ChangeEvent, EventStream};
use velocity_store::rosen::{RosenFollowUp, RosenProgress};
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
            // What a crash or a failed step left undone, before anything new.
            run_follow_ups(application, project).await;
            match open(application, project, after).await {
                Ok(mut stream) => {
                    while let Some(event) = stream.next().await {
                        after = after.max(event.seq);
                        match velocity_rosen::change_event(&event) {
                            Some((change, ChangeEvent::Rebased)) => {
                                crate::rosen_rebase::rebased(application, project, change).await;
                            }
                            Some((change, ChangeEvent::Conflict)) => {
                                let paths = velocity_rosen::conflict_paths(&event);
                                crate::rosen_rebase::conflicted(
                                    application,
                                    project,
                                    change,
                                    paths,
                                )
                                .await;
                            }
                            Some((change, meaning)) => {
                                let output = velocity_rosen::revert_output(&event);
                                let taken = take(
                                    application,
                                    project,
                                    (event.seq, change),
                                    meaning,
                                    output.as_deref(),
                                );
                                report(taken.map(|_| ()));
                                run_follow_ups(application, project).await;
                            }
                            None => {}
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

/// Takes one event in: its thread's new state, the stream's cursor and what is left to do,
/// in one save. Returns the thread working on the change, if one does.
fn take(
    application: &AppHandle,
    project: u32,
    (seq, change): (u64, &str),
    meaning: ChangeEvent,
    output: Option<&str>,
) -> Result<Option<u32>, String> {
    let (progress, message) = match meaning {
        ChangeEvent::Named => (RosenProgress::Named, String::new()),
        ChangeEvent::Landed => (RosenProgress::Landed, String::new()),
        ChangeEvent::Reverted => {
            let report = output.unwrap_or("Rosen kept no output for the failing run.");
            let message = velocity_core::rosen_checker::reverted_message(change, report);
            (RosenProgress::Reverted, message)
        }
        // Handled by `rosen_rebase`: a rebase at the agent's step boundary, a conflict as a
        // message to it.
        ChangeEvent::Rebased | ChangeEvent::Conflict => return Ok(None),
    };
    let thread = save(application, project, (seq, change), (progress, &message))?;
    // A named change waits for a reviewer, and a land or a revert changes what PR duty and
    // its checker have to do: they look now, not at their next 30-second tick.
    crate::reviews::wake(application);
    Ok(thread)
}

/// Runs what `project`'s Rosen events left to do, oldest first per thread. A step that fails
/// stays, with the ones after it, for the next pass; nothing is dropped. One pass runs at a
/// time across projects, so two never send one message.
pub(crate) async fn run_follow_ups(application: &AppHandle, project: u32) {
    let running = application.state::<crate::rosen::ManagedRosen>();
    let _one_at_a_time = running.follow_ups.lock().await;
    let pending = match lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) {
        Ok(store) => store.rosen_follow_ups(project),
        Err(error) => return report::<()>(Err(error)),
    };
    for (thread, change, follow_ups) in pending {
        let key = (project, thread);
        for follow_up in follow_ups {
            let done = follow_up_once(application, key, change.as_deref(), &follow_up).await;
            if let Err(error) = done.and_then(|()| finish(application, key, &follow_up)) {
                report::<()>(Err(error));
                break;
            }
        }
    }
}

async fn follow_up_once(
    application: &AppHandle,
    key: ThreadKey,
    change: Option<&str>,
    follow_up: &RosenFollowUp,
) -> Result<(), String> {
    match follow_up {
        RosenFollowUp::FinishLanded => finish_landed(application, key).await,
        RosenFollowUp::Reopen { message, .. } => {
            let Some(change) = change else {
                return Ok(());
            };
            reopen_reverted(application, key, change).await?;
            deliver(application, key, follow_up, message)
        }
        RosenFollowUp::Deliver { message, .. } => deliver(application, key, follow_up, message),
    }
}

/// Sends the agent `message`, resumed first if it had ended, unless an earlier pass already
/// did and was cut off before it could say so.
fn deliver(
    application: &AppHandle,
    key: ThreadKey,
    follow_up: &RosenFollowUp,
    message: &str,
) -> Result<(), String> {
    let delivered = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .rosen_follow_up_delivered(key.0, key.1, follow_up);
    if delivered {
        return Ok(());
    }
    crate::threads::steer(application, key, message).map(|_| ())
}

fn finish(application: &AppHandle, key: ThreadKey, done: &RosenFollowUp) -> Result<(), String> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .finish_rosen_follow_up(key.0, key.1, done)
    .map_err(|error| error.to_string())
}

/// Rosen reverted thread `key`'s landed change and reopened it (Rosen R12; F7 "After a
/// land"). The workspace went when the change landed, so the thread gets one again at the
/// same folder, at the reopened head; its agent is then resumed with what the failing
/// post-land checks printed. A workspace already there is kept, so this can run again.
async fn reopen_reverted(
    application: &AppHandle,
    key: ThreadKey,
    change: &str,
) -> Result<(), String> {
    let Some(root) = workspace(application, key) else {
        return Ok(());
    };
    let server = crate::rosen::server(application, key.0).await?;
    let agent = velocity_rosen::agent_name(&root);
    velocity_rosen::reopen_workspace(&server, &agent, change, &root)
        .await
        .map_err(|error| error.to_string())?;
    // The land stopped the thread's session for good; the revert gives it work again.
    lock(
        application
            .state::<crate::threads::ManagedThreads>()
            .inner(),
        "thread sessions",
    )?
    .reopen(key);
    Ok(())
}

/// PR duty requested changes on thread `key`'s change `change` (F7): the thread goes back
/// to fixing, and its agent gets `message`, resumed first if it had ended. The message is
/// saved with the state, and sent by the follow-up pass this starts.
pub(crate) fn changes_requested(
    application: &AppHandle,
    key: ThreadKey,
    change: &str,
    message: &str,
) -> Result<(), String> {
    // Sequence 0 leaves the stream's cursor where it is: this is not one of its events.
    let progress = (RosenProgress::ChangesRequested, message);
    save(application, key.0, (0, change), progress)?;
    let handle = application.clone();
    tauri::async_runtime::spawn(async move { run_follow_ups(&handle, key.0).await });
    Ok(())
}

fn save(
    application: &AppHandle,
    project: u32,
    (seq, change): (u64, &str),
    progress: (RosenProgress, &str),
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
/// watcher stop, and its AIRC claims are released. Each step can run again, so a crash
/// between the land and this cleanup is finished on the next pass.
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
