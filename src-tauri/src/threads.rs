use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::{Mutex, MutexGuard};

use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use velocity_acp::client::{AgentEvent, AgentInput, SessionSetup, TitleRequest};
use velocity_core::layout_view::{LayoutIntent, LayoutSnapshot};
use velocity_core::thread_view_model::{SubagentView, TranscriptBlock};
use velocity_core::work_pool::WorkPiece;
use velocity_store::build_gate::{LaunchRequest, require_launch};
use velocity_store::duty::clock_timestamp;
use velocity_store::{AgentActivity, ThreadState};
use velocity_threads::goal_selection::{selection_prompt, selection_reply};
use velocity_threads::goal_swarm::{
    ClaimPhaseOutcome, ClaimedGoalWork, GoalChoice, GoalMemberPlan, GoalSwarmError,
    choose_and_claim_selecting,
};
use velocity_threads::{
    LaunchPlan, RunningSession, SessionInputs, StopOutcome, ThreadKey, ThreadProgress,
    ThreadRuntimes, resolve_title, spawn_session,
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

/// Apply the saved run settings and the AIRC instructions for this thread's launch origin.
fn configure_session(
    application: &AppHandle,
    key: ThreadKey,
    setup: SessionSetup,
    seed_from_title: bool,
) -> SessionSetup {
    let configured = lock(
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
        let airc = crate::airc_access::for_thread(application, key, &setup.worktree, thread);
        let title = seed_from_title.then(|| thread.title.clone());
        Some((
            velocity_threads::thread_run_settings(thread),
            airc,
            title,
            thread.role,
            project.rosen_in_use(),
        ))
    });
    let Some((settings, airc, title, role, rosen)) = configured else {
        return setup;
    };
    let mut setup = setup.with_run_settings(settings);
    setup.role = role;
    setup.read_only = role.is_read_only();
    setup.airc = airc;
    setup.graph_task_title = title;
    if let Ok(app_data) = application.path().app_data_dir() {
        match velocity_threads::agent_memory::prepare_project_memory_for_project(
            &app_data, key.0, rosen,
        ) {
            Ok(memory_dir) => setup.project_memory_dir = Some(memory_dir),
            Err(error) => eprintln!("Project {} agent memory: {error}", key.0),
        }
    }
    crate::handoff::configure(application, key, &mut setup);
    setup
}

/// Whether the thread is still creating its workspace or starting its agent. Nothing of
/// it has run yet, so cleanup that found its work finished waits for it (#514).
pub(crate) fn is_starting(application: &AppHandle, key: ThreadKey) -> Result<bool, String> {
    {
        let runtimes = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?;
        if runtimes.is_setting_up(key) {
            return Ok(true);
        }
        if !runtimes.is_live(key) {
            return Ok(false);
        }
    }
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    Ok(store
        .project(key.0)
        .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
        .is_some_and(|thread| thread.supervision.turns == 0 && thread.agent_session.is_none()))
}

/// Whether the thread's change landed or was abandoned, as Rosen's events recorded it.
fn work_closed(application: &AppHandle, key: ThreadKey) -> bool {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .is_ok_and(|store| {
        store
            .project(key.0)
            .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
            .is_some_and(|thread| matches!(thread.state, ThreadState::Done | ThreadState::Merged))
    })
}

pub(crate) fn stop_follow_through(application: &AppHandle, key: ThreadKey) -> Result<bool, String> {
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
    finish_stopping(application, key, stopped)
}

/// Continuous dispatch must not discard a message queued since it inspected the diff.
pub(crate) fn stop_idle_thread(application: &AppHandle, key: ThreadKey) -> Result<bool, String> {
    let stopped = {
        let mut runtimes = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?;
        if !runtimes.is_at_step_boundary(key) {
            return Ok(false);
        }
        runtimes.stop(key)
    };
    finish_stopping(application, key, stopped)
}

fn finish_stopping(
    application: &AppHandle,
    key: ThreadKey,
    stopped: StopOutcome,
) -> Result<bool, String> {
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
        .map_err(|_| "The agent is still stopping; its workspace was kept".to_owned())?
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

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type, Event)]
#[tauri_specta(event_name = "thread://subagent")]
pub struct ThreadSubagentChanged {
    pub project_identifier: u32,
    pub thread_identifier: u32,
    /// The subagent as it is now: status, newest step and its whole transcript.
    pub subagent: SubagentView,
}

pub(crate) trait Lockable {
    type Target;
    type Guard<'guard>: std::ops::Deref<Target = Self::Target> + std::ops::DerefMut
    where
        Self: 'guard;

    #[track_caller]
    fn lock_with_metrics(&self, what: &str) -> Result<Self::Guard<'_>, String>;
}

impl<Value> Lockable for Mutex<Value> {
    type Target = Value;
    type Guard<'guard>
        = MutexGuard<'guard, Value>
    where
        Value: 'guard;

    fn lock_with_metrics(&self, what: &str) -> Result<Self::Guard<'_>, String> {
        self.lock()
            .map_err(|error| format!("Could not read {what}: {error}"))
    }
}

impl Lockable for ManagedStateStore {
    type Target = velocity_store::StateStore;
    type Guard<'guard> = velocity_store::StoreGuard<'guard>;

    #[track_caller]
    fn lock_with_metrics(&self, what: &str) -> Result<Self::Guard<'_>, String> {
        self.lock()
            .map_err(|error| format!("Could not read {what}: {error}"))
    }
}

impl<Lock: Lockable + Send + Sync + 'static> Lockable for tauri::State<'_, Lock> {
    type Target = Lock::Target;
    type Guard<'guard>
        = Lock::Guard<'guard>
    where
        Self: 'guard;

    #[track_caller]
    fn lock_with_metrics(&self, what: &str) -> Result<Self::Guard<'_>, String> {
        self.inner().lock_with_metrics(what)
    }
}

#[track_caller]
pub(crate) fn lock<'guard, Lock: Lockable + ?Sized>(
    mutex: &'guard Lock,
    what: &str,
) -> Result<Lock::Guard<'guard>, String> {
    mutex.lock_with_metrics(what)
}

/// `start_thread` and `send_message` (§3.1): the composer's thread intents.
pub fn handle_intent(
    application: &AppHandle,
    intent: &LayoutIntent,
) -> Result<Option<LayoutSnapshot>, String> {
    match intent {
        LayoutIntent::StartPrompt(prompt) => start_thread(application, prompt).map(Some),
        LayoutIntent::StartQuestion(prompt) => start_question_thread(application, prompt).map(Some),
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
        let plan = velocity_threads::launch::plan_composer_launch(
            project,
            &worktrees,
            &store.settings(),
            prompt,
            store.composer_thread_model(project.id),
        )
        .map_err(|error| error.to_string())?;
        let thread = store
            .add_composer_thread(plan.project, plan.thread.clone())
            .map_err(|error| error.to_string())?;
        (plan, thread)
    };
    start_planned(application, plan, thread)
}

/// Read-only preface sent ahead of question threads (#533).
pub(crate) const QUESTION_PREFACE: &str = "[Velocity] This is a read-only question thread in the project repository. It has no workspace, Rosen change, claim, or landable status. Answer from the codebase, code graph, spec, and read-only project history without editing files or running builds.";

/// Records a read-only question thread with no workspace, change, or claim and starts its session (#533).
fn start_question_thread(application: &AppHandle, prompt: &str) -> Result<LayoutSnapshot, String> {
    let store_state = application.state::<ManagedStateStore>();
    let (mut plan, thread) = {
        let mut store = lock(store_state.inner(), "persisted state")?;
        let project = store
            .active_project()
            .ok_or("Select or create a project first")?;
        let plan = velocity_threads::plan_question_launch(
            project,
            &store.settings(),
            prompt,
            store.composer_thread_model(project.id),
        )
        .map_err(|error| error.to_string())?;
        let thread = store
            .add_question_thread(plan.project, plan.thread.clone())
            .map_err(|error| error.to_string())?;
        (plan, thread)
    };
    let key = (plan.project, thread);
    let repository = plan.repository.display().to_string();
    let inputs = {
        let mut runtimes = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?;
        let inputs = runtimes.start_question(key, &repository, &plan.prompt);
        if plan.title.request.is_some() {
            runtimes.set_titling(key, Some(plan.title.model_name.clone()));
        }
        inputs
    };
    let snapshot = refresh(application, key, true)?;
    emit_status(application, key);
    if let Some(request) = plan.title.request.take() {
        let (handle, fallback) = (application.clone(), plan.title.fallback.clone());
        tauri::async_runtime::spawn(async move {
            settle_title(&handle, key, &fallback, request).await;
        });
    }
    let handle = application.clone();
    tauri::async_runtime::spawn(
        async move { run_question_session(handle, key, plan, inputs).await },
    );
    Ok(snapshot)
}

async fn run_question_session(
    application: AppHandle,
    key: ThreadKey,
    mut plan: velocity_threads::QuestionPlan,
    inputs: SessionInputs,
) {
    if let Err(error) = bind_launch_account(&application, key, &mut plan.agent) {
        return fail(&application, key, "Could not choose an account", &error);
    }
    report(refresh(&application, key, false));
    if crate::rosen::uses_rosen(&application, key.0) {
        plan.agent.env.push((
            String::from(velocity_rosen::agent_run::BLOCK_VCS_ENV),
            String::from("1"),
        ));
    }
    let mut setup = configure_session(
        &application,
        key,
        SessionSetup::new(plan.repository.clone(), plan.bypass_modes),
        true,
    );
    setup.role = velocity_core::tool_policy::ThreadRole::Question;
    setup.read_only = true;
    setup.preface = Some(QUESTION_PREFACE.into());
    if let Some(server) = crate::rosen::read_only_mcp_server(&application, key.0) {
        setup.shared_servers.push(server);
    }
    setup.pause_on_subscription_limit = true;
    let session = spawn_session(plan.agent, setup, inputs);
    pump(&application, key, session).await;
}

/// F9 uses the same unrestricted worktree/session lifecycle after atomically
/// recording an issue's owning thread.
pub(crate) fn start_planned(
    application: &AppHandle,
    plan: LaunchPlan,
    thread: u32,
) -> Result<LayoutSnapshot, String> {
    start_planned_with_selection(application, plan, thread, true)
}

pub(crate) fn start_planned_with_selection(
    application: &AppHandle,
    mut plan: LaunchPlan,
    thread: u32,
    select: bool,
) -> Result<LayoutSnapshot, String> {
    let key = (plan.project, thread);
    crate::rosen::begin_launch(application, key, &plan.prompt)?;
    let inputs = {
        let mut runtimes = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?;
        let inputs = runtimes.start_on_base(
            key,
            &plan.thread.worktree_path,
            &plan.prompt,
            &plan.worktree.base_ref,
        );
        if plan.title.request.is_some() {
            runtimes.set_titling(key, Some(plan.title.model_name.clone()));
        }
        inputs
    };
    let snapshot = refresh(application, key, select)?;
    emit_status(application, key);
    if let Some(request) = plan.title.request.take() {
        let (handle, fallback) = (application.clone(), plan.title.fallback.clone());
        tauri::async_runtime::spawn(async move {
            settle_title(&handle, key, &fallback, request).await;
        });
    }
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
    crate::rosen::begin_launch(application, key, &plan.slot.prompt)?;
    lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .start_choosing(key, &plan.thread.worktree_path, &plan.worktree.base_ref);
    emit_status(application, key);
    let handle = application.clone();
    tauri::async_runtime::spawn(async move { run_goal_member(handle, key, plan).await });
    Ok(())
}

/// Retries only failed workspace launches; it never resumes an agent in a git checkout.
pub(crate) async fn retry_workspace(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    let action = action_lock(application, key)?;
    let _guard = action.lock_owned().await;
    if lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .is_live(key)
    {
        return Err("The thread is already running".into());
    }
    let plan = plan_workspace_retry(application, key)?;
    match plan {
        velocity_threads::workspace_retry::WorkspaceRetry::Thread(plan) => {
            start_planned_with_selection(application, *plan, key.1, false).map(|_| ())
        }
        velocity_threads::workspace_retry::WorkspaceRetry::Goal(plan) => {
            start_goal_member(application, *plan, key.1)?;
            refresh(application, key, false).map(|_| ())
        }
    }
}

fn plan_workspace_retry(
    application: &AppHandle,
    key: ThreadKey,
) -> Result<velocity_threads::workspace_retry::WorkspaceRetry, String> {
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let (project, thread) = store
        .follow_through_thread(key.0, key.1)
        .map_err(|error| error.to_string())?;
    if thread.state != ThreadState::Error || thread.pending_rosen_launch.is_none() {
        return Err("This thread has no failed Rosen workspace launch".into());
    }
    // Dispatch took the issue back when this thread failed before starting (#514).
    if let Some(issue) = &thread.issue
        && let Some(owner) = velocity_store::issues::issue_thread(&project, issue)
        && owner.id != thread.id
    {
        return Err(format!(
            "Thread {} now works on this thread's issue",
            owner.id
        ));
    }
    let preferences = store.worktree_preferences(&project);
    let plan = velocity_threads::workspace_retry::plan_retry(
        &project,
        &thread,
        &preferences,
        &store.settings(),
    )
    .map_err(|error| error.to_string())?;
    store
        .reclaim_agent_work(key.0, key.1)
        .map_err(|error| error.to_string())?;
    store
        .update_thread(key.0, key.1, |thread| {
            thread.state = ThreadState::Working;
            thread.activity = AgentActivity::Executing;
        })
        .map_err(|error| error.to_string())?;
    Ok(plan)
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
    if let Ok(mut store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) {
        let _ = store.update_thread(key.0, key.1, |thread| {
            thread.supervision.idle_nudged = false;
        });
    }
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
    let lease = action_lock(application, key)?;
    let _guard = lease
        .try_lock()
        .map_err(|_| "The thread is still changing sessions; retry the message".to_owned())?;
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let (_, thread) = store
        .follow_through_thread(key.0, key.1)
        .map_err(|error| error.to_string())?;
    drop(store);
    if thread.activity == AgentActivity::AtLimit {
        return hold_until_reset(application, key, prompt);
    }
    // The message is queued in the runtime, behind any messages queued before, and the
    // session opens with that queue; nothing is lost if it ends before the agent reads it.
    crate::parking::resume_with_message(application, key, prompt)?;
    delivered_snapshot(application, key)
}

/// An agent at its limit resumes on its own at the reset (§2.2), so a message for it, from
/// you or from PR duty's requested changes, is queued for that resume rather than refused:
/// a refusal would leave the agent to resume without it (#355). The queued row is saved
/// with the transcript, so it survives a restart and is not sent twice.
fn hold_until_reset(
    application: &AppHandle,
    key: ThreadKey,
    prompt: &str,
) -> Result<LayoutSnapshot, String> {
    lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .hold_for_resume(key, prompt)
    .map_err(|_| "This agent will resume when its limit resets".to_owned())?;
    report(save_activity(
        application,
        key,
        AgentActivity::AtLimit,
        None,
    ));
    let snapshot = delivered_snapshot(application, key);
    emit_status(application, key);
    snapshot
}

/// Marks the thread's folder ready. False when the thread was deleted meanwhile.
fn folder_ready(
    application: &AppHandle,
    key: ThreadKey,
    folder: &ThreadFolder,
    path: &str,
) -> bool {
    let wanted = mark_worktree_ready(application, key, path, folder.failure.is_some());
    if let Some(change) = &folder.rosen_change
        && let Ok(mut runtimes) = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )
    {
        runtimes.rosen_workspace_ready(key, change);
    }
    wanted
}

/// The folder a new thread's agent works in: a Rosen workspace, or a git worktree when
/// the project chooses git (F5). A failed Rosen launch never takes a git worktree.
#[derive(Clone)]
struct ThreadFolder {
    path: PathBuf,
    branch_name: Option<String>,
    failure: Option<String>,
    rosen_change: Option<String>,
}

async fn take_folder(
    application: &AppHandle,
    key: ThreadKey,
    plan: &mut LaunchPlan,
) -> Result<ThreadFolder, String> {
    if crate::rosen::launch_pending(application, key)? {
        let workspace = crate::rosen::take_workspace(application, key, &plan.worktree).await?;
        plan.agent.env.extend(workspace.environment);
        return Ok(ThreadFolder {
            path: workspace.root,
            branch_name: plan.worktree.branch_name.clone(),
            failure: workspace.setup.err(),
            rosen_change: Some(workspace.change),
        });
    }
    let failure = take_worktree(application, key, plan).await.err();
    Ok(ThreadFolder {
        path: plan.worktree.worktree_path.clone(),
        branch_name: plan.worktree.branch_name.clone(),
        failure,
        rosen_change: None,
    })
}

async fn run_thread(
    application: AppHandle,
    key: ThreadKey,
    mut plan: LaunchPlan,
    inputs: SessionInputs,
) {
    let folder = match take_folder(&application, key, &mut plan).await {
        Ok(folder) => folder,
        Err(error) => {
            return fail(
                &application,
                key,
                "Could not create the Rosen workspace",
                &error,
            );
        }
    };
    let path = folder.path.display().to_string();
    // A thread deleted while its worktree was being created starts no agent (F12).
    if !folder_ready(&application, key, &folder, &path) {
        if let Some(change) = &folder.rosen_change {
            let abandoned =
                crate::rosen::abandon_unstarted(&application, key.0, &folder.path, change).await;
            return report(abandoned);
        }
        return remove_deleted_worktree(plan.worktree.repository_root, folder.path).await;
    }
    report(record_names(
        &application,
        key,
        &path,
        folder.branch_name.as_deref(),
    ));
    if let Some(detail) = &folder.failure {
        return fail(&application, key, "Could not set up the workspace", detail);
    }
    if !finish_workspace_launch(&application, key) {
        return;
    }
    // A change that landed or was abandoned while its workspace was being made gets no
    // agent: the launch ends here and the cleanup that waited for it runs now (#514).
    let closed = work_closed(&application, key);
    if closed || setup_no_longer_wanted(&application, key) {
        if let Ok(mut runtimes) = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        ) {
            runtimes.session_ended(key);
        }
        if closed {
            crate::rosen_events::run_follow_ups(&application, key.0).await;
        }
        return;
    }
    run_thread_session(application, key, plan, inputs, folder).await;
}

fn bind_launch_account(
    application: &AppHandle,
    key: ThreadKey,
    launch: &mut velocity_acp::client::AgentLaunch,
) -> Result<(), String> {
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let thread = store
        .project(key.0)
        .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
        .ok_or("Missing thread")?;
    let settings = store.settings();
    let provider = thread.provider;
    settings
        .ensure_new_work(provider)
        .map_err(|error| error.to_string())?;
    let preferred = thread
        .preferred_account
        .unwrap_or_else(|| settings.default_account(provider));
    let account =
        settings.launch_account(provider, preferred, velocity_store::providers::unix_now());
    let directory = settings
        .saved_account(provider, account)
        .map(|profile| profile.directory.as_str())
        .unwrap_or_default();
    launch.use_account(account, directory);
    store
        .update_thread(key.0, key.1, |thread| thread.bind_provider_account(account))
        .map_err(|error| error.to_string())?;
    Ok(())
}

async fn run_thread_session(
    application: AppHandle,
    key: ThreadKey,
    mut plan: LaunchPlan,
    inputs: SessionInputs,
    folder: ThreadFolder,
) {
    let ThreadFolder {
        path: worktree_path,
        branch_name,
        rosen_change,
        ..
    } = folder;
    if let Err(error) = bind_launch_account(&application, key, &mut plan.agent) {
        return fail(&application, key, "Could not choose an account", &error);
    }
    report(refresh(&application, key, false));
    let mut setup = configure_session(
        &application,
        key,
        SessionSetup::new(worktree_path, plan.bypass_modes),
        true,
    );
    let branch = branch_name.as_deref().unwrap_or(&plan.thread.branch_name);
    setup.session_card = Some(match rosen_change.as_deref() {
        Some(change) => {
            crate::rosen_issue::session_card(&application, key, &setup.worktree, change)
        }
        None => velocity_acp::client::SessionCard {
            identity: Some(format!(
                "{} (thread-{})",
                velocity_rosen::agent_name(&setup.worktree),
                key.1
            )),
            workspace: Some(setup.worktree.clone()),
            change: Some(branch.to_owned()),
            is_rosen: false,
            ..velocity_acp::client::SessionCard::default()
        },
    });
    if rosen_change.is_some() {
        let server =
            crate::rosen::mcp_server_for_role(&setup.worktree, &plan.agent.env, setup.role);
        setup.shared_servers.extend(server);
    }
    if let Err(error) = crate::airc_claims::attach_with_prompt(
        &application,
        key,
        &setup,
        branch,
        Some(&plan.prompt),
    ) {
        return fail(&application, key, "Could not claim work", &error);
    }
    add_work_handoff(&application, key, &mut setup);
    attach_task_brief(&application, key, &plan.prompt, &mut setup).await;
    crate::browser::configure_for_prompt(
        &application,
        key,
        &plan.thread.title,
        &plan.prompt,
        &mut setup,
    );
    setup.pause_on_subscription_limit = true;
    let session = spawn_session(plan.agent, setup, inputs);
    pump(&application, key, session).await;
}

async fn run_goal_member(application: AppHandle, key: ThreadKey, mut plan: GoalMemberPlan) {
    let folder = match take_goal_folder(&application, key, &mut plan).await {
        Ok(folder) => folder,
        Err(error) => {
            return fail(
                &application,
                key,
                "Could not create the Rosen workspace",
                &error,
            );
        }
    };
    let worktree_path = folder.path.clone();
    let branch_name = folder.branch_name.clone().unwrap_or_default();
    let path = worktree_path.display().to_string();
    if !mark_choosing_worktree(&application, key, &path) {
        return discard_goal_folder(&application, key, &plan, &folder).await;
    }
    report(record_names(&application, key, &path, Some(&branch_name)));
    if let Some(detail) = &folder.failure {
        return fail(&application, key, "Could not set up the workspace", detail);
    }
    if !finish_workspace_launch(&application, key) {
        return;
    }
    if let Err(error) = bind_launch_account(&application, key, &mut plan.agent) {
        return fail(&application, key, "Could not choose an account", &error);
    }
    report(refresh(&application, key, false));
    match run_goal_choice(&application, key, &plan, &folder, &branch_name).await {
        Ok((ClaimPhaseOutcome::Left { left_entry, .. }, _)) => {
            if folder.rosen_change.is_some() {
                discard_goal_folder(&application, key, &plan, &folder).await;
            }
            finish_goal_left(&application, key, left_entry);
        }
        Ok((ClaimPhaseOutcome::Claimed(claimed), session)) => {
            finish_goal_claimed(&application, key, plan, folder, *claimed, session).await;
        }
        Err(error) => {
            let can_discard = lock(
                application.state::<ManagedThreads>().inner(),
                "thread sessions",
            )
            .is_ok_and(|runtimes| runtimes.is_retired(key) && !runtimes.shutdown_failed(key));
            if can_discard {
                let _ = crate::airc_claims::release(&application, key);
                discard_goal_folder(&application, key, &plan, &folder).await;
            } else {
                fail(&application, key, "Could not choose work", &error);
            }
        }
    }
}

fn finish_workspace_launch(application: &AppHandle, key: ThreadKey) -> bool {
    match crate::rosen::finish_launch(application, key) {
        Ok(()) => true,
        Err(error) => {
            fail(application, key, "Could not record workspace setup", &error);
            false
        }
    }
}

async fn take_goal_folder(
    application: &AppHandle,
    key: ThreadKey,
    plan: &mut GoalMemberPlan,
) -> Result<ThreadFolder, String> {
    if crate::rosen::launch_pending(application, key)? {
        let workspace = crate::rosen::take_workspace(application, key, &plan.worktree).await?;
        plan.agent.env.extend(workspace.environment);
        return Ok(ThreadFolder {
            path: workspace.root,
            branch_name: plan.worktree.branch_name.clone(),
            failure: workspace.setup.err(),
            rosen_change: Some(workspace.change),
        });
    }
    let pools = crate::ready_pool::pools(application);
    let (path, branch, failure) =
        setup_goal_worktree(application, pools, key.0, &plan.worktree).await;
    Ok(ThreadFolder {
        path,
        branch_name: Some(branch),
        failure,
        rosen_change: None,
    })
}

async fn discard_goal_folder(
    application: &AppHandle,
    key: ThreadKey,
    plan: &GoalMemberPlan,
    folder: &ThreadFolder,
) {
    if let Some(change) = &folder.rosen_change {
        report(crate::rosen::abandon_unstarted(application, key.0, &folder.path, change).await);
    } else if folder.path.exists() {
        remove_deleted_worktree(plan.worktree.repository_root.clone(), folder.path.clone()).await;
    }
}

/// The agent's worktree, from the warm pool when it has one ready (F5).
async fn setup_goal_worktree(
    application: &AppHandle,
    pools: Option<velocity_threads::ReadyPools>,
    project: u32,
    request: &velocity_git::WorktreeSetupRequest,
) -> (PathBuf, String, Option<String>) {
    let mut owned = request.clone();
    let application = application.clone();
    let setup = move || {
        let tip = synchronize_git_request(&application, project, &mut owned)?;
        let mut ticket = pools.and_then(|pools| pools.take(project));
        if let (Some(ticket), Some(tip)) = (&mut ticket, tip) {
            ticket.ready.tip = Some(tip);
        }
        velocity_git::execute_pooled_worktree_setup(
            &owned,
            ticket.as_ref().map(|ticket| &ticket.ready),
        )
        .map_err(|error| error.to_string())
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

struct GoalSelectionTurn {
    inputs: tokio::sync::mpsc::UnboundedSender<AgentInput>,
    session: RunningSession,
    events: Vec<AgentEvent>,
}

struct GoalSessionChoice<'a> {
    application: &'a AppHandle,
    key: ThreadKey,
    plan: &'a GoalMemberPlan,
    folder: &'a ThreadFolder,
    turn: Option<GoalSelectionTurn>,
}

impl GoalChoice for GoalSessionChoice<'_> {
    fn choose(&mut self, shortlist: &[&WorkPiece]) -> Result<Vec<String>, GoalSwarmError> {
        let prompt = selection_prompt(&self.plan.slot.prompt, shortlist);
        let mut setup = goal_session_setup(
            self.application,
            self.key,
            self.plan,
            self.folder,
            &self.plan.slot.prompt,
        );
        if let Some(experiment) = &mut setup.handoff {
            experiment.skip_first_turn = true;
        }
        let (inputs, receiver) = lock(
            self.application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )
        .map_err(GoalSwarmError::Selection)?
        .begin_goal_selection(self.key, &prompt)
        .ok_or_else(|| GoalSwarmError::Selection("The member stopped while choosing".into()))?;
        let turn = self.turn.insert(GoalSelectionTurn {
            inputs,
            session: spawn_session(self.plan.agent.clone(), setup, receiver),
            events: Vec::new(),
        });
        let reply = tauri::async_runtime::block_on(selection_reply(
            &mut turn.session.events,
            &mut turn.events,
        ))?;
        if setup_no_longer_wanted(self.application, self.key) {
            return Err(GoalSwarmError::Selection(
                "The member stopped while choosing".into(),
            ));
        }
        Ok(reply)
    }

    fn stop(&mut self) -> Result<(), GoalSwarmError> {
        if let Some(turn) = self.turn.take() {
            tauri::async_runtime::block_on(turn.stop(self.application, self.key))?;
        }
        Ok(())
    }
}

impl GoalSelectionTurn {
    async fn stop(self, application: &AppHandle, key: ThreadKey) -> Result<(), GoalSwarmError> {
        let _ = self.inputs.send(AgentInput::Stop);
        let result = self.session.outcome.await;
        crate::browser::stop(application, key).await;
        let confirmed = self.session.shutdown.confirmed();
        let mut runtimes = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )
        .map_err(GoalSwarmError::Selection)?;
        runtimes.session_ended_with_shutdown(key, confirmed);
        if confirmed {
            return Ok(());
        }
        Err(GoalSwarmError::Selection(match result {
            Ok(Err(reason)) => reason,
            _ => "The session could not confirm all of its processes stopped".into(),
        }))
    }
}

fn goal_session_setup(
    application: &AppHandle,
    key: ThreadKey,
    plan: &GoalMemberPlan,
    folder: &ThreadFolder,
    graph_title: &str,
) -> SessionSetup {
    let mut setup = configure_session(
        application,
        key,
        SessionSetup::new(folder.path.clone(), plan.bypass_modes.clone()),
        false,
    )
    .with_graph_task_title(graph_title.to_owned());
    if let Some(change) = &folder.rosen_change {
        setup.session_card = Some(crate::rosen_issue::session_card(
            application,
            key,
            &folder.path,
            change,
        ));
        setup
            .shared_servers
            .extend(crate::rosen::mcp_server_for_role(
                &folder.path,
                &plan.agent.env,
                setup.role,
            ));
    }
    crate::browser::configure_for_prompt(
        application,
        key,
        graph_title,
        &plan.slot.prompt,
        &mut setup,
    );
    setup.pause_on_subscription_limit = true;
    setup
}

async fn run_goal_choice(
    application: &AppHandle,
    key: ThreadKey,
    plan: &GoalMemberPlan,
    folder: &ThreadFolder,
    branch_name: &str,
) -> Result<(ClaimPhaseOutcome, Option<GoalSelectionTurn>), String> {
    let handle = application.clone();
    let plan = plan.clone();
    let folder = folder.clone();
    let branch = branch_name.to_owned();
    tauri::async_runtime::spawn_blocking(move || {
        let store = handle.state::<ManagedStateStore>();
        let project = lock(store.inner(), "persisted state")?
            .project(key.0)
            .cloned();
        let rosen = project
            .as_ref()
            .is_some_and(velocity_store::ProjectItem::rosen_in_use);
        let data = handle
            .path()
            .app_data_dir()
            .map_err(|error| error.to_string())?;
        let channel = velocity_airc::Channel::open_for_project(&data, &key.0.to_string(), rosen)
            .map_err(|error| error.to_string())?;
        let subsystems = project
            .as_ref()
            .and_then(|project| crate::code_graph::subsystem_map(&handle, project));
        let mut choice = GoalSessionChoice {
            application: &handle,
            key,
            plan: &plan,
            folder: &folder,
            turn: None,
        };
        let result = choose_and_claim_selecting(
            store.inner(),
            &channel,
            key.0,
            key.1,
            &plan.slot,
            &folder.path,
            &branch,
            &clock_timestamp(),
            subsystems.as_ref(),
            &mut choice,
        );
        match result {
            Ok(outcome) => Ok((outcome, choice.turn)),
            Err(error) => {
                choice.stop().map_err(|error| error.to_string())?;
                Err(error.to_string())
            }
        }
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
    folder: ThreadFolder,
    claimed: ClaimedGoalWork,
    selection: Option<GoalSelectionTurn>,
) {
    if let Some(change) = &folder.rosen_change
        && let Err(error) =
            crate::rosen::record_work_sources(application, key, &folder.path, change).await
    {
        if let Some(turn) = selection {
            report(
                turn.stop(application, key)
                    .await
                    .map_err(|error| error.to_string()),
            );
        }
        return fail(
            application,
            key,
            "Could not record Rosen work sources",
            &error,
        );
    }
    let task_brief = build_thread_task_brief(application, key, &claimed.prompt, &folder.path).await;
    let path = folder.path.display().to_string();
    let prompt = work_handoff(application, key).map_or_else(
        || claimed.prompt.clone(),
        |handoff| format!("{handoff}\n\n{}", claimed.prompt),
    );
    let session = if let Some(turn) = selection {
        report(save_agent_session(application, key, &turn.events));
        report(apply_batch(application, key, &turn.events));
        let continued_prompt = match &task_brief {
            Some(brief) => format!("{brief}\n\n{prompt}"),
            None => prompt.clone(),
        };
        let continued = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )
        .ok()
        .and_then(|mut runtimes| runtimes.continue_claimed_session(key, &path, &continued_prompt));
        if continued.is_some() {
            Some(turn.session)
        } else {
            if let Err(error) = turn.stop(application, key).await {
                return fail(
                    application,
                    key,
                    "Could not stop choosing work",
                    &error.to_string(),
                );
            }
            None
        }
    } else {
        let inputs = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )
        .ok()
        .and_then(|mut runtimes| runtimes.begin_claimed_session(key, &path, &prompt));
        inputs.map(|inputs| {
            let mut setup =
                goal_session_setup(application, key, &plan, &folder, &claimed.piece.title);
            setup.task_brief = task_brief;
            spawn_session(plan.agent.clone(), setup, inputs)
        })
    };
    let Some(session) = session else {
        let retired = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )
        .is_ok_and(|runtimes| runtimes.is_retired(key));
        if !retired {
            return fail(
                application,
                key,
                "Could not start work",
                "The choosing session stopped",
            );
        }
        let _ = crate::airc_claims::release(application, key);
        discard_goal_folder(application, key, &plan, &folder).await;
        return;
    };
    report(crate::airc_modal::refresh(application));
    report(refresh(application, key, false));
    emit_status(application, key);
    pump(application, key, session).await;
}

fn add_work_handoff(application: &AppHandle, key: ThreadKey, setup: &mut SessionSetup) {
    let members = handoff_members(application, key);
    if let Some(card) = setup.session_card.as_mut() {
        populate_session_card_handoff(application, key, &members, card);
        return;
    }
    let Some(handoff) = work_handoff(application, key) else {
        return;
    };
    setup.preface = Some(match setup.preface.take() {
        Some(preface) => format!("{preface}\n\n{handoff}"),
        None => handoff,
    });
}

fn handoff_members(application: &AppHandle, key: ThreadKey) -> Vec<String> {
    let mut members = vec![format!("thread-{}", key.1)];
    if let Ok(store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) && let Some(previous) = store
        .project(key.0)
        .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
        .and_then(|thread| thread.previous_work_thread)
    {
        members.push(format!("thread-{previous}"));
    }
    members
}

fn populate_session_card_handoff(
    application: &AppHandle,
    key: ThreadKey,
    members: &[String],
    card: &mut velocity_acp::client::SessionCard,
) {
    use velocity_threads::launch::handoff_line;
    let mut rosen = false;
    if let Ok(store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) && let Some(project) = store.project(key.0)
    {
        rosen = project.rosen_in_use();
        if let Some(thread) = project.threads.iter().find(|thread| thread.id == key.1) {
            if card.work_item.is_none()
                && (thread.started_by_dispatch || thread.goal_member.is_some())
            {
                card.work_item = Some(handoff_line(&thread.title));
            }
            if let Some(previous) = thread
                .previous_work_thread
                .and_then(|id| project.threads.iter().find(|item| item.id == id))
            {
                let outcome = if previous.state == ThreadState::Merged {
                    "landed"
                } else {
                    "ended without landing"
                };
                let mut summary = format!("{} ({outcome})", handoff_line(&previous.title));
                if let Some(change) = &previous.rosen_change {
                    summary.push_str(&format!(" · change {change}"));
                }
                card.previous_item = Some(summary);
            }
        }
    }
    if let Ok(data) = application.path().app_data_dir() {
        append_airc_claims_to_card(&data, key.0, rosen, members, card);
    }
}

fn append_airc_claims_to_card(
    data: &std::path::Path,
    project: u32,
    rosen: bool,
    members: &[String],
    card: &mut velocity_acp::client::SessionCard,
) {
    use velocity_threads::launch::handoff_line;
    let Ok(channel) = velocity_airc::Channel::open_for_project(data, &project.to_string(), rosen)
    else {
        return;
    };
    if let Ok(claims) = channel.claims() {
        for claim in claims
            .iter()
            .filter(|claim| members.contains(&claim.member))
            .take(4)
        {
            let files = claim
                .files
                .iter()
                .take(4)
                .map(|file| handoff_line(file))
                .collect::<Vec<_>>()
                .join(", ");
            let paths = if files.is_empty() { "none" } else { &files };
            card.push_claim(format!("{} (paths: {paths})", handoff_line(claim.key())));
        }
    }
    if let Ok(waits) = channel.dependency_waits() {
        for wait in waits
            .iter()
            .filter(|wait| members.contains(&wait.member))
            .take(4)
        {
            card.push_claim(format!(
                "wait: {} holds {}",
                handoff_line(&wait.holder_nick),
                handoff_line(&wait.file)
            ));
        }
    }
}

async fn attach_task_brief(
    application: &AppHandle,
    key: ThreadKey,
    prompt: &str,
    setup: &mut SessionSetup,
) {
    setup.task_brief = build_thread_task_brief(application, key, prompt, &setup.worktree).await;
}

async fn build_thread_task_brief(
    application: &AppHandle,
    key: ThreadKey,
    prompt: &str,
    worktree: &std::path::Path,
) -> Option<String> {
    let app_data = application.path().app_data_dir().ok()?;
    let (project, thread) = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )
        .ok()?;
        let project = store.project(key.0)?.clone();
        let thread = project
            .threads
            .iter()
            .find(|thread| thread.id == key.1)?
            .clone();
        (project, thread)
    };
    let input = velocity_threads::task_brief::thread_brief_input(
        &project,
        &thread,
        prompt,
        worktree,
        Vec::new(),
    )?;
    let brief = velocity_threads::task_brief::build_brief_async(&app_data, &project, input).await;
    Some(brief.render_for_session())
}

fn work_handoff(application: &AppHandle, key: ThreadKey) -> Option<String> {
    let (mut handoff, members, rosen) = saved_work_handoff(application, key)?;
    if let Ok(data) = application.path().app_data_dir() {
        handoff.push('\n');
        handoff.push_str(&velocity_threads::launch::airc_work_handoff_for_project(
            &data, key.0, rosen, &members,
        ));
    }
    Some(handoff)
}

fn saved_work_handoff(
    application: &AppHandle,
    key: ThreadKey,
) -> Option<(String, Vec<String>, bool)> {
    use velocity_threads::launch::handoff_line;
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()?;
    let project = store.project(key.0)?;
    let thread = project.threads.iter().find(|thread| thread.id == key.1)?;
    if !thread.started_by_dispatch && thread.goal_member.is_none() {
        return None;
    }
    let workspace = thread.worktree_path.as_deref().unwrap_or_default();
    let change = thread
        .rosen_change
        .as_deref()
        .unwrap_or(&thread.branch_name);
    let mut summary = format!(
        "Velocity work handoff\nItem: {}\nWorkspace: {workspace} ({change})",
        handoff_line(&thread.title)
    );
    let mut members = vec![thread.airc_member()];
    if let Some(previous) = thread
        .previous_work_thread
        .and_then(|id| project.threads.iter().find(|thread| thread.id == id))
    {
        members.push(previous.airc_member());
        let outcome = if previous.state == ThreadState::Merged {
            "landed"
        } else {
            "ended without landing"
        };
        summary.push_str(&format!(
            "\nPrevious item ({outcome}): {}",
            handoff_line(&previous.title)
        ));
        if let Some(change) = &previous.rosen_change {
            summary.push_str(&format!(" (Rosen change {change})"));
        }
        for task in project
            .tasks
            .iter()
            .filter(|task| {
                task.state != velocity_store::TaskState::Done
                    && (task.added_by == velocity_store::TaskAttribution::Thread(previous.id)
                        || task.assigned_thread_id == Some(previous.id))
            })
            .take(4)
        {
            summary.push_str(&format!(
                "\nOpen Task List item #{}: {}",
                task.id,
                handoff_line(&task.text)
            ));
        }
    }
    Some((summary, members, project.rosen_in_use()))
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

/// Takes a worktree with the names chosen at launch, using the warm pool when ready (F5).
async fn take_worktree(
    application: &AppHandle,
    key: ThreadKey,
    plan: &LaunchPlan,
) -> Result<(), String> {
    let mut request = plan.worktree.clone();
    let pools = crate::ready_pool::pools(application);
    let project = key.0;
    let handle = application.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let tip = synchronize_git_request(&handle, project, &mut request)?;
        // Held until the checkout is done: dropping it frees the slot for the next one.
        let mut ticket = pools.and_then(|pools| pools.take(project));
        if let (Some(ticket), Some(tip)) = (&mut ticket, tip) {
            ticket.ready.tip = Some(tip);
        }
        velocity_git::execute_pooled_worktree_setup(
            &request,
            ticket.as_ref().map(|ticket| &ticket.ready),
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}

fn synchronize_git_request(
    application: &AppHandle,
    project: u32,
    request: &mut velocity_git::WorktreeSetupRequest,
) -> Result<Option<String>, String> {
    let tip = crate::rosen::git_base(
        application,
        project,
        &request.repository_root,
        &request.base_ref,
    )?;
    if let Some(tip) = &tip {
        request.base_ref = tip.clone();
    }
    Ok(tip)
}

/// Only the display title can change when the model answers (F4).
async fn settle_title(
    application: &AppHandle,
    key: ThreadKey,
    fallback: &str,
    request: TitleRequest,
) {
    let model = format!(
        "{} · {}",
        request.settings.provider.label(),
        request.settings.model
    );
    let request = match application.path().app_data_dir() {
        Ok(data) => request.with_usage(data, key.0, key.1),
        Err(_) => request,
    };
    let title = resolve_title(request).await;
    record_title_outcome(
        application,
        model,
        title.as_ref().err().map(ToString::to_string),
    );
    if let Ok(mut runtimes) = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    ) {
        runtimes.set_titling(key, None);
    }
    if let Ok(title) = title {
        let mut updated = false;
        let saved = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )
        .and_then(|mut store| {
            store
                .update_thread(key.0, key.1, |thread| {
                    if thread.title == fallback {
                        thread.title = title.clone();
                        updated = true;
                    }
                })
                .map_err(|error| error.to_string())
        });
        report(saved);
        if updated {
            report(crate::airc_claims::update_title(application, key, &title));
        }
    }
    report(refresh(application, key, false));
}

/// Warn once after three consecutive failures for a title model; a success resets the count.
fn record_title_outcome(application: &AppHandle, model: String, error: Option<String>) {
    static FAILURES: LazyLock<Mutex<HashMap<String, u8>>> = LazyLock::new(Mutex::default);
    const WARNING_THRESHOLD: u8 = 3;
    let Some(error) = error else {
        if let Ok(mut failures) = FAILURES.lock() {
            failures.remove(&model);
        }
        return;
    };
    crate::diagnostics::record("thread title", &format!("{model}: {error}"));
    let Ok(mut failures) = FAILURES.lock() else {
        return;
    };
    let count = failures.entry(model).or_default();
    *count = count.saturating_add(1);
    let warn = *count == WARNING_THRESHOLD;
    drop(failures);
    if warn {
        crate::layout::show_toast(
            application,
            "Thread title generation keeps failing. Check Settings → Providers → Thread titles."
                .into(),
        );
    }
}

/// Saves the branch and folder the launch took.
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

pub(crate) fn setup_no_longer_wanted(application: &AppHandle, key: ThreadKey) -> bool {
    !lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .map(|runtimes| runtimes.is_live(key))
    .unwrap_or(false)
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
    let mut handoff = crate::handoff::Progress::default();
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
            crate::handoff::reject(&batch);
            continue;
        }
        report(save_agent_session(application, key, &batch));
        report(save_queued_runs(application, key, &batch));
        report(crate::agent_activity::observe(application, key, &batch));
        report(crate::handoff::observe(
            application,
            key,
            &batch,
            &mut handoff,
        ));
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
        if batch
            .iter()
            .any(|event| matches!(event, AgentEvent::TurnEnded(_)))
        {
            // A turn's end is the agent's step boundary: a rebase Rosen made meanwhile
            // is written into its workspace now, before its next turn reads it.
            crate::rosen_rebase::apply_at_boundary(application, key).await;
            // Land cleanup can wait for this pump to acknowledge session shutdown.
            let handle = application.clone();
            tauri::async_runtime::spawn(async move {
                crate::rosen_events::run_follow_ups(&handle, key.0).await;
            });
        }
    }
    let outcome = session.outcome.await;
    if !current_session(application, key, &generation) {
        return;
    }
    crate::browser::stop(application, key).await;
    let clean_exit = matches!(outcome, Ok(Ok(())));
    let replace = handoff.accepted && shutdown.confirmed() && clean_exit;
    let still_wanted = finish_session(application, key, shutdown.confirmed(), outcome, replace);
    if still_wanted && replace {
        let handle = application.clone();
        tauri::async_runtime::spawn_blocking(move || crate::handoff::resume(&handle, key));
    } else if still_wanted && clean_exit {
        // A follow-up waiting for the agent's step can deliver now that the session ended cleanly.
        let handle = application.clone();
        tauri::async_runtime::spawn(async move {
            crate::rosen_events::run_follow_ups(&handle, key.0).await;
        });
    }
}

fn finish_session(
    application: &AppHandle,
    key: ThreadKey,
    shutdown_confirmed: bool,
    outcome: Result<Result<(), String>, tokio::sync::oneshot::error::RecvError>,
    replacing: bool,
) -> bool {
    let still_wanted = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .map(|mut runtimes| runtimes.session_ended_with_shutdown(key, shutdown_confirmed))
    .unwrap_or(false);
    if !replacing {
        report(clear_queued_runs(application, key));
    }
    if !still_wanted || replacing {
        return still_wanted;
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
    // Subagents still running when the session ended are over now.
    emit_subagents(application, key);
    still_wanted
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
    for event in batch {
        if let AgentEvent::Log(line) = event
            && line.starts_with(velocity_core::agent_env::WITHHELD_LOG_PREFIX)
        {
            crate::diagnostics::record(&format!("thread {}/{}", key.0, key.1), line);
        }
    }
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
    emit_subagents(application, key);
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

/// Persists outstanding `queue_run` commands, phase transitions, and finished run history
/// so a Velocity restart can replay or report them and `run_status` can recover results (#516, #536).
fn save_queued_runs(
    application: &AppHandle,
    key: ThreadKey,
    batch: &[AgentEvent],
) -> Result<(), String> {
    let has_run_event = batch.iter().any(|event| {
        matches!(
            event,
            AgentEvent::QueuedRunAdded(_)
                | AgentEvent::QueuedRunUpdated(_)
                | AgentEvent::QueuedRunFinished(_)
                | AgentEvent::QueuedRunsDelivered { .. }
        )
    });
    if !has_run_event {
        return Ok(());
    }
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |thread| {
        for event in batch {
            match event {
                AgentEvent::QueuedRunAdded(record) | AgentEvent::QueuedRunUpdated(record) => {
                    if let Some(existing) = thread
                        .queued_runs
                        .iter_mut()
                        .find(|existing| existing.id == record.id)
                    {
                        *existing = record.clone();
                    } else if matches!(event, AgentEvent::QueuedRunAdded(_)) {
                        thread.queued_runs.push(record.clone());
                    }
                }
                AgentEvent::QueuedRunFinished(record) => {
                    thread
                        .queued_runs
                        .retain(|existing| existing.id != record.id);
                    thread
                        .queued_run_history
                        .retain(|existing| existing.id != record.id);
                    thread.queued_run_history.push(record.clone());
                    let excess = thread
                        .queued_run_history
                        .len()
                        .saturating_sub(velocity_core::queued_runs::MAX_RUN_HISTORY);
                    if excess > 0 {
                        thread.queued_run_history.drain(..excess);
                    }
                }
                AgentEvent::QueuedRunsDelivered { ids } => {
                    for record in &mut thread.queued_run_history {
                        if ids.iter().any(|id| id == &record.id) {
                            record.delivered = true;
                        }
                    }
                }
                _ => {}
            }
        }
    })
    .map(|_| ())
    .map_err(|error| error.to_string())
}

/// Explicitly stopping or ending a session cancels its queued commands.
fn clear_queued_runs(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    match store.update_thread(key.0, key.1, |thread| thread.queued_runs.clear()) {
        Ok(_) | Err(velocity_store::threads::ThreadError::UnknownThread { .. }) => Ok(()),
        Err(error) => Err(error.to_string()),
    }
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
    crate::continuous_dispatch::return_unstarted(application, key, headline, detail);
    crate::airc_events::record_thread_failed(application, key.0, key.1, headline, detail);
    crate::swarm::record_member_failed(application, key, headline);
    crate::goal_swarm::record_member_failed(application, key, headline);
    report(refresh(application, key, false).map(|_| ()));
    emit_status(application, key);
}

pub(crate) fn refresh_account_labels(application: &AppHandle) -> Result<(), String> {
    crate::layout::refresh_and_publish(application)
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
    let refresh = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .layout_refresh();
    let identifier = usize::try_from(key.1).map_err(|error| error.to_string())?;
    let active_matches = refresh.active_project == Some(key.0);
    let snapshot = {
        let mut layout = lock(
            application.state::<MainLayout>().inner(),
            "main-window state",
        )?;
        let _ = refresh.apply(&mut layout);
        match view {
            Some(view) => layout.set_live_thread(key.0, identifier, view),
            None => layout.forget_live_thread(key.0, identifier),
        }
        if select && active_matches {
            layout.select_thread(identifier);
        }
        layout.snapshot()
    };
    crate::layout::publish(application, &snapshot, None)?;
    Ok(snapshot)
}

/// Updates the window's data for `key` plus its live transcript without holding the store
/// and layout locks together or rebuilding every thread entry on streamed batches (#554).
fn rebuild_layout(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    let view = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .view(key);
    let patch = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .layout_thread_update(key.0, key.1);
    let identifier = usize::try_from(key.1).map_err(|error| error.to_string())?;
    let applied = {
        let mut layout = lock(
            application.state::<MainLayout>().inner(),
            "main-window state",
        )?;
        let applied = match patch {
            Some((revision, entry, spec_locked)) => {
                layout.replace_thread_entry(key.0, revision, entry, spec_locked)
            }
            None => true,
        };
        if applied {
            match view {
                Some(view) => layout.set_live_thread(key.0, identifier, view),
                None => layout.forget_live_thread(key.0, identifier),
            }
            return Ok(());
        }
        false
    };
    debug_assert!(!applied);
    let refresh = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .layout_refresh();
    let mut layout = lock(
        application.state::<MainLayout>().inner(),
        "main-window state",
    )?;
    let _ = refresh.apply(&mut layout);
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

/// Sends each subagent that changed since the last call (§3.2 Subagents).
fn emit_subagents(application: &AppHandle, key: ThreadKey) {
    let changed = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .map(|mut runtimes| runtimes.take_changed_subagents(key))
    .unwrap_or_default();
    for subagent in changed {
        let event = ThreadSubagentChanged {
            project_identifier: key.0,
            thread_identifier: key.1,
            subagent,
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
