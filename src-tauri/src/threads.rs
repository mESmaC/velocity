use std::sync::{Mutex, MutexGuard};

use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use velocity_acp::client::{AgentEvent, SessionSetup};
use velocity_core::layout_view::{LayoutIntent, LayoutSnapshot};
use velocity_core::thread_view_model::TranscriptBlock;
use velocity_store::build_gate::{LaunchRequest, require_launch};
use velocity_store::{AgentActivity, ThreadState};
use velocity_threads::{
    LaunchPlan, RunningSession, SessionInputs, ThreadKey, ThreadProgress, ThreadRuntimes,
    plan_launch, spawn_session,
};

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;

pub type ManagedThreads = Mutex<ThreadRuntimes>;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type, Event)]
#[tauri_specta(event_name = "thread://status")]
pub struct ThreadStatusChanged {
    pub project_identifier: u32,
    pub thread_identifier: u32,
    pub state: ThreadState,
    pub activity: AgentActivity,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type, Event)]
#[tauri_specta(event_name = "thread://step")]
pub struct ThreadStepRecorded {
    pub project_identifier: u32,
    pub thread_identifier: u32,
    /// The thread's newest transcript block after the update.
    pub block: TranscriptBlock,
}

pub(crate) fn lock<'guard, Value>(
    mutex: &'guard Mutex<Value>,
    what: &str,
) -> Result<MutexGuard<'guard, Value>, String> {
    mutex
        .lock()
        .map_err(|error| format!("Could not read {what}: {error}"))
}

/// `start_thread` and `send_message` (§3.1): the composer's thread intents.
pub fn handle_intent(
    application: &AppHandle,
    intent: &LayoutIntent,
) -> Result<Option<LayoutSnapshot>, String> {
    match intent {
        LayoutIntent::StartPrompt(prompt) => start_thread(application, prompt).map(Some),
        LayoutIntent::SteerThread {
            identifier, prompt, ..
        } => send_message(application, *identifier, prompt).map(Some),
        _ => Ok(None),
    }
}

/// Records the thread, shows it, and starts its worktree and agent in the background (F4).
fn start_thread(application: &AppHandle, prompt: &str) -> Result<LayoutSnapshot, String> {
    let store_state = application.state::<ManagedStateStore>();
    let (plan, thread) = {
        let mut store = lock(store_state.inner(), "persisted state")?;
        // The layout guard already checked; effectful handlers check afresh (Rule 2).
        require_launch(store.state(), LaunchRequest::Thread).map_err(|error| error.to_string())?;
        let project = store
            .active_project()
            .ok_or("Select or create a project first")?;
        // Settings → Worktrees for the active project, which is the one the thread starts in.
        let worktrees = store.worktree_context().preferences;
        let plan = plan_launch(project, &worktrees, &store.settings(), prompt)
            .map_err(|error| error.to_string())?;
        let thread = store
            .add_thread(plan.project, plan.thread.clone())
            .map_err(|error| error.to_string())?;
        (plan, thread)
    };
    start_planned(application, plan, thread)
}

/// F9 uses the same unrestricted worktree/session lifecycle after atomically
/// recording an issue's owning thread.
pub(crate) fn start_planned(
    application: &AppHandle,
    plan: LaunchPlan,
    thread: u32,
) -> Result<LayoutSnapshot, String> {
    let key = (plan.project, thread);
    let inputs = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .start(key, &plan.thread.worktree_path, &plan.prompt);
    let snapshot = refresh(application, key, true)?;
    emit_status(application, key);
    let handle = application.clone();
    tauri::async_runtime::spawn(async move { run_thread(handle, key, plan, inputs).await });
    Ok(snapshot)
}

/// A message for a running thread: sent at once when idle, queued while busy (Rule 1).
fn send_message(
    application: &AppHandle,
    identifier: usize,
    prompt: &str,
) -> Result<LayoutSnapshot, String> {
    let project = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .active_project()
    .map(|project| project.id)
    .ok_or("Select or create a project first")?;
    let thread = u32::try_from(identifier).map_err(|error| error.to_string())?;
    let key = (project, thread);
    lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .steer(key, prompt)
    .map_err(|error| error.to_string())?;
    save_activity(application, key, AgentActivity::Thinking, None)?;
    let snapshot = refresh(application, key, false)?;
    emit_status(application, key);
    Ok(snapshot)
}

async fn run_thread(
    application: AppHandle,
    key: ThreadKey,
    plan: LaunchPlan,
    inputs: SessionInputs,
) {
    let request = plan.worktree.clone();
    let created = tauri::async_runtime::spawn_blocking(move || {
        velocity_git::execute_worktree_setup(&request)
    })
    .await;
    let failure = match created {
        Ok(Ok(_)) => None,
        Ok(Err(error)) => Some(error.to_string()),
        Err(error) => Some(error.to_string()),
    };
    // A thread deleted while its worktree was being created starts no agent (F12).
    let wanted = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .map(|mut runtimes| {
        if failure.is_some() {
            runtimes.is_live(key)
        } else {
            runtimes.worktree_ready(key)
        }
    })
    .unwrap_or(false);
    if !wanted {
        return remove_deleted_worktree(plan).await;
    }
    if let Some(detail) = failure {
        // The worktree is kept for a failed thread (F5).
        return fail(&application, key, "Could not set up the worktree", &detail);
    }
    report(refresh(&application, key, false));
    let session = spawn_session(
        plan.agent,
        SessionSetup {
            worktree: plan.worktree.worktree_path,
            airc: None,
            shared_servers: Vec::new(),
            bypass_modes: plan.bypass_modes,
        },
        inputs,
    );
    pump(&application, key, session).await;
}

/// Applies agent events as they arrive. A burst of streamed chunks is applied together and
/// published once, so the window gets one snapshot per burst rather than one per token.
async fn pump(application: &AppHandle, key: ThreadKey, mut session: RunningSession) {
    while let Some(event) = session.events.recv().await {
        let mut batch = Vec::from([event]);
        while let Ok(next) = session.events.try_recv() {
            batch.push(next);
        }
        report(apply_batch(application, key, &batch));
    }
    let outcome = session.outcome.await;
    let still_wanted = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .map(|mut runtimes| runtimes.session_ended(key))
    .unwrap_or(false);
    if !still_wanted {
        // Deleting the thread stopped it; the deletion takes it from here (F12).
        return;
    }
    match outcome {
        Ok(Ok(())) => report(
            save_activity(application, key, AgentActivity::Idle, None)
                .and_then(|_| refresh(application, key, false).map(|_| ())),
        ),
        Ok(Err(error)) => fail(application, key, "The agent stopped", &error),
        Err(_) => fail(
            application,
            key,
            "The agent stopped",
            "Its session ended unexpectedly.",
        ),
    }
}

fn apply_batch(
    application: &AppHandle,
    key: ThreadKey,
    batch: &[AgentEvent],
) -> Result<(), String> {
    let progress: Option<ThreadProgress> = {
        let mut runtimes = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?;
        batch
            .iter()
            .filter_map(|event| runtimes.apply(key, event))
            .last()
    };
    let saved = match progress {
        Some(progress) => save_activity(application, key, progress.activity, Some(progress))?,
        None => false,
    };
    refresh(application, key, false)?;
    if saved {
        emit_status(application, key);
    }
    if batch
        .iter()
        .any(|event| matches!(event, AgentEvent::Transcript { .. }))
    {
        emit_step(application, key);
    }
    Ok(())
}

/// Saves the thread's activity, and its line counts when given; `true` when anything changed.
fn save_activity(
    application: &AppHandle,
    key: ThreadKey,
    activity: AgentActivity,
    progress: Option<ThreadProgress>,
) -> Result<bool, String> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |thread| {
        thread.activity = activity;
        if let Some(progress) = progress {
            thread.additions = progress.added;
            thread.deletions = progress.deleted;
        }
    })
    .map_err(|error| error.to_string())
}

/// The thread was deleted while its worktree was being created. Its deletion skipped the
/// worktree because setup still held it, so it is removed here once setup is done (F12).
async fn remove_deleted_worktree(plan: LaunchPlan) {
    let request = plan.worktree;
    let removed = tauri::async_runtime::spawn_blocking(move || {
        velocity_threads::deletion::remove_worktree(
            &request.repository_root,
            &request.worktree_path,
        )
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|result| result);
    report(removed);
}

/// The thread moves to `error` and keeps its worktree (§2.1, F5).
fn fail(application: &AppHandle, key: ThreadKey, headline: &str, detail: &str) {
    if let Ok(mut runtimes) = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    ) {
        runtimes.fail(key, headline, detail);
    }
    let saved = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .and_then(|mut store| {
        store
            .update_thread(key.0, key.1, |thread| {
                thread.state = ThreadState::Error;
                thread.activity = AgentActivity::Idle;
            })
            .map_err(|error| error.to_string())
    });
    report(saved.map(|_| ()));
    report(refresh(application, key, false).map(|_| ()));
    emit_status(application, key);
}

/// Rebuilds the window from saved state plus the live transcript and publishes it.
fn refresh(
    application: &AppHandle,
    key: ThreadKey,
    select: bool,
) -> Result<LayoutSnapshot, String> {
    let view = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .view(key);
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let mut layout = lock(
        application.state::<MainLayout>().inner(),
        "main-window state",
    )?;
    let identifier = usize::try_from(key.1).map_err(|error| error.to_string())?;
    store.refresh_layout(&mut layout);
    if let Some(view) = view {
        layout.set_live_thread(key.0, identifier, view);
    }
    if select
        && store
            .active_project()
            .is_some_and(|project| project.id == key.0)
    {
        layout.select_thread(identifier);
    }
    let snapshot = layout.snapshot();
    // Publish under the locks, as dispatch does, so snapshots reach the window in order.
    crate::layout::publish(application, &snapshot, None)?;
    Ok(snapshot)
}

fn emit_status(application: &AppHandle, key: ThreadKey) {
    let Ok(store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) else {
        return;
    };
    let Some(thread) = store
        .project(key.0)
        .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
    else {
        return;
    };
    let event = ThreadStatusChanged {
        project_identifier: key.0,
        thread_identifier: key.1,
        state: thread.state,
        activity: thread.activity,
    };
    drop(store);
    report(event.emit(application).map_err(|error| error.to_string()));
}

fn emit_step(application: &AppHandle, key: ThreadKey) {
    let block = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .ok()
    .and_then(|runtimes| runtimes.view(key))
    .and_then(|view| view.blocks.last().cloned());
    if let Some(block) = block {
        let event = ThreadStepRecorded {
            project_identifier: key.0,
            thread_identifier: key.1,
            block,
        };
        report(event.emit(application).map_err(|error| error.to_string()));
    }
}

/// Background work has no caller to return an error to; the log is where it goes.
pub(crate) fn report<Value>(result: Result<Value, String>) {
    if let Err(error) = result {
        eprintln!("Velocity thread runtime: {error}");
    }
}
