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
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |thread| {
        if batch.is_empty() {
            velocity_threads::agent_activity::begin(thread, now());
        }
        for event in batch {
            velocity_threads::agent_activity::observe(thread, event, now());
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
    let (live, build) = {
        let runtimes = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?;
        (
            runtimes.is_live(key) && !runtimes.is_setting_up(key),
            runtimes
                .view(key)
                .is_some_and(|view| velocity_threads::agent_activity::in_build_queue(&view.blocks)),
        )
    };
    checkpoint_unavailable(application, key)?;
    let (_, current) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .follow_through_thread(key.0, key.1)
    .map_err(|error| error.to_string())?;
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
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
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
    lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
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
    post(channel, key, "at limit", &message)?;
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
