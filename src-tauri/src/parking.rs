//! Parking edge (SPEC.md §2.2): ends a waiting agent's session and resumes it when its
//! wait ends. `velocity_threads::parking` decides; this module stops and starts sessions.
use std::path::Path;

use tauri::{AppHandle, Manager};
use velocity_acp::client::SessionSetup;
use velocity_airc::Channel;
use velocity_store::{AgentActivity, ProjectItem, ThreadItem, ThreadState};
use velocity_threads::parking::{
    ParkingAction, ParkingInput, RosenParkingAction, RosenParkingInput, parking_action,
    resume_prompt, rosen_parking_action, rosen_resume_prompt,
};
use velocity_threads::{ThreadKey, agent_launch_for_account, spawn_session};

use crate::state::ManagedStateStore;
use crate::threads::{ManagedThreads, lock};

/// One parking pass over a project's threads, run from the AIRC delivery cycle after the
/// runner's lifecycle has been written as wait ends.
pub fn cycle(application: &AppHandle, project: &ProjectItem, channel: &Channel) {
    for thread in &project.threads {
        if !matches!(thread.state, ThreadState::Working | ThreadState::Review) {
            continue;
        }
        let key = (project.id, thread.id);
        if let Err(error) = step(application, key, thread, channel) {
            eprintln!("Velocity parking thread {}: {error}", thread.id);
        }
    }
}

fn step(
    application: &AppHandle,
    key: ThreadKey,
    thread: &ThreadItem,
    channel: &Channel,
) -> Result<(), String> {
    // A thread in its 5 s undo window is left as it is; after Undo the next pass sees it.
    if lock(
        application
            .state::<crate::thread_menu::ManagedDeletions>()
            .inner(),
        "pending deletions",
    )?
    .is_pending(key)
    {
        return Ok(());
    }
    let (session_running, turn_in_progress) = {
        let runtimes = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?;
        (
            runtimes.is_live(key) && !runtimes.is_setting_up(key),
            runtimes.has_turn_in_progress(key),
        )
    };
    if let Some(change) = thread.rosen_change.as_deref()
        && step_rosen(
            application,
            key,
            thread,
            channel,
            change,
            session_running,
            turn_in_progress,
        )?
    {
        return Ok(());
    }
    let member = thread.airc_member();
    let wait = channel
        .wait_state(&member)
        .map_err(|error| error.to_string())?;
    let input = ParkingInput {
        state: thread.state,
        activity: thread.activity,
        session_running,
        wait,
    };
    if crate::goal_stack::continue_wait(application, key, thread, channel)? {
        return Ok(());
    }
    match parking_action(input) {
        ParkingAction::Park => park(application, key),
        ParkingAction::Wake => wake(application, key, &member, channel),
        ParkingAction::Nothing => Ok(()),
    }
}

/// Checks Rosen path waits for a Rosen thread. Returns `Ok(true)` when Rosen handled this
/// thread's wait state, or `Ok(false)` to fall back to any pre-existing legacy AIRC wait.
fn step_rosen(
    application: &AppHandle,
    key: ThreadKey,
    thread: &ThreadItem,
    channel: &Channel,
    change: &str,
    session_running: bool,
    turn_in_progress: bool,
) -> Result<bool, String> {
    let needs_check = thread.rosen_path_wait.is_some()
        || thread.activity == AgentActivity::Parked
        || (session_running && !turn_in_progress);
    if !needs_check {
        return Ok(false);
    }
    let active = tauri::async_runtime::block_on(async {
        let server = crate::rosen::server(application, key.0).await?;
        velocity_rosen::mcp_proxy::active_waits(&server, change)
            .await
            .map_err(|error| error.to_string())
    });
    let active = match active {
        Ok(active) => active,
        Err(_) => return Ok(thread.rosen_path_wait.is_some()),
    };
    let mut tracked_wait = thread.rosen_path_wait.is_some();
    if !active.is_empty() && !tracked_wait {
        let paths: Vec<String> = active.iter().map(|claim| claim.path.clone()).collect();
        sync_rosen_wait(application, key, paths)?;
        tracked_wait = true;
    }
    if active.is_empty() && !tracked_wait {
        return Ok(false);
    }
    if crate::goal_stack::continue_wait(application, key, thread, channel)? {
        return Ok(true);
    }
    let input = RosenParkingInput {
        state: thread.state,
        activity: thread.activity,
        session_running,
        turn_in_progress,
        has_active_waits: !active.is_empty(),
        tracked_wait,
    };
    match rosen_parking_action(input) {
        RosenParkingAction::Park => park(application, key)?,
        RosenParkingAction::Wake => wake_rosen(application, key)?,
        RosenParkingAction::ContinueLive => continue_live_rosen(application, key)?,
        RosenParkingAction::Nothing => {}
    }
    Ok(true)
}

fn sync_rosen_wait(
    application: &AppHandle,
    key: ThreadKey,
    paths: Vec<String>,
) -> Result<(), String> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |saved| {
        let wait = saved
            .rosen_path_wait
            .get_or_insert_with(velocity_store::rosen::RosenPathWait::default);
        wait.merge(paths, Vec::new());
        saved.dependency_wait = Some(wait.description());
    })
    .map_err(|error| error.to_string())?;
    crate::threads::refresh(application, key, false)?;
    Ok(())
}

fn take_rosen_wait(
    application: &AppHandle,
    key: ThreadKey,
) -> Result<Option<velocity_store::rosen::RosenPathWait>, String> {
    let mut taken = None;
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |saved| {
        taken = saved.rosen_path_wait.take();
        if taken.is_some() {
            saved.dependency_wait = None;
        }
    })
    .map_err(|error| error.to_string())?;
    Ok(taken)
}

fn wake_rosen(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    let lease = crate::threads::action_lock(application, key)?;
    let Ok(_guard) = lease.try_lock() else {
        return Ok(());
    };
    let Some(wait) = take_rosen_wait(application, key)? else {
        return Ok(());
    };
    tauri::async_runtime::block_on(crate::rosen_rebase::apply_at_boundary(application, key));
    if let Err(error) = resume(application, key, &rosen_resume_prompt(&wait)) {
        crate::threads::fail(
            application,
            key,
            "Could not resume the parked agent",
            &error,
        );
    }
    Ok(())
}

fn continue_live_rosen(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    let lease = crate::threads::action_lock(application, key)?;
    let Ok(_guard) = lease.try_lock() else {
        return Ok(());
    };
    let Some(wait) = take_rosen_wait(application, key)? else {
        return Ok(());
    };
    tauri::async_runtime::block_on(crate::rosen_rebase::apply_at_boundary(application, key));
    crate::threads::refresh(application, key, false)?;
    let prompt = rosen_resume_prompt(&wait);
    if let Err(error) = crate::threads::steer(application, key, &prompt) {
        crate::threads::fail(
            application,
            key,
            "Could not continue the waiting agent",
            &error,
        );
    }
    Ok(())
}

/// Ends the session and waits for its processes, including its AIRC server, to stop.
fn park(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    let Some(ended) = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .park(key) else {
        return Ok(());
    };
    ended
        .blocking_recv()
        .map_err(|error| format!("Agent shutdown was not confirmed: {error}"))?;
    let failed = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .shutdown_failed(key);
    if failed {
        crate::threads::fail(
            application,
            key,
            "Could not park the agent",
            "Its session could not confirm all of its processes stopped.",
        );
        return Ok(());
    }
    crate::threads::save_activity(application, key, AgentActivity::Parked, None)?;
    crate::threads::refresh(application, key, false)?;
    crate::threads::emit_status(application, key);
    Ok(())
}

/// Ends the wait the way the parked `airc_wait` call would have, then resumes the session.
fn wake(
    application: &AppHandle,
    key: ThreadKey,
    member: &str,
    channel: &Channel,
) -> Result<(), String> {
    let lease = crate::threads::action_lock(application, key)?;
    let Ok(_guard) = lease.try_lock() else {
        return Ok(());
    };
    // A wait record that cannot be read still resumes the agent: it re-reads its claims.
    let ended = match channel.wake_parked(member) {
        Ok(None) => return Ok(()),
        Ok(Some(ended)) => Some(ended),
        Err(_) => None,
    };
    // A Rosen dependency must be visible before the agent starts its next step.
    // The server's rebase may have arrived while this session was parked.
    tauri::async_runtime::block_on(crate::rosen_rebase::apply_at_boundary(application, key));
    if let Err(error) = resume(application, key, &resume_prompt(ended.as_ref())) {
        crate::threads::fail(
            application,
            key,
            "Could not resume the parked agent",
            &error,
        );
    }
    Ok(())
}

/// What a resumed session opens with.
enum Opening<'a> {
    /// A prompt from Velocity, sent ahead of the messages still queued.
    Prompt(&'a str),
    /// Queued runs saved before a Velocity restart (#516), started before opening a turn.
    RestoredRuns(Vec<velocity_core::queued_runs::QueuedRunRecord>),
    Handoff,
    AfterLimit(u32),
    /// A message you sent the ended thread. It joins the queue, so it is kept until the
    /// agent echoes it, however the new session ends (#317).
    Message(&'a str),
}

pub(crate) fn resume(application: &AppHandle, key: ThreadKey, prompt: &str) -> Result<(), String> {
    start_session(application, key, Opening::Prompt(prompt))
}

pub(crate) fn resume_restored_runs(
    application: &AppHandle,
    key: ThreadKey,
    runs: Vec<velocity_core::queued_runs::QueuedRunRecord>,
) -> Result<(), String> {
    start_session(application, key, Opening::RestoredRuns(runs))
}

/// Resumes an ended thread for a message you sent it, queued behind any earlier sends.
pub(crate) fn resume_with_message(
    application: &AppHandle,
    key: ThreadKey,
    message: &str,
) -> Result<(), String> {
    start_session(application, key, Opening::Message(message))
}

pub(crate) fn resume_handoff(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    start_session(application, key, Opening::Handoff)
}

pub(crate) fn resume_after_limit(
    application: &AppHandle,
    key: ThreadKey,
    account: u32,
) -> Result<(), String> {
    start_session(application, key, Opening::AfterLimit(account))
}

fn start_session(application: &AppHandle, key: ThreadKey, opening: Opening) -> Result<(), String> {
    let (project, mut thread, settings) = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let (project, thread) = store
            .follow_through_thread(key.0, key.1)
            .map_err(|error| error.to_string())?;
        (project, thread, store.settings())
    };
    if matches!(
        thread.state,
        ThreadState::Merged | ThreadState::Done | ThreadState::Spec | ThreadState::Draft
    ) {
        return Err("This thread cannot resume a build session".into());
    }
    let account = match opening {
        Opening::AfterLimit(account) => account,
        _ => settings.launch_account(
            thread.provider,
            thread.preferred_account.unwrap_or(thread.provider_account),
            velocity_store::providers::unix_now(),
        ),
    };
    thread.bind_provider_account(account);
    let path = thread.worktree_path.as_deref().ok_or("Missing worktree")?;
    let (mut agent, bypass_modes) = agent_launch_for_account(&settings, thread.provider, account)
        .map_err(|error| error.to_string())?;
    let mut setup = SessionSetup::new(path.into(), bypass_modes)
        .with_run_settings(velocity_threads::thread_run_settings(&thread));
    match crate::rosen::prepare_resume(application, key.0, &thread, &mut agent, &mut setup) {
        Some(prepared) => prepared?,
        None => velocity_git::verify_thread_worktree(
            Path::new(&project.repository_path),
            Path::new(path),
            &thread.branch_name,
        )
        .map_err(|error| error.to_string())?,
    }
    setup.airc = crate::airc_access::for_thread(application, key, Path::new(path), &thread);
    crate::airc_claims::attach(application, key, &setup, &thread.branch_name)?;
    reset_supervision(application, key, &thread)?;
    setup.pause_on_subscription_limit = true;
    setup.run_history = thread.queued_run_history.clone();
    let inputs = resume_inputs(application, key, path, opening, &thread, &mut setup)?;
    crate::threads::save_activity(application, key, AgentActivity::Thinking, None)?;
    crate::threads::refresh(application, key, false)?;
    crate::threads::emit_status(application, key);
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        crate::handoff::add_preface(&handle, key, &mut setup).await;
        crate::browser::configure(&handle, key, &mut setup);
        crate::threads::pump(&handle, key, spawn_session(agent, setup, inputs)).await;
    });
    Ok(())
}

fn reset_supervision(
    application: &AppHandle,
    key: ThreadKey,
    thread: &ThreadItem,
) -> Result<(), String> {
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    store
        .reclaim_agent_work(key.0, key.1)
        .map_err(|error| error.to_string())?;
    store
        .update_thread(key.0, key.1, |saved| {
            saved.activity = AgentActivity::Thinking;
            saved.bind_provider_account(thread.provider_account);
            saved.supervision.idle_nudged = false;
            saved.supervision.last_update = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn resume_inputs(
    application: &AppHandle,
    key: ThreadKey,
    path: &str,
    opening: Opening,
    thread: &ThreadItem,
    setup: &mut SessionSetup,
) -> Result<velocity_threads::SessionInputs, String> {
    let inputs = {
        let mut runtimes = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?;
        let blocks = runtimes
            .view(key)
            .map(|view| view.blocks)
            .unwrap_or_default();
        setup.resume = crate::handoff::resume_context(thread, &blocks);
        let follows_pull_request = thread.state == ThreadState::Review;
        match opening {
            Opening::Handoff => {
                let queued = blocks.iter().any(|block| matches!(
                    block, velocity_core::thread_view_model::TranscriptBlock::UserMessage { queued: true, .. }
                ));
                let opening = (!queued).then_some(velocity_threads::parking::HANDOFF_OPENING);
                runtimes.resume_parked(key, path, opening, follows_pull_request)
            }
            Opening::Prompt(prompt) => {
                runtimes.resume_parked(key, path, Some(prompt), follows_pull_request)
            }
            Opening::RestoredRuns(runs) => {
                setup.restored_runs = runs;
                runtimes.resume_parked(key, path, None, follows_pull_request)
            }
            Opening::AfterLimit(_) => runtimes.resume_parked(
                key, path, Some("Continue the interrupted work after the subscription limit. Recheck your claim and worktree, then resume from the transcript."), follows_pull_request,
            ),
            Opening::Message(message) => {
                runtimes.resume_with_message(key, path, message, follows_pull_request)
            }
        }
        .map_err(|error| error.to_string())?
    };
    Ok(inputs)
}
