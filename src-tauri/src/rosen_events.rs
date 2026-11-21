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
use velocity_store::rosen::{RosenFollowUp, RosenProgress, RosenResolutionStatus};
use velocity_store::rosen_work::WorkEvidence;
use velocity_threads::ThreadKey;

use crate::layout::MainLayout;
use crate::rosen_rebase::BoundaryOutcome;
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
                    crate::continuous_dispatch::overlap::invalidate_base_commit(project);
                    report(crate::rosen_thread::refresh(application, project, None, rewound).await);
                    let handle = application.clone();
                    let restored = tauri::async_runtime::spawn_blocking(move || {
                        restore_work(&handle, project)
                    })
                    .await
                    .map_err(|error| error.to_string())
                    .and_then(|result| result);
                    report(restored);
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
    // Drive thread view refreshes directly from Rosen events instead of polling every 5 s (#555).
    while let Some(event) = stream.next().await {
        let superseded = replay.superseded(&event);
        if let Err(error) =
            consume_connected(application, project, replay, &event, superseded).await
        {
            report::<()>(Err(error));
            break;
        }
        *after = (*after).max(event.seq);
        if superseded {
            continue;
        }
        if matches!(event.kind, Landed | Reverted) {
            crate::continuous_dispatch::overlap::invalidate_base_commit(project);
            if let Some(warm) = warm {
                let _ = warm.try_send(());
            }
        }
        match event.kind {
            Built | Green => {
                report(crate::rosen_thread::refresh_landed(application, project).await)
            }
            Landed | Reverted | Updated | Conflict | Rebased | PostLandFailure | RevertRefused
            | Review => {
                report(
                    crate::rosen_thread::refresh(
                        application,
                        project,
                        event.change.as_deref(),
                        true,
                    )
                    .await,
                );
            }
            Snapshot if velocity_rosen::change_event(&event).is_some() => {
                report(
                    crate::rosen_thread::refresh(
                        application,
                        project,
                        event.change.as_deref(),
                        true,
                    )
                    .await,
                );
            }
            _ => {}
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
        let sequence = event.seq;
        crate::state::try_with_store(application, move |store| {
            store
                .save_rosen_cursor(project, sequence)
                .map_err(|error| error.to_string())
        })
        .await?;
    }
    Ok(())
}

fn connection_state(application: &AppHandle, project: u32, reconnecting: bool) {
    let snapshot = lock(application.state::<MainLayout>().inner(), "layout")
        .ok()
        .and_then(|mut layout| {
            layout
                .set_rosen_reconnecting(project, reconnecting)
                .then(|| layout.snapshot())
        });
    if let Some(snapshot) = snapshot {
        report(crate::layout::publish(application, &snapshot, None));
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
    if matches!(
        event.kind,
        velocity_rosen::ProjectEventKind::Claim | velocity_rosen::ProjectEventKind::Released
    ) {
        observe_path_wait(application, project, event).await?;
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
            let handle = application.clone();
            let change = change.to_owned();
            let sequence = event.seq;
            let paths = velocity_rosen::conflict_paths(event);
            tauri::async_runtime::spawn_blocking(move || {
                crate::rosen_rebase::conflicted(&handle, project, &change, sequence, paths)
            })
            .await
            .map_err(|error| error.to_string())??;
            run_follow_ups(application, project).await;
        }
        Some((change, ChangeEvent::Named)) => {
            let handle = application.clone();
            let change = change.to_owned();
            let sequence = event.seq;
            let commit = event
                .detail
                .get("commit")
                .and_then(|commit| commit.as_str())
                .map(str::to_owned);
            tauri::async_runtime::spawn_blocking(move || {
                record_named(&handle, project, &change, sequence, commit.as_deref())
            })
            .await
            .map_err(|error| error.to_string())??;
        }
        Some((change, meaning)) => {
            let handle = application.clone();
            let change = change.to_owned();
            let sequence = event.seq;
            let output = velocity_rosen::revert_output(event);
            let commit = work_evidence.commit.map(str::to_owned);
            let revert_reason = work_evidence.revert_reason.map(str::to_owned);
            tauri::async_runtime::spawn_blocking(move || {
                let evidence = WorkEvidence {
                    commit: commit.as_deref(),
                    revert_reason: revert_reason.as_deref(),
                };
                take(
                    &handle,
                    project,
                    (sequence, &change),
                    meaning,
                    output.as_deref(),
                    evidence,
                )
            })
            .await
            .map_err(|error| error.to_string())??;
            run_follow_ups(application, project).await;
        }
        None if has_evidence => run_follow_ups(application, project).await,
        None => {}
    }
    Ok(())
}

async fn open_rosen_thread(
    application: &AppHandle,
    project: u32,
    change: &str,
) -> Option<ThreadKey> {
    let change = change.to_owned();
    crate::state::with_store(application, move |store| {
        store
            .project(project)?
            .threads
            .iter()
            .find(|thread| {
                thread.rosen_change.as_deref() == Some(change.as_str())
                    && matches!(
                        thread.state,
                        velocity_store::ThreadState::Working | velocity_store::ThreadState::Review
                    )
            })
            .map(|thread| (project, thread.id))
    })
    .await
    .ok()
    .flatten()
}

/// Tracks Rosen path waits and wake releases on the owning thread so Velocity can park
/// once the turn ends and never miss a release that arrives before turn end (#531).
async fn observe_path_wait(
    application: &AppHandle,
    project: u32,
    event: &velocity_rosen::ProjectEvent,
) -> Result<(), String> {
    let Some(change) = event.change.as_deref() else {
        return Ok(());
    };
    let Some(key) = open_rosen_thread(application, project, change).await else {
        return Ok(());
    };
    match event.kind {
        velocity_rosen::ProjectEventKind::Claim
            if event.detail.get("kind").and_then(|value| value.as_str()) == Some("wait") =>
        {
            let paths = event_strings(&event.detail, "paths");
            let holders = overlap_owners(&event.detail);
            let handle = application.clone();
            tauri::async_runtime::spawn_blocking(move || {
                record_rosen_wait(&handle, key, paths, holders)
            })
            .await
            .map_err(|error| error.to_string())??;
        }
        velocity_rosen::ProjectEventKind::Released => {
            let is_wake = event.detail.get("wake").and_then(|value| value.as_bool()) == Some(true);
            if is_wake {
                let path = event
                    .detail
                    .get("path")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned);
                let released_by = event
                    .detail
                    .get("released_by")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned);
                let handle = application.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    record_rosen_wake(&handle, key, path.as_deref(), released_by.as_deref())
                })
                .await
                .map_err(|error| error.to_string())??;
            } else if let Ok(server) = crate::rosen::server(application, project).await
                && let Ok(active) = velocity_rosen::mcp_proxy::active_waits(&server, change).await
                && active.is_empty()
            {
                let handle = application.clone();
                tauri::async_runtime::spawn_blocking(move || clear_rosen_wait(&handle, key))
                    .await
                    .map_err(|error| error.to_string())??;
            }
        }
        _ => {}
    }
    Ok(())
}

fn event_strings(detail: &serde_json::Value, key: &str) -> Vec<String> {
    detail
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

fn overlap_owners(detail: &serde_json::Value) -> Vec<String> {
    detail
        .get("overlaps")
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("owner").and_then(|value| value.as_str()))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

fn record_rosen_wait(
    application: &AppHandle,
    key: ThreadKey,
    paths: Vec<String>,
    holders: Vec<String>,
) -> Result<(), String> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |saved| {
        let wait = saved
            .rosen_path_wait
            .get_or_insert_with(velocity_store::rosen::RosenPathWait::default);
        wait.merge(paths, holders);
        saved.dependency_wait = Some(wait.description());
    })
    .map_err(|error| error.to_string())?;
    crate::threads::refresh(application, key, false)?;
    Ok(())
}

fn record_rosen_wake(
    application: &AppHandle,
    key: ThreadKey,
    path: Option<&str>,
    released_by: Option<&str>,
) -> Result<(), String> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |saved| {
        if let Some(wait) = saved.rosen_path_wait.as_mut() {
            wait.record_wake(path, released_by);
        }
    })
    .map_err(|error| error.to_string())?;
    Ok(())
}

fn clear_rosen_wait(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    let mut cleared = false;
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |saved| {
        if saved.rosen_path_wait.take().is_some() {
            saved.dependency_wait = None;
            cleared = true;
        }
    })
    .map_err(|error| error.to_string())?;
    if cleared {
        crate::threads::refresh(application, key, false)?;
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
        IssueEvent::Reverted { commit, reason } => WorkEvidence {
            commit: *commit,
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
    sequence: u64,
    commit: Option<&str>,
) -> Result<(), String> {
    let refresh = {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        store
            .save_rosen_named(project, sequence, change, commit)
            .map_err(|error| error.to_string())?;
        store.layout_refresh()
    };
    publish_progress(application, refresh)?;
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

enum FollowUpStep {
    Finished,
    Waiting(String),
    WorkspaceMissing(String),
    Failed(String),
}

/// Runs what `project`'s Rosen events left to do, oldest first per thread. A step that fails
/// stays, with the ones after it, for the next pass; nothing is dropped. One pass runs at a
/// time across projects, so two never send one message.
pub(crate) async fn run_follow_ups(application: &AppHandle, project: u32) {
    let running = application.state::<crate::rosen::ManagedRosen>();
    let _one_at_a_time = running.follow_ups.lock().await;
    let pending = match load_and_flush_follow_ups(application, project).await {
        Ok(pending) => pending,
        Err(error) => return report::<()>(Err(error)),
    };
    for (thread, change, follow_ups) in pending {
        run_thread_follow_ups(
            application,
            (project, thread),
            change.as_deref(),
            &follow_ups,
            false,
        )
        .await;
    }
    crate::rosen_issue::follow_up(application, project).await;
}

/// Immediately attempts to deliver `key`'s queued conflict resolution inside the follow-up lock,
/// updating the thread notice and Stored conflicts card with the live outcome (#522).
pub(crate) async fn deliver_conflict_now(
    application: &AppHandle,
    key: ThreadKey,
    already_pending: bool,
) -> Result<(), String> {
    let running = application.state::<crate::rosen::ManagedRosen>();
    let _one_at_a_time = running.follow_ups.lock().await;
    let pending = load_and_flush_follow_ups(application, key.0).await?;
    if let Some((_, change, follow_ups)) = pending.into_iter().find(|(id, _, _)| *id == key.1) {
        run_thread_follow_ups(
            application,
            key,
            change.as_deref(),
            &follow_ups,
            already_pending,
        )
        .await;
    }
    Ok(())
}

async fn load_and_flush_follow_ups(
    application: &AppHandle,
    project: u32,
) -> Result<Vec<(u32, Option<String>, Vec<RosenFollowUp>)>, String> {
    let handle = application.clone();
    tauri::async_runtime::spawn_blocking(move || {
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
    .map_err(|error| error.to_string())?
}

async fn run_thread_follow_ups(
    application: &AppHandle,
    key: ThreadKey,
    change: Option<&str>,
    follow_ups: &[RosenFollowUp],
    already_pending_click: bool,
) {
    for (index, follow_up) in follow_ups.iter().enumerate() {
        let has_conflict = follow_ups[index..]
            .iter()
            .any(|item| matches!(item, RosenFollowUp::Conflict { .. }));
        match follow_up_once(application, key, change, follow_up).await {
            FollowUpStep::Finished => {
                let handle = application.clone();
                let done = follow_up.clone();
                let finished =
                    tauri::async_runtime::spawn_blocking(move || finish(&handle, key, &done))
                        .await
                        .map_err(|error| error.to_string())
                        .and_then(|result| result);
                if let Err(error) = finished {
                    report::<()>(Err(error));
                    break;
                }
            }
            step => {
                let handle = application.clone();
                let item = follow_up.clone();
                let _ = tauri::async_runtime::spawn_blocking(move || {
                    record_follow_up_stop(
                        &handle,
                        key,
                        &item,
                        step,
                        has_conflict,
                        already_pending_click,
                    );
                })
                .await;
                break;
            }
        }
    }
}

fn record_follow_up_stop(
    application: &AppHandle,
    key: ThreadKey,
    follow_up: &RosenFollowUp,
    step: FollowUpStep,
    has_conflict: bool,
    already_pending_click: bool,
) {
    match step {
        FollowUpStep::Finished => {}
        FollowUpStep::Waiting(reason) => {
            if has_conflict {
                let status = if already_pending_click {
                    format!("Resolution already queued · {reason}")
                } else {
                    format!("Resolution queued · sent at the next step boundary · {reason}")
                };
                update_resolution_feedback(
                    application,
                    key,
                    Some(RosenResolutionStatus::new(status, false)),
                    Some(None),
                );
            }
        }
        FollowUpStep::WorkspaceMissing(reason) => {
            let notice = follow_up_error_notice(follow_up, &reason);
            let card = has_conflict.then(|| {
                let status = if already_pending_click {
                    format!("Resolution already queued · {reason}")
                } else {
                    format!("Can't resolve here · {reason}")
                };
                RosenResolutionStatus::new(status, true)
            });
            update_resolution_feedback(application, key, card, Some(Some(notice)));
            report::<()>(Err(reason));
        }
        FollowUpStep::Failed(error) => {
            let notice = follow_up_error_notice(follow_up, &error);
            let card = has_conflict.then(|| {
                let status = if already_pending_click {
                    format!("Resolution already queued · delivery failed: {error}")
                } else {
                    format!("Resolution queued · delivery failed: {error}")
                };
                RosenResolutionStatus::new(status, false)
            });
            update_resolution_feedback(application, key, card, Some(Some(notice)));
            report::<()>(Err(error));
        }
    }
}

fn follow_up_error_notice(follow_up: &RosenFollowUp, error: &str) -> String {
    let action = match follow_up {
        RosenFollowUp::Conflict { .. } => "deliver Rosen conflict resolution",
        RosenFollowUp::Regenerate { .. } => "deliver Rosen regeneration notice",
        RosenFollowUp::Reopen { .. } => "reopen reverted Rosen change",
        RosenFollowUp::Deliver { .. } => "deliver Rosen update",
        RosenFollowUp::FinishLanded | RosenFollowUp::FinishAbandoned => {
            "finish closed Rosen thread"
        }
    };
    format!("Could not {action}: {error}")
}

fn update_resolution_feedback(
    application: &AppHandle,
    key: ThreadKey,
    resolution: Option<RosenResolutionStatus>,
    follow_up_error: Option<Option<String>>,
) {
    let refresh = {
        let Ok(mut store) = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        ) else {
            return;
        };
        let mut changed = false;
        if let Some(status) = resolution {
            match store.set_rosen_resolution_status(key.0, key.1, Some(status)) {
                Ok(updated) => changed |= updated,
                Err(error) => report::<()>(Err(error.to_string())),
            }
        }
        if let Some(error) = follow_up_error {
            match store.set_rosen_follow_up_error(key.0, key.1, error) {
                Ok(updated) => changed |= updated,
                Err(error) => report::<()>(Err(error.to_string())),
            }
        }
        changed.then(|| store.layout_refresh())
    };
    if let Some(refresh) = refresh {
        report(publish_progress(application, refresh));
    }
}

async fn follow_up_once(
    application: &AppHandle,
    key: ThreadKey,
    change: Option<&str>,
    follow_up: &RosenFollowUp,
) -> FollowUpStep {
    if let RosenFollowUp::Conflict { message, .. } | RosenFollowUp::Regenerate { message, .. } =
        follow_up
    {
        return deliver_rebase_notice(application, key, change, follow_up, message).await;
    }
    if matches!(
        follow_up,
        RosenFollowUp::FinishLanded | RosenFollowUp::FinishAbandoned
    ) {
        if matches!(follow_up, RosenFollowUp::FinishLanded) {
            tick_spec_section_on_land(application, key).await;
        }
        return match finish_work(application, key).await {
            Ok(true) => FollowUpStep::Finished,
            Ok(false) => FollowUpStep::Waiting("waiting for workspace setup to finish".into()),
            Err(error) => FollowUpStep::Failed(error),
        };
    }
    let result = match follow_up {
        RosenFollowUp::Reopen { message, .. } => {
            let Some(change) = change else {
                return FollowUpStep::Finished;
            };
            match reopen_reverted(application, key, change).await {
                Ok(()) => {
                    let message =
                        with_revert_fix_forward_brief(application, key, change, message).await;
                    deliver(application, key, follow_up, &message)
                }
                Err(error) => Err(error),
            }
        }
        RosenFollowUp::Deliver { message, .. } => deliver(application, key, follow_up, message),
        RosenFollowUp::FinishLanded
        | RosenFollowUp::FinishAbandoned
        | RosenFollowUp::Conflict { .. }
        | RosenFollowUp::Regenerate { .. } => Ok(()),
    };
    match result {
        Ok(()) => FollowUpStep::Finished,
        Err(error) => FollowUpStep::Failed(error),
    }
}

/// Recheck current server state, since a queued event may already have been resolved.
/// Failures and busy turns leave the notice pending rather than steering an old basis.
async fn deliver_rebase_notice(
    application: &AppHandle,
    key: ThreadKey,
    change: Option<&str>,
    follow_up: &RosenFollowUp,
    message: &str,
) -> FollowUpStep {
    let Some(change) = change else {
        return FollowUpStep::Finished;
    };
    match lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) {
        Ok(store) if store.rosen_follow_up_delivered(key.0, key.1, follow_up) => {
            return FollowUpStep::Finished;
        }
        Ok(_) => {}
        Err(error) => return FollowUpStep::Failed(error),
    }
    let server = match crate::rosen::server(application, key.0).await {
        Ok(server) => server,
        Err(error) => return FollowUpStep::Failed(error),
    };
    match rebase_notice_pending(&server, change, follow_up).await {
        Ok(false) => {
            report(crate::rosen_thread::refresh(application, key.0, Some(change), true).await);
            return FollowUpStep::Finished;
        }
        Ok(true) => {}
        Err(error) => return FollowUpStep::Failed(error),
    }
    match crate::rosen_rebase::prepare_at_boundary(application, key, Some(change), true).await {
        BoundaryOutcome::Ready => {}
        BoundaryOutcome::Waiting(reason) => return FollowUpStep::Waiting(reason),
        BoundaryOutcome::WorkspaceMissing(reason) => {
            return FollowUpStep::WorkspaceMissing(reason);
        }
        BoundaryOutcome::Failed(error) => return FollowUpStep::Failed(error),
    }
    match rebase_notice_pending(&server, change, follow_up).await {
        Ok(false) => {
            report(crate::rosen_thread::refresh(application, key.0, Some(change), true).await);
            FollowUpStep::Finished
        }
        Ok(true) => match deliver(application, key, follow_up, message) {
            Ok(()) => FollowUpStep::Finished,
            Err(error) => FollowUpStep::Failed(error),
        },
        Err(error) => FollowUpStep::Failed(error),
    }
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
    let refresh = {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        store
            .finish_rosen_follow_up(key.0, key.1, done)
            .map_err(|error| error.to_string())?;
        store.layout_refresh()
    };
    publish_progress(application, refresh)
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
    let Some(root) = workspace(application, key).await? else {
        return Ok(());
    };
    let server = crate::rosen::server(application, key.0).await?;
    let agent = crate::rosen::thread_agent(application, key, &root);
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
    let refresh = {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        store
            .checkpoint_rosen_land(project, change, generation)
            .map_err(|error| error.to_string())?;
        store.layout_refresh()
    };
    publish_progress(application, refresh)?;
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
    let refresh = {
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
        store.layout_refresh()
    };
    report(publish_progress(application, refresh));
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
    let (thread, refresh) = {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let thread = store
            .save_rosen_work_progress(project, seq, change, progress, evidence)
            .map_err(|error| error.to_string())?;
        (thread, store.layout_refresh())
    };
    publish_progress(application, refresh)?;
    Ok(thread)
}

fn restore_work(application: &AppHandle, project: u32) -> Result<(), String> {
    let refresh = {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        store
            .restore_rosen_work(project)
            .map_err(|error| error.to_string())?
            .then(|| store.layout_refresh())
    };
    if let Some(refresh) = refresh {
        publish_progress(application, refresh)?;
    }
    Ok(())
}

pub(crate) fn publish_progress(
    application: &AppHandle,
    refresh: velocity_store::projects::LayoutRefresh,
) -> Result<(), String> {
    crate::layout::apply_refresh_and_publish(application, refresh)
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
    if let Some(root) = workspace(application, key).await? {
        crate::rosen::stop_watch(application, &root).await;
    }
    crate::airc_claims::release(application, key)?;
    crate::goal_swarm::run_keep_going(application, key.0, key.1).await?;
    Ok(true)
}

async fn workspace(application: &AppHandle, key: ThreadKey) -> Result<Option<PathBuf>, String> {
    crate::state::with_store(application, move |store| {
        store
            .project(key.0)
            .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
            .and_then(|thread| thread.worktree_path.as_deref())
            .map(PathBuf::from)
    })
    .await
}

/// Ticks the owning §7 implementation-order item in Rosen when a change lands (#539).
async fn tick_spec_section_on_land(application: &AppHandle, key: ThreadKey) {
    let target = crate::state::with_store(application, move |store| {
        let project = store.project(key.0)?;
        let thread = project.threads.iter().find(|thread| thread.id == key.1)?;
        let issue = thread.issue.as_ref().map(|issue| issue.number);
        let section = thread.spec_section.clone();
        let title = thread.title.clone();
        let label = issue
            .map(|number| format!("#{number}"))
            .or_else(|| section.clone())
            .unwrap_or_else(|| thread.agent_name());
        Some((project.spec_path.clone(), section, issue, title, label))
    })
    .await
    .ok()
    .flatten();
    let Some((spec_path, section, issue, title, label)) = target else {
        return;
    };
    let Ok(server) = crate::rosen::server(application, key.0).await else {
        return;
    };
    let query = velocity_rosen::spec_tick::TickQuery {
        spec_section: section.as_deref(),
        issue_number: issue,
        title: Some(title.as_str()),
    };
    if let Err(error) =
        velocity_rosen::spec_tick::tick_on_land(&server, &spec_path, &query, &label).await
    {
        report::<()>(Err(format!(
            "Could not tick §7 item for {label} on land: {error}"
        )));
    }
}

/// Appends the structured fix-forward task brief (including landed head and failed check)
/// when a reverted Rosen change is reopened for its author (#539).
async fn with_revert_fix_forward_brief(
    application: &AppHandle,
    key: ThreadKey,
    change: &str,
    message: &str,
) -> String {
    let Ok(app_data) = application.path().app_data_dir() else {
        return message.to_owned();
    };
    let context = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()
    .and_then(|store| {
        let project = store.project(key.0)?.clone();
        let thread = project
            .threads
            .iter()
            .find(|thread| thread.id == key.1)?
            .clone();
        let checkout = thread
            .worktree_path
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(&project.repository_path));
        Some((project, thread, checkout))
    });
    let Some((project, thread, checkout)) = context else {
        return message.to_owned();
    };
    let landed_head = thread
        .rosen_last_reverted_commit
        .as_deref()
        .unwrap_or("unknown");
    let input = velocity_threads::task_brief::revert_brief_input(
        &project,
        &thread,
        change,
        landed_head,
        message,
        &checkout,
    );
    let brief = velocity_threads::task_brief::build_brief_async(&app_data, &project, input).await;
    format!("{}\n\n{message}", brief.render(3_800))
}
