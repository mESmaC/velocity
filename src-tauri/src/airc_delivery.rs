//! Native F14 message edge: queued input, persistent acknowledgement and user comments.
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
        loop {
            if let Err(error) = cycle(&application) {
                eprintln!("Velocity AIRC delivery: {error}");
            }
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
    });
}

fn cycle(application: &AppHandle) -> Result<(), String> {
    let projects = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .state()
    .projects
    .clone();
    for project in projects {
        update_waits(application, &project)?;
        for thread in &project.threads {
            if let Err(error) = deliver(application, &project, thread) {
                eprintln!("Velocity AIRC thread {}: {error}", thread.id);
            }
        }
    }
    Ok(())
}

fn deliver(
    application: &AppHandle,
    project: &ProjectItem,
    thread: &ThreadItem,
) -> Result<(), String> {
    let key = (project.id, thread.id);
    let member = format!("thread-{}", key.1);
    let channel = channel(application, project.id)?;
    let (live, setting_up) = {
        let runtimes = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?;
        (runtimes.is_live(key), runtimes.is_setting_up(key))
    };
    if !live {
        return channel.leave(&member).map_err(|error| error.to_string());
    }
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
        &channel,
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
fn update_waits(application: &AppHandle, project: &ProjectItem) -> Result<(), String> {
    let channel = channel(application, project.id)?;
    for (identifier, description) in velocity_threads::dependencies::refresh(&channel, project)
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
pub fn airc_send(
    application: AppHandle,
    project_identifier: u32,
    text: String,
) -> Result<(), String> {
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
}
