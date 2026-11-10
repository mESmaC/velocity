use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard};

use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use velocity_acp::client::{AgentEvent, SessionSetup};
use velocity_core::layout_view::{LayoutIntent, LayoutSnapshot};
use velocity_core::thread_view_model::TranscriptBlock;
use velocity_git::{TitledWorktree, WorktreeName};
use velocity_github::follow_through::AgentFollowUp;
use velocity_store::build_gate::{LaunchRequest, require_launch};
use velocity_store::{AgentActivity, ThreadState};
use velocity_threads::{
    DeliveryStatus, LaunchPlan, RunningSession, SessionInputs, StopOutcome, ThreadKey,
    ThreadProgress, ThreadRuntimes, agent_launch, plan_launch, resolve_title, spawn_session,
    title_worktree_name,
};

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;

pub type ManagedThreads = Mutex<ThreadRuntimes>;

pub fn action_lock(
    application: &AppHandle,
    key: ThreadKey,
) -> Result<std::sync::Arc<tokio::sync::Mutex<()>>, String> {
    Ok(lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .action_lock(key))
}

pub fn follow_through_hooks() -> crate::follow_through::Hooks {
    crate::follow_through::Hooks::new(deliver_follow_up, stop_follow_through, work_snapshot)
}

fn deliver_follow_up(
    application: &AppHandle,
    key: ThreadKey,
    pending: &AgentFollowUp,
) -> Result<DeliveryStatus, String> {
    let delivered = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .deliver_follow_up(key, &pending.identifier, &pending.message);
    match delivered {
        Ok(status) => Ok(status),
        Err(_) => resume_follow_through(application, key, pending),
    }
}

fn resume_follow_through(
    application: &AppHandle,
    key: ThreadKey,
    pending: &AgentFollowUp,
) -> Result<DeliveryStatus, String> {
    let (project, thread) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .follow_through_thread(key.0, key.1)
    .map_err(|error| error.to_string())?;
    let path = thread.worktree_path.as_deref().ok_or("Missing worktree")?;
    velocity_git::verify_thread_worktree(
        std::path::Path::new(&project.repository_path),
        std::path::Path::new(path),
        &thread.branch_name,
    )
    .map_err(|error| error.to_string())?;
    let (agent, bypass_modes) = agent_launch(thread.provider).map_err(|error| error.to_string())?;
    let setup = SessionSetup::new(path.into(), bypass_modes);
    let inputs = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .resume_follow_through(key, path, &pending.identifier, &pending.message)
    .map_err(|error| error.to_string())?;
    let updated = save_activity(application, key, AgentActivity::Thinking, None)
        .and_then(|_| refresh(application, key, false));
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        pump(&handle, key, spawn_session(agent, setup, inputs)).await;
    });
    updated?;
    Ok(DeliveryStatus::Pending)
}

fn stop_follow_through(application: &AppHandle, key: ThreadKey) -> Result<bool, String> {
    let stopped = {
        let mut runtimes = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?;
        if runtimes.is_setting_up(key) {
            return Ok(false);
        }
        runtimes.stop(key)
    };
    match stopped {
        StopOutcome::NotRunning => {}
        StopOutcome::SettingUp => return Ok(false),
        StopOutcome::Stopping(ended) => ended
            .blocking_recv()
            .map_err(|error| format!("Agent shutdown was not confirmed: {error}"))?,
    }
    if lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .shutdown_failed(key)
    {
        Err("The session could not confirm all of its processes stopped".into())
    } else {
        Ok(true)
    }
}

fn work_snapshot(
    application: &AppHandle,
    key: ThreadKey,
) -> Option<velocity_core::thread_view_model::SessionWork> {
    lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .ok()?
    .work_snapshot(key)
}

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

/// `start_thread` and `send_message` (Â§3.1): the composer's thread intents.
pub fn handle_intent(
    application: &AppHandle,
    intent: &LayoutIntent,
) -> Result<Option<LayoutSnapshot>, String> {
    match intent {
        LayoutIntent::OpenPullRequest(number) => {
            open_pull_request(application, *number).map(|_| None)
        }
        LayoutIntent::StartPrompt(prompt) => start_thread(application, prompt).map(Some),
        LayoutIntent::SteerThread {
            identifier, prompt, ..
        } => send_message(application, *identifier, prompt).map(Some),
        _ => Ok(None),
    }
}

fn open_pull_request(application: &AppHandle, number: usize) -> Result<(), String> {
    let directory =
        {
            let store = lock(
                application.state::<ManagedStateStore>().inner(),
                "persisted state",
            )?;
            let project = store.active_project().ok_or("Select a project first")?;
            if !project.threads.iter().any(|thread| {
                thread.pull_request_number.map(|value| value as usize) == Some(number)
            }) {
                return Err("The PR does not belong to a thread in this project".into());
            }
            std::path::PathBuf::from(&project.repository_path)
        };
    let number = u32::try_from(number).map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        report(
            velocity_github::PullRequestClient::discover(&directory)
                .and_then(|client| client.open_in_browser(number))
                .map_err(|error| error.to_string()),
        );
    });
    Ok(())
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
        // Settings â†’ Worktrees for the active project, which is the one the thread starts in.
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
    let inputs = {
        let mut runtimes = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?;
        let inputs = runtimes.start(key, &plan.thread.worktree_path, &plan.prompt);
        if plan.title.request.is_some() {
            runtimes.set_titling(key, Some(plan.title.model_name.clone()));
        }
        inputs
    };
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
    let (worktree_path, branch_name, failure) = match take_worktree(&application, key, &plan).await
    {
        Ok(worktree) => (
            worktree.worktree_path,
            worktree.branch_name,
            worktree.setup.err().map(|error| error.to_string()),
        ),
        Err(error) => (
            plan.worktree.worktree_path.clone(),
            plan.worktree.branch_name.clone(),
            Some(error),
        ),
    };
    let path = worktree_path.display().to_string();
    // A thread deleted while its worktree was being created starts no agent (F12).
    let wanted = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .map(|mut runtimes| {
        runtimes.rename_worktree(key, &path);
        if failure.is_some() {
            runtimes.is_live(key)
        } else {
            runtimes.worktree_ready(key)
        }
    })
    .unwrap_or(false);
    if !wanted {
        return remove_deleted_worktree(plan.worktree.repository_root, worktree_path).await;
    }
    report(record_names(
        &application,
        key,
        &path,
        branch_name.as_deref(),
    ));
    if let Some(detail) = failure {
        // The worktree is kept for a failed thread (F5).
        return fail(&application, key, "Could not set up the worktree", &detail);
    }
    if setup_no_longer_wanted(&application, key) {
        if let Ok(mut runtimes) = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        ) {
            runtimes.session_ended(key);
        }
        return;
    }
    report(refresh(&application, key, false));
    let session = spawn_session(
        plan.agent,
        SessionSetup::new(worktree_path, plan.bypass_modes),
        inputs,
    );
    pump(&application, key, session).await;
}

/// Checks the worktree out while the title model answers; naming and setup wait for the
/// title, at most F4's two seconds (`resolve_title` enforces the deadline).
async fn take_worktree(
    application: &AppHandle,
    key: ThreadKey,
    plan: &LaunchPlan,
) -> Result<TitledWorktree, String> {
    let (name_sender, name_receiver) = std::sync::mpsc::channel();
    let request = plan.worktree.clone();
    let worktree = tauri::async_runtime::spawn_blocking(move || {
        velocity_git::execute_titled_worktree_setup(&request, || {
            name_receiver.recv().ok().flatten()
        })
    });
    let name = settle_title(application, key, plan).await;
    // Setup is waiting for this; if the checkout already failed, nobody is listening.
    let _ = name_sender.send(name);
    worktree.await.map_err(|error| error.to_string())
}

/// Asks the title model, shows the title it settles on, and returns the names the worktree
/// takes from it: `None` keeps the message slug (F4).
async fn settle_title(
    application: &AppHandle,
    key: ThreadKey,
    plan: &LaunchPlan,
) -> Option<WorktreeName> {
    let title = resolve_title(plan.title.clone()).await;
    if let Ok(mut runtimes) = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    ) {
        runtimes.set_titling(key, None);
    }
    if let Some(title) = &title {
        let saved = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )
        .and_then(|mut store| {
            store
                .update_thread(key.0, key.1, |thread| thread.title.clone_from(title))
                .map_err(|error| error.to_string())
        });
        report(saved);
    }
    report(refresh(application, key, false));
    title.and_then(|title| title_worktree_name(&plan.naming, &plan.worktree, &title))
}

/// Saves the branch and folder git ended with: the title's, or the message slug's when the
/// title was missing or the rename failed.
fn record_names(
    application: &AppHandle,
    key: ThreadKey,
    path: &str,
    branch: Option<&str>,
) -> Result<bool, String> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |thread| {
        thread.worktree_path = Some(path.to_owned());
        if let Some(branch) = branch {
            branch.clone_into(&mut thread.branch_name);
        }
    })
    .map_err(|error| error.to_string())
}

fn setup_no_longer_wanted(application: &AppHandle, key: ThreadKey) -> bool {
    let merged = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .map(|store| store.thread_has_merged_pull_request(key.0, key.1))
    .unwrap_or(true);
    let live = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .map(|runtimes| runtimes.is_live(key))
    .unwrap_or(false);
    merged || !live
}

/// Applies agent events as they arrive. A burst of streamed chunks is applied together and
/// published once, so the window gets one snapshot per burst rather than one per token.
async fn pump(application: &AppHandle, key: ThreadKey, mut session: RunningSession) {
    let shutdown = session.shutdown.clone();
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
    .map(|mut runtimes| runtimes.session_ended_with_shutdown(key, shutdown.confirmed()))
    .unwrap_or(false);
    if !still_wanted {
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
        if let Some(progress) = progress
            && thread.pull_request_number.is_none()
        {
            thread.additions = progress.added;
            thread.deletions = progress.deleted;
        }
    })
    .map_err(|error| error.to_string())
}

/// The thread was deleted while its worktree was being created. Its deletion skipped the
/// worktree because setup still held it, so it is removed here once setup is done (F12).
/// Removes the worktree of a thread deleted during setup, under whatever name git left it.
async fn remove_deleted_worktree(repository: PathBuf, worktree: PathBuf) {
    let removed = tauri::async_runtime::spawn_blocking(move || {
        velocity_threads::deletion::remove_worktree(&repository, &worktree)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|result| result);
    report(removed);
}

/// The thread moves to `error` and keeps its worktree (Â§2.1, F5).
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
    crate::airc_events::record_thread_failed(application, key.0, key.1, headline, detail);
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
