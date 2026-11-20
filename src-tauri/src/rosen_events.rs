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
use velocity_rosen::{ChangeEvent, EventReplay, EventStream};
use velocity_store::rosen::{RosenFollowUp, RosenProgress};
use velocity_store::rosen_work::WorkEvidence;
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
    let warm = crate::code_graph::follow_rosen_base(application, project);
    let mut position = (String::new(), 0);
    loop {
        if !crate::rosen::rosen_in_use(application, project) {
            connection_state(application, project, false);
            crate::rosen::end_following(application, project);
            return;
        }
        if !crate::rosen::paused(application) {
            match open(application, project, &mut position).await {
                Ok((mut stream, replay, rewound)) => {
                    connection_state(application, project, false);
                    if rewound {
                        report(
                            crate::rosen_thread::refresh(application, project, None, true).await,
                        );
                    }
                    report(restore_work(application, project));
                    // What a crash or a failed step left undone, before anything new.
                    run_follow_ups(application, project).await;
                    report(crate::rosen_mirror::refresh(application, project).await);
                    if let Some(warm) = &warm {
                        let _ = warm.try_send(());
                    }
                    follow_connected(
                        application,
                        project,
                        &mut stream,
                        &replay,
                        &mut position.1,
                        warm.as_ref(),
                    )
                    .await;
                }
                Err(error) => eprintln!("Rosen events for project {project}: {error}"),
            }
        }
        connection_state(application, project, true);
        tokio::time::sleep(RECONNECT).await;
    }
}

async fn follow_connected(
    application: &AppHandle,
    project: u32,
    stream: &mut EventStream,
    replay: &EventReplay,
    after: &mut u64,
    warm: Option<&std::sync::mpsc::SyncSender<()>>,
) {
    use velocity_rosen::ProjectEventKind::*;
    let mut refresh = tokio::time::interval(RECONNECT);
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            _ = refresh.tick() => {
                report(crate::rosen_thread::refresh(application, project, None, false).await);
            }
            event = stream.next() => {
                let Some(event) = event else { break };
                let superseded = replay.superseded(&event);
                if let Err(error) = consume_connected(application, project, replay, &event, superseded).await {
                    report::<()>(Err(error));
                    break;
                }
                *after = (*after).max(event.seq);
                if superseded {
                    continue;
                }
                if matches!(event.kind, Landed | Reverted) && let Some(warm) = warm {
                    let _ = warm.try_send(());
                }
                match event.kind {
                    Built | Green => report(crate::rosen_thread::refresh(application, project, None, false).await),
                    Landed | Reverted | Updated | Conflict | Rebased | PostLandFailure | Review => {
                        report(crate::rosen_thread::refresh(application, project, event.change.as_deref(), true).await);
                    }
                    Snapshot if velocity_rosen::change_event(&event).is_some() => {
                        report(crate::rosen_thread::refresh(application, project, event.change.as_deref(), true).await);
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn consume_connected(
    application: &AppHandle,
    project: u32,
    replay: &EventReplay,
    event: &velocity_rosen::ProjectEvent,
    superseded: bool,
) -> Result<(), String> {
    if !superseded {
        consume(application, project, event).await?;
    }
    // The last event of a rewind can be unrelated and carry no checkpoint of its own.
    if event.seq == replay.boundary() {
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .save_rosen_cursor(project, event.seq)
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn connection_state(application: &AppHandle, project: u32, reconnecting: bool) {
    if let Ok(mut layout) = lock(application.state::<MainLayout>().inner(), "layout")
        && layout.set_rosen_reconnecting(project, reconnecting)
    {
        report(crate::layout::publish(
            application,
            &layout.snapshot(),
            None,
        ));
    }
}

async fn consume(
    application: &AppHandle,
    project: u32,
    event: &velocity_rosen::ProjectEvent,
) -> Result<(), String> {
    if matches!(
        event.kind,
        velocity_rosen::ProjectEventKind::PostLandFailure
            | velocity_rosen::ProjectEventKind::RevertRefused
    ) {
        // The independent repair worker reads durable evidence, even while a build runs.
        crate::rosen_landing::wake(application);
    }
    if event.kind == velocity_rosen::ProjectEventKind::Mirror {
        report(crate::rosen_mirror::refresh(application, project).await);
    }
    if event.kind == velocity_rosen::ProjectEventKind::Review {
        crate::handoff::observe_review(application, project, event)?;
    }
    let (has_evidence, work_evidence) = observe_work(application, project, event).await?;
    match velocity_rosen::change_event(event) {
        Some((change, ChangeEvent::Rebased)) => {
            crate::rosen_rebase::rebased(
                application,
                project,
                change,
                event.seq,
                &velocity_rosen::regenerate_paths(event),
            )
            .await?;
            run_follow_ups(application, project).await;
        }
        Some((change, ChangeEvent::Conflict)) => {
            crate::rosen_rebase::conflicted(
                application,
                project,
                change,
                event.seq,
                velocity_rosen::conflict_paths(event),
            )?;
            run_follow_ups(application, project).await;
        }
        Some((change, ChangeEvent::Named)) => record_named(application, project, change, event)?,
        Some((change, meaning)) => {
            let output = velocity_rosen::revert_output(event);
            take(
                application,
                project,
                (event.seq, change),
                meaning,
                output.as_deref(),
                work_evidence,
            )?;
            run_follow_ups(application, project).await;
        }
        None if has_evidence => run_follow_ups(application, project).await,
        None => {}
    }
    Ok(())
}

async fn observe_work<'a>(
    application: &AppHandle,
    project: u32,
    event: &'a velocity_rosen::ProjectEvent,
) -> Result<(bool, WorkEvidence<'a>), String> {
    use velocity_rosen::{IssueEvent, ProjectEventKind};

    if matches!(
        event.kind,
        ProjectEventKind::Built | ProjectEventKind::Green
    ) {
        crate::rosen_issue::confirm_builds(application, project, event.seq).await?;
    }
    let Some((change, issue_event)) = velocity_rosen::issue_event(event) else {
        return Ok((false, WorkEvidence::default()));
    };
    let evidence = match &issue_event {
        IssueEvent::Landed(commit) => WorkEvidence {
            commit: Some(commit),
            revert_reason: None,
        },
        IssueEvent::Reverted { reason, .. } => WorkEvidence {
            commit: None,
            revert_reason: Some(reason),
        },
        _ => WorkEvidence::default(),
    };
    crate::rosen_issue::observe(application, project, change, event.seq, issue_event)?;
    Ok((true, evidence))
}

async fn open(
    application: &AppHandle,
    project: u32,
    position: &mut (String, u64),
) -> Result<(EventStream, EventReplay, bool), String> {
    let server = crate::rosen::server(application, project).await?;
    let mut rewound = false;
    let (stream, replay) = EventStream::open_checked(&server, |head| {
        let (saved, through, reset) = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .resume_rosen_events(project, &head.identity, head.latest)
        .map_err(|error| error.to_string())?;
        rewound = reset;
        let after = if !reset && position.0 == head.identity && position.1 <= head.latest {
            saved.max(position.1)
        } else {
            saved
        };
        position.0.clone_from(&head.identity);
        position.1 = after;
        Ok((after, if after < through { through } else { 0 }))
    })
    .await?;
    Ok((stream, replay, rewound))
}

fn record_named(
    application: &AppHandle,
    project: u32,
    change: &str,
    event: &velocity_rosen::ProjectEvent,
) -> Result<(), String> {
    let commit = event
        .detail
        .get("commit")
        .and_then(|commit| commit.as_str());
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    store
        .save_rosen_named(project, event.seq, change, commit)
        .map_err(|error| error.to_string())?;
    publish_progress(application, &store)?;
    crate::rosen_landing::wake(application);
    Ok(())
}

/// Takes one event in: its thread's new state, the stream's cursor and what is left to do,
/// in one save. Returns the thread working on the change, if one does.
fn take(
    application: &AppHandle,
    project: u32,
    (seq, change): (u64, &str),
    meaning: ChangeEvent,
    output: Option<&str>,
    evidence: WorkEvidence<'_>,
) -> Result<Option<u32>, String> {
    let (progress, message) = match meaning {
        ChangeEvent::Landed => (RosenProgress::Landed, String::new()),
        ChangeEvent::Abandoned => (RosenProgress::Abandoned, String::new()),
        ChangeEvent::Reverted => {
            let report = output.unwrap_or("Rosen kept no output for the failing run.");
            let message = velocity_core::rosen_checker::reverted_message(change, report);
            (RosenProgress::Reverted, message)
        }
        // Handled by `rosen_rebase`: a rebase at the agent's step boundary, a conflict as a
        // message to it.
        ChangeEvent::Named | ChangeEvent::Rebased | ChangeEvent::Conflict => return Ok(None),
    };
    let thread = save(
        application,
        project,
        (seq, change),
        (progress, &message),
        evidence,
    )?;
    // Named work, lands, and reverts can change what the background runner can do.
    crate::rosen_landing::wake(application);
    Ok(thread)
}

/// Runs what `project`'s Rosen events left to do, oldest first per thread. A step that fails
/// stays, with the ones after it, for the next pass; nothing is dropped. One pass runs at a
/// time across projects, so two never send one message.
pub(crate) async fn run_follow_ups(application: &AppHandle, project: u32) {
    let running = application.state::<crate::rosen::ManagedRosen>();
    let _one_at_a_time = running.follow_ups.lock().await;
    let handle = application.clone();
    let pending = match tauri::async_runtime::spawn_blocking(move || {
        let pending = lock(
            handle.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .rosen_follow_ups(project);
        // Ordinary saves are deferred. Flush after reading the pending work so every
        // step we run has its cursor and retry record on disk before any side effect.
        if !pending.is_empty() {
            crate::state::save_now(&handle)?;
        }
        Ok::<_, String>(pending)
    })
    .await
    {
        Ok(Ok(pending)) => pending,
        Ok(Err(error)) => return report::<()>(Err(error)),
        Err(error) => return report::<()>(Err(error.to_string())),
    };
    for (thread, change, follow_ups) in pending {
        let key = (project, thread);
        for follow_up in follow_ups {
            match follow_up_once(application, key, change.as_deref(), &follow_up).await {
                Ok(true) => {
                    if let Err(error) = finish(application, key, &follow_up) {
                        report::<()>(Err(error));
                        break;
                    }
                }
                Ok(false) => break,
                Err(error) => {
                    report::<()>(Err(error));
                    break;
                }
            }
        }
    }
    crate::rosen_issue::follow_up(application, project).await;
}

async fn follow_up_once(
    application: &AppHandle,
    key: ThreadKey,
    change: Option<&str>,
    follow_up: &RosenFollowUp,
) -> Result<bool, String> {
    if let RosenFollowUp::Conflict { message, .. } | RosenFollowUp::Regenerate { message, .. } =
        follow_up
    {
        return deliver_rebase_notice(application, key, change, follow_up, message).await;
    }
    if matches!(
        follow_up,
        RosenFollowUp::FinishLanded | RosenFollowUp::FinishAbandoned
    ) {
        return finish_work(application, key).await;
    }
    match follow_up {
        RosenFollowUp::Reopen { message, .. } => {
            let Some(change) = change else {
                return Ok(true);
            };
            reopen_reverted(application, key, change).await?;
            deliver(application, key, follow_up, message)
        }
        RosenFollowUp::Deliver { message, .. } => deliver(application, key, follow_up, message),
        RosenFollowUp::FinishLanded
        | RosenFollowUp::FinishAbandoned
        | RosenFollowUp::Conflict { .. }
        | RosenFollowUp::Regenerate { .. } => Ok(()),
    }
    .map(|()| true)
}

/// Recheck current server state, since a queued event may already have been resolved.
/// Failures and busy turns leave the notice pending rather than steering an old basis.
async fn deliver_rebase_notice(
    application: &AppHandle,
    key: ThreadKey,
    change: Option<&str>,
    follow_up: &RosenFollowUp,
    message: &str,
) -> Result<bool, String> {
    let Some(change) = change else {
        return Ok(true);
    };
    if lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .rosen_follow_up_delivered(key.0, key.1, follow_up)
    {
        return Ok(true);
    }
    let server = crate::rosen::server(application, key.0).await?;
    if !rebase_notice_pending(&server, change, follow_up).await? {
        return Ok(true);
    }
    if !crate::rosen_rebase::apply_at_boundary(application, key).await {
        return Ok(false);
    }
    if rebase_notice_pending(&server, change, follow_up).await? {
        deliver(application, key, follow_up, message)?;
    }
    Ok(true)
}

async fn rebase_notice_pending(
    server: &velocity_rosen::ProjectServer,
    change: &str,
    follow_up: &RosenFollowUp,
) -> Result<bool, String> {
    if matches!(follow_up, RosenFollowUp::Regenerate { .. }) {
        // Generated-only rebases have no conflict records to use as a delivery gate.
        velocity_rosen::landing::is_open(server, change).await
    } else {
        velocity_rosen::has_open_conflicts(server, change).await
    }
    .map_err(|error| error.to_string())
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
    let Some(root) = workspace(application, key)? else {
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

/// Checkpoint the land reply before another checker pass can read a stale review roster.
pub(crate) fn record_landed(
    application: &AppHandle,
    project: u32,
    change: &str,
    generation: u64,
) -> Result<(), String> {
    // Keep the cursor unchanged so the event follower still takes the land. Checkpointing
    // closes the gap before the follower catches up, but only for the lifecycle generation
    // captured with this named work. A newer event or fixing state must win a delayed reply.
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    store
        .checkpoint_rosen_land(project, change, generation)
        .map_err(|error| error.to_string())?;
    publish_progress(application, &store)?;
    drop(store);
    let handle = application.clone();
    tauri::async_runtime::spawn(async move { run_follow_ups(&handle, project).await });
    Ok(())
}

/// Sends a land refusal to the change's author via the durable follow-up queue.
pub(crate) fn record_land_refusal(
    application: &AppHandle,
    project: u32,
    change: &str,
    generation: u64,
    message: &str,
) -> Result<bool, String> {
    {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        if store
            .checkpoint_rosen_land_refusal(project, change, generation, message)
            .map_err(|error| error.to_string())?
            .is_none()
        {
            return Ok(false);
        }
        report(publish_progress(application, &store));
    }
    let handle = application.clone();
    tauri::async_runtime::spawn(async move { run_follow_ups(&handle, project).await });
    Ok(true)
}

fn save(
    application: &AppHandle,
    project: u32,
    (seq, change): (u64, &str),
    progress: (RosenProgress, &str),
    evidence: WorkEvidence<'_>,
) -> Result<Option<u32>, String> {
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let thread = store
        .save_rosen_work_progress(project, seq, change, progress, evidence)
        .map_err(|error| error.to_string())?;
    publish_progress(application, &store)?;
    Ok(thread)
}

fn restore_work(application: &AppHandle, project: u32) -> Result<(), String> {
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    if store
        .restore_rosen_work(project)
        .map_err(|error| error.to_string())?
    {
        publish_progress(application, &store)?;
    }
    Ok(())
}

pub(crate) fn publish_progress(
    application: &AppHandle,
    store: &velocity_store::StateStore,
) -> Result<(), String> {
    let layout_state = application.state::<MainLayout>();
    let mut layout = lock(layout_state.inner(), "layout")?;
    store.refresh_follow_through_layout(&mut layout);
    let state = layout.snapshot();
    crate::layout::publish(application, &state, None)?;
    Ok(())
}

/// Stop the finished item's session and watcher before its worker takes new work.
/// A repeated follow-up is safe after a crash or a cleanup failure.
///
/// A thread still creating its workspace or starting its agent is left alone (#514):
/// `false` keeps the follow-up for the next pass, and the launch itself runs it again
/// once the workspace is there. Nothing is stopped or released until then.
async fn finish_work(application: &AppHandle, key: ThreadKey) -> Result<bool, String> {
    let handle = application.clone();
    let stopped = tauri::async_runtime::spawn_blocking(move || {
        if crate::threads::is_starting(&handle, key)? {
            return Ok(false);
        }
        crate::threads::stop_follow_through(&handle, key)
    })
    .await
    .map_err(|error| error.to_string())??;
    if !stopped {
        return Ok(false);
    }
    if let Some(root) = workspace(application, key)? {
        crate::rosen::stop_watch(application, &root).await;
    }
    crate::airc_claims::release(application, key)?;
    crate::goal_swarm::run_keep_going(application, key.0, key.1).await?;
    Ok(true)
}

fn workspace(application: &AppHandle, key: ThreadKey) -> Result<Option<PathBuf>, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    Ok(store
        .project(key.0)
        .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
        .and_then(|thread| thread.worktree_path.as_deref())
        .map(PathBuf::from))
}
