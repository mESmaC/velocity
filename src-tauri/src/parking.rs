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

fn resume(application: &AppHandle, key: ThreadKey, prompt: &str) -> Result<(), String> {
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
    let path = thread.worktree_path.as_deref().ok_or("Missing worktree")?;
    velocity_git::verify_thread_worktree(
        Path::new(&project.repository_path),
        Path::new(path),
        &thread.branch_name,
    )
    .map_err(|error| error.to_string())?;
    let (agent, bypass_modes) =
        agent_launch(&settings, thread.provider).map_err(|error| error.to_string())?;
    let mut setup = SessionSetup::new(path.into(), bypass_modes)
        .with_run_settings(velocity_threads::thread_run_settings(&thread));
    setup.airc = crate::airc_access::managed(application)
        .map(|airc| airc.thread(key.0, key.1, Path::new(path)));
    crate::airc_claims::attach(application, key, &setup, &thread.branch_name)?;
    let inputs = {
        let mut runtimes = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?;
        let blocks = runtimes
            .view(key)
            .map(|view| view.blocks)
            .unwrap_or_default();
        setup.resume = Some(SessionResume::new(
            thread.agent_session.clone(),
            transcript_context(&blocks),
        ));
        runtimes
            .resume_parked(key, path, prompt, thread.state == ThreadState::Review)
            .map_err(|error| error.to_string())?
    };
    crate::threads::save_activity(application, key, AgentActivity::Thinking, None)?;
    crate::threads::refresh(application, key, false)?;
    crate::threads::emit_status(application, key);
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        crate::threads::pump(&handle, key, spawn_session(agent, setup, inputs)).await;
    });
    Ok(())
}
