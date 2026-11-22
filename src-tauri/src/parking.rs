//! Parking edge (SPEC.md §2.2): ends a waiting agent's session and resumes it when its
//! wait ends. `velocity_threads::parking` decides; this module stops and starts sessions.
use std::path::Path;

use tauri::{AppHandle, Manager};
use velocity_acp::client::SessionSetup;
use velocity_airc::Channel;
use velocity_store::{AgentActivity, ProjectItem, ThreadItem, ThreadState};
use velocity_threads::parking::{
    ParkingAction, ParkingInput, ResumeError, ResumeOpeningKind, RosenParkingAction,
    RosenParkingInput, can_resume_build_session, check_resumable, drop_pending_deliveries,
    parking_action, resume_prompt, rosen_parking_action, rosen_wait_resume_prompt,
    should_break_mutual_overlap_wait,
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

struct RosenWaitCheck {
    has_active: bool,
    tracked: bool,
}

/// Syncs any active Rosen path waits (`rosen wait`) or open preflight overlap blockers onto
/// `key` and returns whether the thread has an active or tracked Rosen wait (#565).
pub(crate) fn sync_active_rosen_waits(
    application: &AppHandle,
    key: ThreadKey,
    change: &str,
) -> Result<bool, String> {
    let thread = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let Some(thread) = store
            .project(key.0)
            .and_then(|project| project.threads.iter().find(|item| item.id == key.1))
            .cloned()
        else {
            return Ok(false);
        };
        thread
    };
    let status = inspect_rosen_waits(application, key, &thread, change, true)?;
    Ok(status.has_active || status.tracked)
}

/// Checks Rosen path waits and preflight overlap waits for a Rosen thread. Returns `Ok(true)`
/// when Rosen handled this thread's wait state, or `Ok(false)` for legacy AIRC waits.
fn step_rosen(
    application: &AppHandle,
    key: ThreadKey,
    thread: &ThreadItem,
    channel: &Channel,
    change: &str,
    session_running: bool,
    turn_in_progress: bool,
) -> Result<bool, String> {
    let check_path_waits = thread.rosen_path_wait.is_some()
        || thread.activity == AgentActivity::Parked
        || (session_running && !turn_in_progress)
        || (thread.state == ThreadState::Working && thread.activity == AgentActivity::Idle);
    let check_overlap_waits =
        thread.rosen_overlap_wait.is_some() || thread.state == ThreadState::Working;
    if !check_path_waits && !check_overlap_waits {
        return Ok(false);
    }
    let status = inspect_rosen_waits(application, key, thread, change, check_path_waits)?;
    if !status.has_active && !status.tracked {
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
        has_active_waits: status.has_active,
        tracked_wait: status.tracked,
    };
    match rosen_parking_action(input) {
        RosenParkingAction::Park => park(application, key)?,
        RosenParkingAction::Wake => wake_rosen(application, key)?,
        RosenParkingAction::ContinueLive => continue_live_rosen(application, key)?,
        RosenParkingAction::Nothing => {}
    }
    Ok(true)
}

fn inspect_rosen_waits(
    application: &AppHandle,
    key: ThreadKey,
    thread: &ThreadItem,
    change: &str,
    check_path_waits: bool,
) -> Result<RosenWaitCheck, String> {
    let path_status = if check_path_waits {
        inspect_path_waits(application, key, thread, change)?
    } else {
        RosenWaitCheck {
            has_active: thread.rosen_path_wait.is_some(),
            tracked: thread.rosen_path_wait.is_some(),
        }
    };
    let overlap_status = inspect_overlap_waits(application, key, thread, change)?;
    Ok(RosenWaitCheck {
        has_active: path_status.has_active || overlap_status.has_active,
        tracked: path_status.tracked || overlap_status.tracked,
    })
}

fn inspect_path_waits(
    application: &AppHandle,
    key: ThreadKey,
    thread: &ThreadItem,
    change: &str,
) -> Result<RosenWaitCheck, String> {
    let queried = tauri::async_runtime::block_on(async {
        let server = crate::rosen::server(application, key.0).await?;
        let active = velocity_rosen::mcp_proxy::active_waits(&server, change)
            .await
            .map_err(|error| error.to_string())?;
        if active.is_empty() {
            return Ok::<_, String>((Vec::new(), Vec::new()));
        }
        let paths: Vec<String> = active.into_iter().map(|claim| claim.path).collect();
        let holders = if thread.rosen_path_wait.is_none() {
            velocity_rosen::mcp_proxy::wait_holders(&server, change, &paths)
                .await
                .unwrap_or_default()
                .into_iter()
                .map(|(other_change, owner)| {
                    let short = other_change.get(..8).unwrap_or(&other_change);
                    format!("{owner} ({short})")
                })
                .collect()
        } else {
            Vec::new()
        };
        Ok((paths, holders))
    });
    let (paths, holders) = match queried {
        Ok(result) => result,
        Err(_) => {
            let tracked = thread.rosen_path_wait.is_some();
            return Ok(RosenWaitCheck {
                has_active: tracked,
                tracked,
            });
        }
    };
    let mut tracked = thread.rosen_path_wait.is_some();
    if !paths.is_empty() && !tracked {
        sync_rosen_wait(application, key, paths.clone(), holders)?;
        tracked = true;
    }
    Ok(RosenWaitCheck {
        has_active: !paths.is_empty(),
        tracked,
    })
}

fn inspect_overlap_waits(
    application: &AppHandle,
    key: ThreadKey,
    thread: &ThreadItem,
    change: &str,
) -> Result<RosenWaitCheck, String> {
    let Ok(data_dir) = application.path().app_data_dir() else {
        let tracked = thread.rosen_overlap_wait.is_some();
        return Ok(RosenWaitCheck {
            has_active: tracked,
            tracked,
        });
    };
    let server = velocity_rosen::ProjectServer::new(&data_dir, key.0);
    let record = velocity_rosen::preflight::load_change_record(&server, change);
    if let Some(existing) = thread.rosen_overlap_wait.as_ref() {
        return inspect_tracked_overlap_wait(application, key, change, existing, record.as_ref());
    }
    let Some(record) = record.filter(|item| {
        thread.state == ThreadState::Working
            && item.failed_on_overlaps()
            && thread.supervision.last_overlap_wait_at != Some(item.recorded_at)
    }) else {
        return Ok(RosenWaitCheck {
            has_active: false,
            tracked: false,
        });
    };
    adopt_preflight_overlap_wait(application, key, &record)
}

fn adopt_preflight_overlap_wait(
    application: &AppHandle,
    key: ThreadKey,
    record: &velocity_core::preflight::PreflightRecord,
) -> Result<RosenWaitCheck, String> {
    let candidate_ids: Vec<String> = record
        .blocking_overlaps
        .iter()
        .map(|overlap| overlap.change.clone())
        .collect();
    let open = tauri::async_runtime::block_on(async {
        let server = crate::rosen::server(application, key.0).await?;
        velocity_rosen::preflight::open_change_ids(&server, &candidate_ids)
            .await
            .map_err(|error| error.to_string())
    });
    let Ok(open) = open else {
        return Ok(RosenWaitCheck {
            has_active: false,
            tracked: false,
        });
    };
    let active_overlaps: Vec<_> = record
        .blocking_overlaps
        .iter()
        .filter(|overlap| open.contains(&overlap.change))
        .cloned()
        .collect();
    if active_overlaps.is_empty() {
        return Ok(RosenWaitCheck {
            has_active: false,
            tracked: false,
        });
    }
    let wait = velocity_store::rosen::RosenOverlapWait::from_overlaps(
        &active_overlaps,
        record.recorded_at,
    );
    sync_overlap_wait(application, key, wait)?;
    Ok(RosenWaitCheck {
        has_active: true,
        tracked: true,
    })
}

fn inspect_tracked_overlap_wait(
    application: &AppHandle,
    key: ThreadKey,
    change: &str,
    existing: &velocity_store::rosen::RosenOverlapWait,
    record: Option<&velocity_core::preflight::PreflightRecord>,
) -> Result<RosenWaitCheck, String> {
    if let Some(latest) = record
        && latest.recorded_at > existing.preflight_recorded_at
        && !latest.failed_on_overlaps()
    {
        clear_overlap_wait(application, key)?;
        return Ok(RosenWaitCheck {
            has_active: false,
            tracked: false,
        });
    }
    if existing.blockers.is_empty() {
        return Ok(RosenWaitCheck {
            has_active: false,
            tracked: true,
        });
    }
    let blocker_ids = existing.blocker_change_ids();
    let open = tauri::async_runtime::block_on(async {
        let server = crate::rosen::server(application, key.0).await?;
        velocity_rosen::preflight::open_change_ids(&server, &blocker_ids)
            .await
            .map_err(|error| error.to_string())
    });
    let Ok(open) = open else {
        return Ok(RosenWaitCheck {
            has_active: true,
            tracked: true,
        });
    };
    let mut updated = existing.clone();
    let changed = updated.retain_open(&open);
    let remaining_ids = updated.blocker_change_ids();
    let mutual_break = !remaining_ids.is_empty()
        && is_mutual_overlap_deadlock(application, key.0, change, &remaining_ids)?;
    if mutual_break {
        updated.record_mutual_break();
    }
    if changed || mutual_break {
        update_overlap_wait(application, key, updated.clone())?;
    }
    Ok(RosenWaitCheck {
        has_active: !updated.blockers.is_empty(),
        tracked: true,
    })
}

fn is_mutual_overlap_deadlock(
    application: &AppHandle,
    project: u32,
    change: &str,
    open_blockers: &[String],
) -> Result<bool, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let Some(item) = store.project(project) else {
        return Ok(false);
    };
    let peers: Vec<(String, Vec<String>)> = item
        .threads
        .iter()
        .filter(|other| matches!(other.state, ThreadState::Working | ThreadState::Review))
        .filter_map(|other| {
            let other_change = other.rosen_change.as_ref()?;
            let other_wait = other.rosen_overlap_wait.as_ref()?;
            Some((other_change.clone(), other_wait.blocker_change_ids()))
        })
        .collect();
    let peer_slices: Vec<(&str, &[String])> = peers
        .iter()
        .map(|(other_change, targets)| (other_change.as_str(), targets.as_slice()))
        .collect();
    Ok(should_break_mutual_overlap_wait(
        change,
        open_blockers,
        &peer_slices,
    ))
}

fn sync_rosen_wait(
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
        saved.dependency_wait = saved.rosen_wait_description();
    })
    .map_err(|error| error.to_string())?;
    crate::threads::refresh(application, key, false)?;
    Ok(())
}

fn sync_overlap_wait(
    application: &AppHandle,
    key: ThreadKey,
    wait: velocity_store::rosen::RosenOverlapWait,
) -> Result<(), String> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |saved| {
        saved.supervision.last_overlap_wait_at = Some(wait.preflight_recorded_at);
        saved.supervision.idle_nudged = false;
        saved.supervision.idle_nudge_turns = None;
        saved.rosen_overlap_wait = Some(wait);
        saved.dependency_wait = saved.rosen_wait_description();
    })
    .map_err(|error| error.to_string())?;
    crate::threads::refresh(application, key, false)?;
    Ok(())
}

fn update_overlap_wait(
    application: &AppHandle,
    key: ThreadKey,
    wait: velocity_store::rosen::RosenOverlapWait,
) -> Result<(), String> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |saved| {
        saved.rosen_overlap_wait = Some(wait);
        saved.dependency_wait = saved.rosen_wait_description();
    })
    .map_err(|error| error.to_string())?;
    crate::threads::refresh(application, key, false)?;
    Ok(())
}

fn clear_overlap_wait(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |saved| {
        saved.rosen_overlap_wait = None;
        saved.dependency_wait = saved.rosen_wait_description();
    })
    .map_err(|error| error.to_string())?;
    crate::threads::refresh(application, key, false)?;
    Ok(())
}

struct TakenRosenWaits {
    path: Option<velocity_store::rosen::RosenPathWait>,
    overlap: Option<velocity_store::rosen::RosenOverlapWait>,
}

fn take_rosen_waits(
    application: &AppHandle,
    key: ThreadKey,
) -> Result<Option<TakenRosenWaits>, String> {
    let mut taken = None;
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |saved| {
        let path = saved.rosen_path_wait.take();
        let overlap = saved.rosen_overlap_wait.take();
        if path.is_some() || overlap.is_some() {
            saved.dependency_wait = None;
            saved.supervision.idle_nudged = false;
            saved.supervision.idle_nudge_turns = None;
            taken = Some(TakenRosenWaits { path, overlap });
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
    let Some(waits) = take_rosen_waits(application, key)? else {
        return Ok(());
    };
    tauri::async_runtime::block_on(crate::rosen_rebase::apply_at_boundary(application, key));
    let prompt = rosen_wait_resume_prompt(waits.path.as_ref(), waits.overlap.as_ref());
    match resume(application, key, &prompt) {
        Ok(()) => {}
        Err(error) if error.is_non_resumable() => {
            drop_non_resumable_delivery(application, key, &error);
        }
        Err(error) => crate::threads::fail(
            application,
            key,
            "Could not resume the parked agent",
            &error.to_string(),
        ),
    }
    Ok(())
}

fn continue_live_rosen(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    let lease = crate::threads::action_lock(application, key)?;
    let Ok(_guard) = lease.try_lock() else {
        return Ok(());
    };
    let Some(waits) = take_rosen_waits(application, key)? else {
        return Ok(());
    };
    tauri::async_runtime::block_on(crate::rosen_rebase::apply_at_boundary(application, key));
    crate::threads::refresh(application, key, false)?;
    let prompt = rosen_wait_resume_prompt(waits.path.as_ref(), waits.overlap.as_ref());
    match crate::threads::steer_checked(application, key, &prompt) {
        Ok(_) => {}
        Err(error) if error.is_non_resumable() => {
            drop_non_resumable_delivery(application, key, &error);
        }
        Err(error) => crate::threads::fail(
            application,
            key,
            "Could not continue the waiting agent",
            &error.to_string(),
        ),
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
    match resume(application, key, &resume_prompt(ended.as_ref())) {
        Ok(()) => {}
        Err(error) if error.is_non_resumable() => {
            drop_non_resumable_delivery(application, key, &error);
        }
        Err(error) => crate::threads::fail(
            application,
            key,
            "Could not resume the parked agent",
            &error.to_string(),
        ),
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

impl Opening<'_> {
    const fn kind(&self) -> ResumeOpeningKind {
        match self {
            Self::Prompt(_) => ResumeOpeningKind::Prompt,
            Self::RestoredRuns(_) => ResumeOpeningKind::RestoredRuns,
            Self::Handoff => ResumeOpeningKind::Handoff,
            Self::AfterLimit(_) => ResumeOpeningKind::AfterLimit,
            Self::Message(_) => ResumeOpeningKind::Message,
        }
    }
}

/// Records one diagnostic line naming the thread and opening kind, then drops any pending
/// deliveries so a Merged, Done, Spec or Draft thread is never retried (#585).
pub(crate) fn drop_non_resumable_delivery(
    application: &AppHandle,
    key: ThreadKey,
    error: &ResumeError,
) {
    crate::diagnostics::record("thread runtime", &error.to_string());
    let updated = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()
    .and_then(|mut store| {
        let mut changed = false;
        let saved = store.update_thread(key.0, key.1, |thread| {
            if !can_resume_build_session(thread.state) {
                changed = drop_pending_deliveries(thread);
            }
        });
        saved.ok().map(|_| changed)
    })
    .unwrap_or(false);
    if updated {
        let _ = crate::threads::refresh(application, key, false);
        crate::threads::emit_status(application, key);
    }
}

pub(crate) fn resume(
    application: &AppHandle,
    key: ThreadKey,
    prompt: &str,
) -> Result<(), ResumeError> {
    start_session(application, key, Opening::Prompt(prompt))
}

pub(crate) fn resume_restored_runs(
    application: &AppHandle,
    key: ThreadKey,
    runs: Vec<velocity_core::queued_runs::QueuedRunRecord>,
) -> Result<(), ResumeError> {
    start_session(application, key, Opening::RestoredRuns(runs))
}

/// Resumes an ended thread for a message you sent it, queued behind any earlier sends.
pub(crate) fn resume_with_message(
    application: &AppHandle,
    key: ThreadKey,
    message: &str,
) -> Result<(), ResumeError> {
    start_session(application, key, Opening::Message(message))
}

pub(crate) fn resume_handoff(application: &AppHandle, key: ThreadKey) -> Result<(), ResumeError> {
    start_session(application, key, Opening::Handoff)
}

pub(crate) fn resume_after_limit(
    application: &AppHandle,
    key: ThreadKey,
    account: u32,
) -> Result<(), ResumeError> {
    start_session(application, key, Opening::AfterLimit(account))
}

fn start_session(
    application: &AppHandle,
    key: ThreadKey,
    opening: Opening,
) -> Result<(), ResumeError> {
    let (project, thread, settings) = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )
        .map_err(ResumeError::Other)?;
        let (project, thread) = store
            .follow_through_thread(key.0, key.1)
            .map_err(|error| ResumeError::Other(error.to_string()))?;
        (project, thread, store.settings())
    };
    check_resumable(key, thread.state, opening.kind())?;
    launch_resumed_session(application, key, project, thread, settings, opening)
        .map_err(ResumeError::Other)
}

fn launch_resumed_session(
    application: &AppHandle,
    key: ThreadKey,
    project: ProjectItem,
    mut thread: ThreadItem,
    settings: velocity_store::Settings,
    opening: Opening,
) -> Result<(), String> {
    let account = match opening {
        Opening::AfterLimit(account) => account,
        _ => settings.launch_account(
            thread.provider,
            thread.preferred_account.unwrap_or(thread.provider_account),
            velocity_store::providers::unix_now(),
        ),
    };
    thread.bind_provider_account(account);
    let read_only = thread.role.is_read_only();
    let path = if read_only {
        project.repository_path.as_str()
    } else {
        thread.worktree_path.as_deref().ok_or("Missing worktree")?
    };
    let (mut agent, bypass_modes) = agent_launch_for_account(&settings, thread.provider, account)
        .map_err(|error| error.to_string())?;
    let mut setup = SessionSetup::new(path.into(), bypass_modes)
        .with_run_settings(velocity_threads::thread_run_settings(&thread));
    setup.role = thread.role;
    setup.read_only = read_only;
    if read_only {
        if crate::rosen::uses_rosen(application, key.0) {
            agent.env.push((
                String::from(velocity_rosen::agent_run::BLOCK_VCS_ENV),
                String::from("1"),
            ));
            setup
                .shared_servers
                .extend(crate::rosen::read_only_mcp_server(application, key.0));
        }
        setup.preface = Some(crate::threads::QUESTION_PREFACE.into());
        setup.airc = crate::airc_access::for_thread(application, key, Path::new(path), &thread);
    } else {
        match crate::rosen::prepare_resume(application, key.0, &thread, &mut agent, &mut setup) {
            Some(prepared) => prepared?,
            None => {
                return Err(
                    "This thread has no Rosen workspace; finish migrating the project to Rosen before resuming."
                        .into(),
                );
            }
        }
        setup.airc = crate::airc_access::for_thread(application, key, Path::new(path), &thread);
        crate::airc_claims::attach(application, key, &setup, &thread.branch_name)?;
    }
    reset_supervision(application, key, &thread)?;
    setup.pause_on_subscription_limit = true;
    setup.run_history = thread.queued_run_history.clone();
    let inputs = resume_inputs(application, key, path, opening, &thread, &mut setup)?;
    crate::threads::save_activity(application, key, AgentActivity::Thinking, None)?;
    crate::threads::refresh(application, key, false)?;
    crate::threads::emit_status(application, key);
    let handle = application.clone();
    let title = thread.title.clone();
    tauri::async_runtime::spawn(async move {
        if !read_only {
            crate::handoff::add_preface(&handle, key, &mut setup).await;
            crate::browser::configure_for_prompt(&handle, key, &title, "", &mut setup);
        }
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
