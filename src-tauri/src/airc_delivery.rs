//! Native F14 message edge: queued input, persistent acknowledgement and user comments.
use std::collections::BTreeSet;

use tauri::{AppHandle, Manager};
use velocity_airc::Channel;
use velocity_store::{ProjectItem, ThreadItem};
use velocity_threads::DeliveryStatus;

use crate::state::ManagedStateStore;
use crate::threads::{ManagedThreads, lock};

fn channel(application: &AppHandle, project: u32) -> Result<Channel, String> {
    let directory = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    Channel::open(&directory, &project.to_string()).map_err(|error| error.to_string())
}

pub fn initialize(application: &AppHandle) {
    let application = application.clone();
    std::thread::spawn(move || {
        let mut left = Left::new();
        let mut pass: u64 = 0;
        loop {
            pass = pass.wrapping_add(1);
            // A member can rejoin without this loop seeing its thread live (a resume
            // that fails between passes), so the set is cleared now and then: a missed
            // leave is at most this many seconds late.
            if pass.is_multiple_of(LEFT_RECHECK_PASSES) {
                left.clear();
            }
            if let Err(error) = cycle(&application, &mut left) {
                eprintln!("Velocity AIRC delivery: {error}");
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
}

/// Threads whose member this loop has already taken out of the channel. A stopped thread
/// leaves once, not every second: each leave locks and reads the channel (#322).
type Left = BTreeSet<velocity_threads::ThreadKey>;

const LEFT_RECHECK_PASSES: u64 = 30;

fn cycle(application: &AppHandle, left: &mut Left) -> Result<(), String> {
    // Transcripts are shared, not copied, so this clone copies thread records only.
    let projects = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .state()
    .projects
    .clone();
    for project in projects {
        if let Err(error) = project_cycle(application, &project, left) {
            eprintln!("Velocity AIRC project {}: {error}", project.id);
        }
    }
    Ok(())
}

fn project_cycle(
    application: &AppHandle,
    project: &ProjectItem,
    left: &mut Left,
) -> Result<(), String> {
    // One handle for the whole pass: opening resolves and creates the folder each time.
    let channel = channel(application, project.id)?;
    let changed = velocity_threads::agent_tasks::ingest_shared(
        application.state::<ManagedStateStore>().inner(),
        project.id,
        &channel,
    )
    .map_err(|error| error.to_string())?;
    if changed {
        crate::threads::refresh(
            application,
            (project.id, project.active_thread_id.unwrap_or(0)),
            false,
        )?;
    }
    if let Err(error) = update_waits(application, project, &channel) {
        eprintln!("Velocity AIRC project {} waits: {error}", project.id);
    }
    crate::agent_activity::cycle(application, project, &channel)?;
    crate::parking::cycle(application, project, &channel);
    for thread in &project.threads {
        if let Err(error) = deliver(application, (project.id, thread), &channel, left) {
            eprintln!("Velocity AIRC thread {}: {error}", thread.id);
        }
    }
    Ok(())
}

fn deliver(
    application: &AppHandle,
    (project, thread): (u32, &ThreadItem),
    channel: &Channel,
    left: &mut Left,
) -> Result<(), String> {
    let key = (project, thread.id);
    let member = format!("thread-{}", key.1);
    let (live, setting_up) = {
        let runtimes = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?;
        (runtimes.is_live(key), runtimes.is_setting_up(key))
    };
    if !live {
        if left.contains(&key) {
            return Ok(());
        }
        channel.leave(&member).map_err(|error| error.to_string())?;
        left.insert(key);
        return Ok(());
    }
    left.remove(&key);
    if setting_up {
        return Ok(());
    }
    let Some(path) = &thread.worktree_path else {
        return Ok(());
    };
    let nick = channel
        .join(&member, path)
        .map_err(|error| error.to_string())?;
    let cursor = velocity_airc::deliver_targeted(
        channel,
        &nick,
        channel
            .delivery_cursor(&member, thread.airc_cursor)
            .map_err(|error| error.to_string())?,
        thread.activity == velocity_store::AgentActivity::Idle,
        |identifier, message| {
            lock(
                application.state::<ManagedThreads>().inner(),
                "thread sessions",
            )?
            .deliver_follow_up(key, identifier, message)
            .map(|status| status == DeliveryStatus::Delivered)
            .map_err(|error| error.to_string())
        },
    )
    .map_err(|error| error.to_string())?;
    if cursor != thread.airc_cursor {
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .update_thread(key.0, key.1, |saved| saved.airc_cursor = cursor)
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Translate the runner's saved lifecycle into wake events. The waiting MCP server
/// subscribes to these writes; it never polls GitHub or the runner.
fn update_waits(
    application: &AppHandle,
    project: &ProjectItem,
    channel: &Channel,
) -> Result<(), String> {
    for (identifier, description) in velocity_threads::dependencies::refresh(channel, project)
        .map_err(|error| error.to_string())?
    {
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .update_thread(project.id, identifier, |saved| {
            saved.dependency_wait = description
        })
        .map_err(|error| error.to_string())?;
        crate::threads::refresh(application, (project.id, identifier), false)?;
    }
    Ok(())
}

/// Sending is independent of the later modal: the typed IPC is also usable headlessly.
#[tauri::command]
#[specta::specta]
pub async fn airc_send(
    application: AppHandle,
    project_identifier: u32,
    text: String,
) -> Result<(), String> {
    crate::layout::off_ui_thread(move || {
        let exists = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .state()
        .projects
        .iter()
        .any(|project| project.id == project_identifier);
        if !exists {
            return Err("Unknown project".into());
        }
        channel(&application, project_identifier)?
            .comment(&text)
            .map_err(|error| error.to_string())?;
        crate::airc_modal::refresh(&application)
    })
    .await
}
