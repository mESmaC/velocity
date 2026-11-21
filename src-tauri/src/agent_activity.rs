//! Timer and process edges for the §2.2 supervision reducer.
use crate::{
    state::ManagedStateStore,
    threads::{ManagedThreads, lock},
};
use tauri::{AppHandle, Manager};
use velocity_acp::client::AgentEvent;
use velocity_airc::{Channel, Event};
use velocity_store::{AgentActivity, ProjectItem};
use velocity_threads::{
    ThreadKey,
    agent_activity::{
        Action, capped, decide_with_turn, idle_nudge_prompt_for, is_dispatch_or_issue,
        last_wait_description,
    },
};

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

pub fn observe(
    application: &AppHandle,
    key: ThreadKey,
    batch: &[AgentEvent],
) -> Result<(), String> {
    let timestamp = now();
    // Keep the runtime lock through checkpointing so a nudge cannot race its reset.
    let mut runtimes = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?;
    if !runtimes.observe_supervision(key, batch, timestamp) {
        return Ok(());
    }
    let worked = batch
        .iter()
        .any(|event| matches!(event, AgentEvent::TurnEnded(_)))
        && runtimes.take_supervision_worked(key);
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let mut reached_limit = None;
    store
        .update_thread(key.0, key.1, |thread| {
            if batch.is_empty() {
                velocity_threads::agent_activity::begin(thread, timestamp);
            }
            let previous = thread.supervision.retry_after;
            for event in batch {
                velocity_threads::agent_activity::observe(thread, event, timestamp);
            }
            if worked && !is_dispatch_or_issue(thread) {
                thread.supervision.idle_nudged = false;
                thread.supervision.idle_nudge_turns = None;
            }
            if thread.supervision.retry_after != previous
                && let Some(until) = thread
                    .supervision
                    .retry_after
                    .and_then(|until| u32::try_from(until).ok())
            {
                reached_limit = Some((thread.provider, thread.provider_account, until));
            }
        })
        .map_err(|error| error.to_string())?;
    if let Some((provider, account, until)) = reached_limit {
        store
            .update_provider_account(provider, account, |profile| {
                profile.blocked_until = Some(until.max(profile.blocked_until.unwrap_or(0)));
            })
            .map_err(|error| error.to_string())?;
        drop(store);
        drop(runtimes);
        let _ = crate::providers::current_view(application);
    }
    Ok(())
}

pub fn cycle(
    application: &AppHandle,
    project: &ProjectItem,
    channel: &Channel,
) -> Result<(), String> {
    let settings = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .settings()
    .agents;
    for thread in &project.threads {
        if let Err(error) = step(application, (project.id, thread.id), channel, &settings) {
            eprintln!("Velocity agent supervision: {error}");
        }
    }
    Ok(())
}

fn step(
    application: &AppHandle,
    key: ThreadKey,
    channel: &Channel,
    settings: &velocity_store::AgentSettings,
) -> Result<(), String> {
    let lease = crate::threads::action_lock(application, key)?;
    let Ok(_guard) = lease.try_lock() else {
        return Ok(());
    };
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
    let (live, build, turn_in_progress, heartbeat) = {
        let runtimes = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?;
        let turn_in_progress = runtimes.has_turn_in_progress(key);
        (
            runtimes.is_live(key) && !runtimes.is_setting_up(key),
            // A subagent in the build queue keeps its parent out of the stall clock too.
            // Tool rows left running after a turn ends are stale and never exempt an idle
            // thread; real queued runs are tracked in `thread.queued_runs` (#544).
            turn_in_progress
                && (runtimes.view(key).is_some_and(|view| {
                    velocity_threads::agent_activity::in_build_queue(&view.blocks)
                }) || runtimes.subagent_in_build_queue(key)),
            turn_in_progress,
            runtimes.heartbeat(key),
        )
    };
    checkpoint_unavailable(application, key)?;
    let mut current = supervised_thread(application, key)?;
    if let Some(heartbeat) = heartbeat {
        current.supervision.last_update = current.supervision.last_update.max(heartbeat);
    }
    let action = decide_with_turn(&current, settings, now(), live, build, turn_in_progress);
    if !live
        && current.activity == AgentActivity::AtLimit
        && current.supervision.retry_after.is_some()
        && !capped(&current, settings, now())
        && current.worktree_path.is_some()
        && matches!(
            current.state,
            velocity_store::ThreadState::Working | velocity_store::ThreadState::Review
        )
        && let Some(account) = available_account(application, &current)?
    {
        if let Err(error) = crate::parking::resume_after_limit(application, key, account) {
            crate::threads::fail(application, key, "Could not resume the agent", &error);
        }
        return Ok(());
    }
    if action == Action::Resume && current.supervision.retry_after.is_some() {
        return Ok(());
    }
    execute(application, key, channel, settings, action, live)
}

fn available_account(
    application: &AppHandle,
    thread: &velocity_store::ThreadItem,
) -> Result<Option<u32>, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let timestamp = velocity_store::providers::unix_now();
    let settings = store.settings();
    let preferred = thread.provider_account;
    let accounts = settings.provider_accounts(thread.provider);
    let account = settings.available_account(thread.provider, preferred, timestamp);
    let eligible = |account: u32| {
        account != preferred
            || thread
                .supervision
                .retry_after
                .is_none_or(|retry| retry <= u64::from(timestamp))
    };
    Ok(account.filter(|account| eligible(*account)).or_else(|| {
        accounts
            .into_iter()
            .find(|account| eligible(account.id) && account.available_at(timestamp))
            .map(|account| account.id)
    }))
}

/// A failed setup has no worktree, so supervision must not use the follow-through lookup.
fn supervised_thread(
    application: &AppHandle,
    key: ThreadKey,
) -> Result<velocity_store::ThreadItem, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let project = store
        .project(key.0)
        .ok_or_else(|| format!("Project {} is not registered", key.0))?;
    velocity_threads::agent_activity::supervised_thread(project, key.1).map_err(str::to_owned)
}

fn execute(
    application: &AppHandle,
    key: ThreadKey,
    channel: &Channel,
    settings: &velocity_store::AgentSettings,
    action: Action,
    live: bool,
) -> Result<(), String> {
    match action {
        Action::Nothing => Ok(()),
        Action::Resume => {
            if let Err(error) = crate::parking::resume(
                application,
                key,
                "Retry after the provider usage limit. Reclaim the piece before continuing.",
            ) {
                crate::threads::fail(application, key, "Could not resume the agent", &error);
            }
            Ok(())
        }
        Action::Nudge => nudge(application, key, channel),
        Action::IdleNudge => idle_nudge(application, key, channel, settings, live),
        Action::Restart => restart(application, key, channel),
        Action::Limit => limit(application, key, channel),
        Action::Release => release(application, key, channel),
    }
}

fn checkpoint_unavailable(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let current = store
        .project(key.0)
        .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
        .ok_or("Missing supervision thread")?;
    let mut next = current.clone();
    velocity_threads::agent_activity::checkpoint_unavailable(&mut next, now());
    if current.supervision.unavailable_since == next.supervision.unavailable_since {
        return Ok(());
    }
    store
        .update_thread(key.0, key.1, |thread| {
            velocity_threads::agent_activity::checkpoint_unavailable(thread, now());
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn post(
    application: &AppHandle,
    channel: &Channel,
    key: ThreadKey,
    tag: &str,
    text: &str,
) -> Result<(), String> {
    let (member, fallback_nick, change) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()
    .and_then(|store| {
        let thread = store
            .project(key.0)?
            .threads
            .iter()
            .find(|thread| thread.id == key.1)?;
        Some((
            thread.airc_member(),
            thread.agent_name(),
            thread.rosen_change.clone(),
        ))
    })
    .unwrap_or_else(|| {
        let legacy = format!("thread-{}", key.1);
        (legacy.clone(), legacy, None)
    });
    let nick = channel
        .claims()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|claim| claim.member == member)
        .map(|claim| claim.nick)
        .unwrap_or(fallback_nick);
    let mut event = Event::new(tag, &nick, text);
    if let Some(change) = change {
        event = event.for_change(&change);
    }
    channel
        .post_event(event)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn nudge(application: &AppHandle, key: ThreadKey, channel: &Channel) -> Result<(), String> {
    let mut runtimes = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?;
    runtimes
        .nudge(
            key,
            "No output for the stall timeout. Continue, or say what is blocking you.",
        )
        .map_err(|error| error.to_string())?;
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |thread| {
        thread.supervision.nudged = true;
        thread.supervision.last_update = now();
        thread.activity = AgentActivity::Stalled;
    })
    .map_err(|error| error.to_string())?;
    runtimes.nudge_supervision(key, now());
    drop(runtimes);
    post(
        application,
        channel,
        key,
        "stalled",
        "No output; sent a recovery prompt",
    )?;
    publish(application, key)
}

fn idle_nudge(
    application: &AppHandle,
    key: ThreadKey,
    channel: &Channel,
    settings: &velocity_store::AgentSettings,
    live: bool,
) -> Result<(), String> {
    let thread = supervised_thread(application, key)?;
    if thread.dependency_wait.is_some() || thread.has_rosen_wait() {
        return Ok(());
    }
    if let Some(change) = thread.rosen_change.as_deref() {
        if crate::parking::sync_active_rosen_waits(application, key, change)? {
            return Ok(());
        }
        if is_dispatch_or_issue(&thread) && !unnamed_open_change(application, key.0, change)? {
            return Ok(());
        }
    }
    let turns_before = thread.supervision.turns;
    let blocks = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .view(key)
    .map(|view| view.blocks)
    .unwrap_or_default();
    let last_wait = last_wait_description(&thread, &blocks);
    let prompt = idle_nudge_prompt_for(&thread, settings, last_wait.as_deref());
    let timestamp = now();
    if live {
        lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?
        .nudge(key, &prompt)
        .map_err(|error| error.to_string())?;
        record_idle_nudge(application, key, timestamp, turns_before)?;
        publish(application, key)?;
    } else {
        crate::parking::resume(application, key, &prompt)?;
        record_idle_nudge(application, key, timestamp, turns_before)?;
    }
    let note = match last_wait {
        Some(wait) => format!(
            "Idle in working with nothing outstanding (last waited on: {wait}); sent a recovery prompt"
        ),
        None => "Idle in working with nothing outstanding; sent a recovery prompt".into(),
    };
    post(application, channel, key, "stalled", &note)
}

fn unnamed_open_change(
    application: &AppHandle,
    project: u32,
    change: &str,
) -> Result<bool, String> {
    tauri::async_runtime::block_on(async {
        let server = crate::rosen::server(application, project).await?;
        velocity_rosen::dispatch::is_unnamed_open_change(&server, change, None)
            .await
            .map_err(|error| error.to_string())
    })
}

fn record_idle_nudge(
    application: &AppHandle,
    key: ThreadKey,
    timestamp: u64,
    turns_before: u32,
) -> Result<(), String> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |saved| {
        saved.supervision.idle_nudged = true;
        saved.supervision.idle_nudge_turns = Some(turns_before);
        saved.supervision.last_update = timestamp;
    })
    .map_err(|error| error.to_string())?;
    lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .idle_nudge_supervision(key, timestamp);
    Ok(())
}

fn end(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    let ended = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .park(key);
    if let Some(ended) = ended {
        ended.blocking_recv().map_err(|error| error.to_string())?;
    }
    if lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .shutdown_failed(key)
    {
        return Err("Agent shutdown was not confirmed".into());
    }
    Ok(())
}

fn restart(application: &AppHandle, key: ThreadKey, channel: &Channel) -> Result<(), String> {
    end(application, key)?;
    post(
        application,
        channel,
        key,
        "stalled",
        "Still silent after nudge; restarting in the same workspace",
    )?;
    crate::parking::resume(
        application,
        key,
        "You were restarted after remaining silent. Continue from your transcript and retained claims.",
    )
}

fn limit(application: &AppHandle, key: ThreadKey, channel: &Channel) -> Result<(), String> {
    let message = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let (_, thread) = store
            .follow_through_thread(key.0, key.1)
            .map_err(|error| error.to_string())?;
        velocity_threads::agent_activity::limit_message(&thread, &store.settings().agents)
    };
    end(application, key)?;
    crate::threads::save_activity(application, key, AgentActivity::AtLimit, None)?;
    post(application, channel, key, "at limit", &message)?;
    publish(application, key)
}

fn release(application: &AppHandle, key: ThreadKey, channel: &Channel) -> Result<(), String> {
    let thread = supervised_thread(application, key)?;
    let pushed = velocity_threads::agent_activity::pushed_commit(&thread);
    velocity_threads::agent_activity::release_claims(channel, &thread, pushed.as_deref())
        .map_err(|error| error.to_string())?;
    let _ = channel.leave(&thread.airc_member());
    let _ = channel.leave(&format!("thread-{}", key.1));
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .release_agent_work(key.0, key.1, pushed)
    .map_err(|error| error.to_string())?;
    let note = if thread.worktree_path.is_some() {
        "Piece returned to the pool; workspace kept"
    } else {
        "Piece returned to the pool; setup left no workspace"
    };
    post(application, channel, key, "released", note)?;
    publish(application, key)
}

fn publish(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    crate::threads::refresh(application, key, false)?;
    crate::threads::emit_status(application, key);
    Ok(())
}
