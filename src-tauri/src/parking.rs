//! Parking edge (SPEC.md §2.2): ends a waiting agent's session and resumes it when its
//! wait ends. `velocity_threads::parking` decides; this module stops and starts sessions.
use std::path::Path;

use tauri::{AppHandle, Manager};
use velocity_acp::client::{SessionResume, SessionSetup};
use velocity_airc::Channel;
use velocity_store::{AgentActivity, ProjectItem, ThreadItem, ThreadState};
use velocity_threads::parking::{
    ParkingAction, ParkingInput, parking_action, resume_prompt, transcript_context,
};
use velocity_threads::{ThreadKey, agent_launch, spawn_session};

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
    let session_running = {
        let runtimes = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?;
        runtimes.is_live(key) && !runtimes.is_setting_up(key)
    };
    let wait = channel
        .wait_state(&member(key))
        .map_err(|error| error.to_string())?;
    let input = ParkingInput {
        state: thread.state,
        activity: thread.activity,
        session_running,
        wait,
    };
    match parking_action(input) {
        ParkingAction::Park => park(application, key),
        ParkingAction::Wake => wake(application, key, channel),
        ParkingAction::Nothing => Ok(()),
    }
}

fn member(key: ThreadKey) -> String {
    format!("thread-{}", key.1)
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
fn wake(application: &AppHandle, key: ThreadKey, channel: &Channel) -> Result<(), String> {
    // A wait record that cannot be read still resumes the agent: it re-reads its claims.
    let ended = match channel.wake_parked(&member(key)) {
        Ok(None) => return Ok(()),
        Ok(Some(ended)) => Some(ended),
        Err(_) => None,
    };
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
#[derive(Clone, Copy)]
enum Opening<'a> {
    /// A prompt from Velocity, sent ahead of the messages still queued.
    Prompt(&'a str),
    /// A message you sent the ended thread. It joins the queue, so it is kept until the
    /// agent echoes it, however the new session ends (#317).
    Message(&'a str),
}

pub(crate) fn resume(application: &AppHandle, key: ThreadKey, prompt: &str) -> Result<(), String> {
    start_session(application, key, Opening::Prompt(prompt))
}

/// Resumes an ended thread for a message you sent it, queued behind any earlier sends.
pub(crate) fn resume_with_message(
    application: &AppHandle,
    key: ThreadKey,
    message: &str,
) -> Result<(), String> {
    start_session(application, key, Opening::Message(message))
}

fn start_session(application: &AppHandle, key: ThreadKey, opening: Opening) -> Result<(), String> {
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
    if matches!(
        thread.state,
        ThreadState::Merged | ThreadState::Done | ThreadState::Spec | ThreadState::Draft
    ) {
        return Err("This thread cannot resume a build session".into());
    }
    let path = thread.worktree_path.as_deref().ok_or("Missing worktree")?;
    let (mut agent, bypass_modes) =
        agent_launch(&settings, thread.provider).map_err(|error| error.to_string())?;
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
    setup.airc = crate::airc_access::managed(application)
        .map(|airc| airc.thread(key.0, key.1, Path::new(path)));
    crate::airc_claims::attach(application, key, &setup, &thread.branch_name)?;
    reset_supervision(application, key)?;
    let inputs = resume_inputs(application, key, path, opening, &thread, &mut setup)?;
    crate::threads::save_activity(application, key, AgentActivity::Thinking, None)?;
    crate::threads::refresh(application, key, false)?;
    crate::threads::emit_status(application, key);
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        crate::threads::pump(&handle, key, spawn_session(agent, setup, inputs)).await;
    });
    Ok(())
}

fn reset_supervision(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
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
        setup.resume = Some(
            SessionResume::new(thread.agent_session.clone(), transcript_context(&blocks))
                .with_history_replay(blocks.is_empty()),
        );
        let follows_pull_request = thread.state == ThreadState::Review;
        match opening {
            Opening::Prompt(prompt) => {
                runtimes.resume_parked(key, path, Some(prompt), follows_pull_request)
            }
            Opening::Message(message) => {
                runtimes.resume_with_message(key, path, message, follows_pull_request)
            }
        }
        .map_err(|error| error.to_string())?
    };
    Ok(inputs)
}
