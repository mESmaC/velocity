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
    agent_activity::{Action, decide},
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
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |thread| {
        if batch.is_empty() {
            velocity_threads::agent_activity::begin(thread, timestamp);
        }
        for event in batch {
            velocity_threads::agent_activity::observe(thread, event, timestamp);
        }
    })
    .map(|_| ())
    .map_err(|error| error.to_string())
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
    let (live, build, heartbeat) = {
        let runtimes = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?;
        (
            runtimes.is_live(key) && !runtimes.is_setting_up(key),
            runtimes
                .view(key)
                .is_some_and(|view| velocity_threads::agent_activity::in_build_queue(&view.blocks)),
            runtimes.heartbeat(key),
        )
    };
    checkpoint_unavailable(application, key)?;
    let (_, mut current) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .follow_through_thread(key.0, key.1)
    .map_err(|error| error.to_string())?;
    if let Some(heartbeat) = heartbeat {
        current.supervision.last_update = current.supervision.last_update.max(heartbeat);
    }
    execute(
        application,
        key,
        channel,
        decide(&current, settings, now(), live, build),
    )
}

fn execute(
    application: &AppHandle,
    key: ThreadKey,
    channel: &Channel,
    action: Action,
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

fn post(channel: &Channel, key: ThreadKey, tag: &str, text: &str) -> Result<(), String> {
    let member = format!("thread-{}", key.1);
    let nick = channel
        .claims()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|claim| claim.member == member)
        .map(|claim| claim.nick)
        .unwrap_or(member);
    channel
        .post_event(Event::new(tag, &nick, text))
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
    post(channel, key, "stalled", "No output; sent a recovery prompt")?;
    publish(application, key)
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
        channel,
        key,
        "stalled",
        "Still silent after nudge; restarting in the same worktree",
    )?;
    crate::parking::resume(
        application,
        key,
        "You were restarted after remaining silent. Continue from your transcript and retained claims.",
    )
}

fn limit(application: &AppHandle, key: ThreadKey, channel: &Channel) -> Result<(), String> {
    end(application, key)?;
    crate::threads::save_activity(application, key, AgentActivity::AtLimit, None)?;
    post(
        channel,
        key,
        "at limit",
        "Per-agent cap reached; session stopped and worktree kept",
    )?;
    publish(application, key)
}

fn release(application: &AppHandle, key: ThreadKey, channel: &Channel) -> Result<(), String> {
    let (_, thread) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .follow_through_thread(key.0, key.1)
    .map_err(|error| error.to_string())?;
    let pushed = velocity_threads::agent_activity::pushed_commit(&thread);
    velocity_threads::agent_activity::release_claims(channel, &thread, pushed.as_deref())
        .map_err(|error| error.to_string())?;
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .release_agent_work(key.0, key.1, pushed)
    .map_err(|error| error.to_string())?;
    post(
        channel,
        key,
        "released",
        "Piece returned to the pool; worktree kept",
    )?;
    publish(application, key)
}

fn publish(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    crate::threads::refresh(application, key, false)?;
    crate::threads::emit_status(application, key);
    Ok(())
}
