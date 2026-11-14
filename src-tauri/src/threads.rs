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
use velocity_store::duty::clock_timestamp;
use velocity_store::goal_swarms::GoalAgentSlot;
use velocity_store::{AgentActivity, ThreadState};
use velocity_threads::goal_swarm::{
    ClaimPhaseOutcome, ClaimedGoalWork, GoalMemberPlan, choose_and_claim,
};
use velocity_threads::{
    DeliveryStatus, LaunchPlan, RunningSession, SessionInputs, StopOutcome, ThreadKey,
    ThreadProgress, ThreadRuntimes, agent_launch, plan_launch_with_preferences, resolve_title,
    spawn_session, title_worktree_name,
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

/// The thread's saved model, effort and speed applied to its session (F15). A thread the store
/// no longer has keeps the agent's own defaults.
fn with_run_settings(application: &AppHandle, key: ThreadKey, setup: SessionSetup) -> SessionSetup {
    let settings = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()
    .and_then(|store| {
        let project = store
            .state()
            .projects
            .iter()
            .find(|item| item.id == key.0)?;
        let thread = project.threads.iter().find(|item| item.id == key.1)?;
        Some(velocity_threads::thread_run_settings(thread))
    });
    match settings {
        Some(settings) => setup.with_run_settings(settings),
        None => setup,
    }
}

fn resume_follow_through(
    application: &AppHandle,
    key: ThreadKey,
    pending: &AgentFollowUp,
) -> Result<DeliveryStatus, String> {
    let (project, thread, settings) = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let (project, thread) = store
            .follow_through_thread(key.0, key.1)
            .map_err(|error| error.to_string())?;
        (project, thread, store.settings())
    };
    if thread.activity == AgentActivity::AtLimit {
        return Ok(DeliveryStatus::Pending);
    }
    let path = thread.worktree_path.as_deref().ok_or("Missing worktree")?;
    velocity_git::verify_thread_worktree(
        std::path::Path::new(&project.repository_path),
        std::path::Path::new(path),
        &thread.branch_name,
    )
    .map_err(|error| error.to_string())?;
    let (agent, bypass_modes) =
        agent_launch(&settings, thread.provider).map_err(|error| error.to_string())?;
    let mut setup = SessionSetup::new(path.into(), bypass_modes)
        .with_run_settings(velocity_threads::thread_run_settings(&thread));
    setup.airc = crate::airc_access::managed(application)
        .map(|airc| airc.thread(key.0, key.1, std::path::Path::new(path)));
    setup.resume = Some(
        velocity_acp::client::SessionResume::new(
            thread.agent_session.clone(),
            velocity_threads::parking::transcript_context(&thread.transcript),
        )
        .with_history_replay(thread.transcript.is_empty()),
    );
    crate::airc_claims::attach(application, key, &setup, &thread.branch_name)?;
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
        // Bounded: the watcher runs every project's follow-through on one thread, so a session
        // that never reports its end must not stop every other PR from being followed (#278).
        // A timed-out wait is a cleanup error, retried on the next pass. The timer is made
        // inside the runtime: this watcher thread is not one of its workers.
        StopOutcome::Stopping(ended) => tauri::async_runtime::block_on(async move {
            tokio::time::timeout(velocity_threads::deletion::AGENT_EXIT_WAIT, ended).await
        })
        .map_err(|_| "The agent is still stopping; its worktree was kept".to_owned())?
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

/// `start_thread` and `send_message` (§3.1): the composer's thread intents.
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
        } => {
            let project = lock(
                application.state::<ManagedStateStore>().inner(),
                "persisted state",
            )?
            .active_project()
            .map(|project| project.id)
            .ok_or("Select or create a project first")?;
            deliver_message(application, project, *identifier, prompt).map(Some)
        }
        LayoutIntent::RemoveQueuedMessage { identifier, index } => {
            let project = lock(
                application.state::<ManagedStateStore>().inner(),
                "persisted state",
            )?
            .active_project()
            .map(|project| project.id)
            .ok_or("Select a project first")?;
            remove_queued_message(application, project, *identifier, *index).map(Some)
        }
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
        // Settings → Worktrees for the active project, which is the one the thread starts in.
        let worktrees = store.worktree_context().preferences;
        let plan = plan_launch_with_preferences(project, &worktrees, &store.settings(), prompt)
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

/// Registers a goal swarm member in `choosing work` and spawns its worktree + AIRC claim
/// lifecycle in the background (`SPEC.md` F16).
pub(crate) fn start_goal_member(
    application: &AppHandle,
    plan: GoalMemberPlan,
    thread: u32,
) -> Result<(), String> {
    let key = (plan.project_id, thread);
    lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .start_choosing(key, &plan.thread.worktree_path);
    emit_status(application, key);
    let handle = application.clone();
    tauri::async_runtime::spawn(async move { run_goal_member(handle, key, plan).await });
    Ok(())
}

/// A message for a running thread: sent at once when idle, queued while busy (Rule 1).
/// Hands a composer message to thread or swarm `identifier` in `project`: a swarm's
/// composer reaches every working member (F10, F16). The project is the one the message
/// was written in, which may no longer be the active one by the time it is sent.
pub(crate) fn deliver_message(
    application: &AppHandle,
    project: u32,
    identifier: usize,
    prompt: &str,
) -> Result<LayoutSnapshot, String> {
    if let Some(result) = crate::swarm::broadcast(application, project, identifier, prompt)
        .or_else(|| crate::goal_swarm::broadcast(application, project, identifier, prompt))
    {
        return result;
    }
    let thread = u32::try_from(identifier).map_err(|error| error.to_string())?;
    steer(application, (project, thread), prompt)
}

/// Withdraws a message you queued that the agent has not read yet (F4, #231). The saved
/// transcript drops it too, so it does not come back after a restart.
pub(crate) fn remove_queued_message(
    application: &AppHandle,
    project: u32,
    identifier: usize,
    index: usize,
) -> Result<LayoutSnapshot, String> {
    let (project, activity) = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let project = store.project(project).ok_or("Unknown project")?;
        let activity = project
            .threads
            .iter()
            .find(|thread| usize::try_from(thread.id).ok() == Some(identifier))
            .map(|thread| thread.activity)
            .ok_or("Unknown thread")?;
        (project.id, activity)
    };
    let key = (
        project,
        u32::try_from(identifier).map_err(|error| error.to_string())?,
    );
    let removed = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .remove_queued(key, index);
    if !removed {
        return Err("The agent has already read that message".into());
    }
    save_activity(application, key, activity, None)?;
    refresh(application, key, false)
}

/// Queues `prompt` for one thread's agent; swarm broadcasts and rebase notices use it too.
pub(crate) fn steer(
    application: &AppHandle,
    key: ThreadKey,
    prompt: &str,
) -> Result<LayoutSnapshot, String> {
    let parked = {
        let mut runtimes = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?;
        if !runtimes.is_live(key) && !runtimes.is_parked(key) {
            drop(runtimes);
            return resume_message(application, key, prompt);
        }
        if runtimes.steer(key, prompt).is_err() {
            drop(runtimes);
            return resume_message(application, key, prompt);
        }
        runtimes.is_parked(key)
    };
    // A parked thread holds the message for its resume and stays parked (§2.2).
    let activity = if parked {
        AgentActivity::Parked
    } else {
        AgentActivity::Thinking
    };
    report(save_activity(application, key, activity, None));
    let snapshot = delivered_snapshot(application, key);
    emit_status(application, key);
    snapshot
}

/// The window once the agent has the message. From here a failed save or refresh is
/// reported, not returned: an error would tell the window the message was never sent, and
/// it would go back to the composer as a draft (#274).
fn delivered_snapshot(application: &AppHandle, key: ThreadKey) -> Result<LayoutSnapshot, String> {
    refresh(application, key, false).or_else(|error| {
        report::<()>(Err(error));
        let layout = lock(
            application.state::<MainLayout>().inner(),
            "main-window state",
        )?;
        Ok(layout.snapshot())
    })
}

fn resume_message(
    application: &AppHandle,
    key: ThreadKey,
    prompt: &str,
) -> Result<LayoutSnapshot, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let (_, thread) = store
        .follow_through_thread(key.0, key.1)
        .map_err(|error| error.to_string())?;
    if thread.activity == AgentActivity::AtLimit {
        return Err("This agent will resume when its limit resets".into());
    }
    drop(store);
    // The message is queued in the runtime, behind any messages queued before, and the
    // session opens with that queue; nothing is lost if it ends before the agent reads it.
    crate::parking::resume_with_message(application, key, prompt)?;
    delivered_snapshot(application, key)
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
    let wanted = mark_worktree_ready(&application, key, &path, failure.is_some());
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
    let mut setup = with_run_settings(
        &application,
        key,
        SessionSetup::new(worktree_path, plan.bypass_modes),
    );
    setup.airc = crate::airc_access::managed(&application)
        .map(|airc| airc.thread(key.0, key.1, &setup.worktree));
    let branch = branch_name.as_deref().unwrap_or(&plan.thread.branch_name);
    if let Err(error) = crate::airc_claims::attach(&application, key, &setup, branch) {
        return fail(&application, key, "Could not claim work", &error);
    }
    let session = spawn_session(plan.agent, setup, inputs);
    pump(&application, key, session).await;
}

async fn run_goal_member(application: AppHandle, key: ThreadKey, plan: GoalMemberPlan) {
    let pools = crate::ready_pool::pools(&application);
    let (worktree_path, branch_name, failure) =
        setup_goal_worktree(pools, key.0, &plan.worktree).await;
    let path = worktree_path.display().to_string();
    if !mark_choosing_worktree(&application, key, &path) {
        return remove_deleted_worktree(plan.worktree.repository_root, worktree_path).await;
    }
    report(record_names(&application, key, &path, Some(&branch_name)));
    if let Some(detail) = failure {
        return fail(&application, key, "Could not set up the worktree", &detail);
    }
    report(refresh(&application, key, false));
    match run_goal_choice(&application, key, &plan.slot, &worktree_path, &branch_name).await {
        Ok(ClaimPhaseOutcome::Left { left_entry, .. }) => {
            finish_goal_left(&application, key, left_entry);
        }
        Ok(ClaimPhaseOutcome::Claimed(claimed)) => {
            finish_goal_claimed(&application, key, plan, worktree_path, &path, *claimed).await;
        }
        Err(error) => {
            if !mark_choosing_worktree(&application, key, &path) {
                let _ = crate::airc_claims::release(&application, key);
                remove_deleted_worktree(plan.worktree.repository_root, worktree_path).await;
            } else {
                fail(&application, key, "Could not choose work", &error);
            }
        }
    }
}

/// The agent's worktree, from the warm pool when it has one ready (F5).
async fn setup_goal_worktree(
    pools: Option<velocity_threads::ReadyPools>,
    project: u32,
    request: &velocity_git::WorktreeSetupRequest,
) -> (PathBuf, String, Option<String>) {
    let owned = request.clone();
    let setup = move || {
        let ticket = pools.and_then(|pools| pools.take(project));
        velocity_git::execute_pooled_worktree_setup(
            &owned,
            ticket.as_ref().map(|ticket| &ticket.ready),
        )
    };
    match tauri::async_runtime::spawn_blocking(setup).await {
        Ok(Ok(outcome)) => (
            outcome.worktree_path,
            outcome
                .branch_name
                .unwrap_or_else(|| request.branch_name.clone().unwrap_or_default()),
            None,
        ),
        Ok(Err(error)) => (
            request.worktree_path.clone(),
            request.branch_name.clone().unwrap_or_default(),
            Some(error.to_string()),
        ),
        Err(error) => (
            request.worktree_path.clone(),
            request.branch_name.clone().unwrap_or_default(),
            Some(error.to_string()),
        ),
    }
}

fn mark_choosing_worktree(application: &AppHandle, key: ThreadKey, path: &str) -> bool {
    lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .map(|mut runtimes| {
        runtimes.rename_worktree(key, path);
        runtimes.is_live(key)
    })
    .unwrap_or(false)
}

async fn run_goal_choice(
    application: &AppHandle,
    key: ThreadKey,
    slot: &GoalAgentSlot,
    worktree_path: &std::path::Path,
    branch_name: &str,
) -> Result<ClaimPhaseOutcome, String> {
    let handle = application.clone();
    let slot = slot.clone();
    let worktree = worktree_path.to_path_buf();
    let branch = branch_name.to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        let data = handle
            .path()
            .app_data_dir()
            .map_err(|error| error.to_string())?;
        let channel = velocity_airc::Channel::open(&data, &key.0.to_string())
            .map_err(|error| error.to_string())?;
        let store = handle.state::<ManagedStateStore>();
        choose_and_claim(
            store.inner(),
            &channel,
            key.0,
            key.1,
            &slot,
            &worktree,
            &branch,
            &clock_timestamp(),
        )
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

fn finish_goal_left(
    application: &AppHandle,
    key: ThreadKey,
    left_entry: Option<Box<velocity_airc::Entry>>,
) {
    if let Ok(mut runtimes) = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    ) {
        runtimes.discard_choosing(key);
    }
    if let Some(entry) = left_entry {
        report(
            crate::airc_events::AircEventPublished::new(key.0, *entry)
                .emit(application)
                .map_err(|error| error.to_string()),
        );
    }
    report(crate::airc_modal::refresh(application));
    report(refresh(application, key, false).map(|_| ()));
}

async fn finish_goal_claimed(
    application: &AppHandle,
    key: ThreadKey,
    plan: GoalMemberPlan,
    worktree_path: PathBuf,
    path: &str,
    claimed: ClaimedGoalWork,
) {
    let inputs = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .ok()
    .and_then(|mut runtimes| runtimes.begin_claimed_session(key, path, &claimed.prompt));
    let Some(inputs) = inputs else {
        let _ = crate::airc_claims::release(application, key);
        remove_deleted_worktree(plan.worktree.repository_root, worktree_path).await;
        return;
    };
    report(crate::airc_modal::refresh(application));
    report(refresh(application, key, false));
    emit_status(application, key);
    let mut setup = with_run_settings(
        application,
        key,
        SessionSetup::new(worktree_path, plan.bypass_modes),
    );
    setup.airc = crate::airc_access::managed(application)
        .map(|airc| airc.thread(key.0, key.1, &setup.worktree));
    let session = spawn_session(plan.agent, setup, inputs);
    pump(application, key, session).await;
}

fn mark_worktree_ready(application: &AppHandle, key: ThreadKey, path: &str, failed: bool) -> bool {
    lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .map(|mut runtimes| {
        runtimes.rename_worktree(key, path);
        if failed {
            runtimes.is_live(key)
        } else {
            runtimes.worktree_ready(key)
        }
    })
    .unwrap_or(false)
}

/// Checks the worktree out while the title model answers; naming and setup wait for the
/// title, at most F4's two seconds (`resolve_title` enforces the deadline). The checkout is a
/// ready worktree from the warm pool when it has one (F5).
async fn take_worktree(
    application: &AppHandle,
    key: ThreadKey,
    plan: &LaunchPlan,
) -> Result<TitledWorktree, String> {
    let (name_sender, name_receiver) = std::sync::mpsc::channel();
    let request = plan.worktree.clone();
    let pools = crate::ready_pool::pools(application);
    let project = key.0;
    let worktree = tauri::async_runtime::spawn_blocking(move || {
        // Held until the checkout is done: dropping it frees the slot for the next one.
        let ticket = pools.and_then(|pools| pools.take(project));
        velocity_git::execute_titled_pooled_setup(
            &request,
            ticket.as_ref().map(|ticket| &ticket.ready),
            || name_receiver.recv().ok().flatten(),
        )
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
pub(crate) async fn pump(application: &AppHandle, key: ThreadKey, mut session: RunningSession) {
    let generation = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .ok()
    .and_then(|runtimes| runtimes.session_identifier(key));
    report(crate::agent_activity::observe(application, key, &[]));
    let shutdown = session.shutdown.clone();
    let mut mutations = velocity_acp::tool_mutations::ToolMutations::default();
    let worktree = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()
    .and_then(|store| store.follow_through_thread(key.0, key.1).ok())
    .and_then(|(_, thread)| thread.worktree_path.map(PathBuf::from));
    while let Some(event) = session.events.recv().await {
        let mut batch = Vec::from([event]);
        while let Ok(next) = session.events.try_recv() {
            batch.push(next);
        }
        if !current_session(application, key, &generation) {
            continue;
        }
        report(save_agent_session(application, key, &batch));
        report(crate::agent_activity::observe(application, key, &batch));
        if let Some(worktree) = &worktree {
            for event in &batch {
                report(crate::airc_claims::observe(
                    application,
                    key,
                    worktree,
                    &mutations.observe(event),
                ));
            }
        }
        report(apply_batch(application, key, &batch));
    }
    let outcome = session.outcome.await;
    if !current_session(application, key, &generation) {
        return;
    }
    finish_session(application, key, shutdown.confirmed(), outcome);
}

fn finish_session(
    application: &AppHandle,
    key: ThreadKey,
    shutdown_confirmed: bool,
    outcome: Result<Result<(), String>, tokio::sync::oneshot::error::RecvError>,
) {
    let still_wanted = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .map(|mut runtimes| runtimes.session_ended_with_shutdown(key, shutdown_confirmed))
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

fn current_session(application: &AppHandle, key: ThreadKey, generation: &Option<String>) -> bool {
    lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .is_ok_and(|runtimes| runtimes.session_identifier(key) == *generation)
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
        Some(progress) => save_progress(application, key, progress)?,
        None => false,
    };
    // Most bursts are streamed text that changes nothing saved. Rebuilding the window from
    // every thread's record and sending it all for each of them, on every agent at once,
    // is what slowed Velocity down over a session (#319). Such a burst only moves its own
    // thread's live transcript, and the window is told at most ten times a second.
    if saved {
        rebuild_layout(application, key)?;
        crate::layout::publish_soon(application);
        emit_status(application, key);
    } else if update_live_view(application, key)? {
        crate::layout::publish_soon(application);
    }
    if batch
        .iter()
        .any(|event| matches!(event, AgentEvent::Transcript { .. }))
    {
        emit_step(application, key);
    }
    Ok(())
}

/// Keeps the agent's ACP session id, which a parked thread loads again on resume (§2.2).
fn save_agent_session(
    application: &AppHandle,
    key: ThreadKey,
    batch: &[AgentEvent],
) -> Result<(), String> {
    let Some(session) = batch.iter().rev().find_map(|event| match event {
        AgentEvent::SessionStarted { session_id, .. } => Some(session_id.0.to_string()),
        _ => None,
    }) else {
        return Ok(());
    };
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |thread| thread.agent_session = Some(session))
    .map(|_| ())
    .map_err(|error| error.to_string())
}

/// Saves the thread's activity, and its line counts when given; `true` when anything changed.
pub(crate) fn save_activity(
    application: &AppHandle,
    key: ThreadKey,
    activity: AgentActivity,
    progress: Option<ThreadProgress>,
) -> Result<bool, String> {
    let transcript = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .view(key)
    .map(|view| view.blocks);
    save_thread(application, key, activity, progress, transcript)
}

/// Saves a streamed batch's progress. The transcript goes along only when its save is due,
/// so a swarm's chunks don't rewrite `state.json` many times a second (#273).
fn save_progress(
    application: &AppHandle,
    key: ThreadKey,
    progress: ThreadProgress,
) -> Result<bool, String> {
    let turn_over = progress.activity == AgentActivity::Idle;
    let transcript = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .transcript_to_save(key, std::time::Instant::now(), turn_over);
    save_thread(
        application,
        key,
        progress.activity,
        Some(progress),
        transcript,
    )
}

fn save_thread(
    application: &AppHandle,
    key: ThreadKey,
    activity: AgentActivity,
    progress: Option<ThreadProgress>,
    transcript: Option<velocity_core::thread_view_model::SharedBlocks>,
) -> Result<bool, String> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |thread| {
        if let Some(transcript) = transcript {
            thread.transcript = transcript;
        }
        if thread.activity != AgentActivity::AtLimit {
            thread.activity = activity;
        }
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

/// The thread moves to `error` and keeps its worktree (§2.1, F5).
pub(crate) fn fail(application: &AppHandle, key: ThreadKey, headline: &str, detail: &str) {
    if let Ok(mut runtimes) = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    ) {
        runtimes.fail(key, headline, detail);
    }
    report(save_activity(application, key, AgentActivity::Idle, None));
    let saved = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .and_then(|mut store| {
        store
            .update_thread(key.0, key.1, |thread| {
                thread.state = ThreadState::Error;
                thread.supervision.unavailable_since.get_or_insert_with(|| {
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs()
                });
                thread.activity = AgentActivity::Idle;
            })
            .map_err(|error| error.to_string())
    });
    report(saved.map(|_| ()));
    crate::airc_events::record_thread_failed(application, key.0, key.1, headline, detail);
    crate::swarm::record_member_failed(application, key, headline);
    crate::goal_swarm::record_member_failed(application, key, headline);
    report(refresh(application, key, false).map(|_| ()));
    emit_status(application, key);
}

/// Rebuilds the window from saved state plus the live transcript and publishes it.
pub(crate) fn refresh(
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
    match view {
        Some(view) => layout.set_live_thread(key.0, identifier, view),
        None => layout.forget_live_thread(key.0, identifier),
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

/// Rebuilds the window's data from the store with `key`'s live transcript, without
/// publishing it.
fn rebuild_layout(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
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
    store.refresh_layout(&mut layout);
    let identifier = usize::try_from(key.1).map_err(|error| error.to_string())?;
    match view {
        Some(view) => layout.set_live_thread(key.0, identifier, view),
        None => layout.forget_live_thread(key.0, identifier),
    }
    Ok(())
}

/// Replaces only `key`'s live transcript. Returns whether the change shows on screen.
fn update_live_view(application: &AppHandle, key: ThreadKey) -> Result<bool, String> {
    let Some(view) = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .view(key) else {
        return Ok(false);
    };
    let identifier = usize::try_from(key.1).map_err(|error| error.to_string())?;
    let mut layout = lock(
        application.state::<MainLayout>().inner(),
        "main-window state",
    )?;
    let visible = layout.live_view_shows(key.0, identifier, &view);
    layout.set_live_thread(key.0, identifier, view);
    Ok(visible)
}

pub(crate) fn emit_status(application: &AppHandle, key: ThreadKey) {
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
        crate::diagnostics::record("thread runtime", &error);
    }
}
