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
        for thread in &project.threads {
            deliver(application, &project, thread)?;
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
    let member = format!("thread-{}-{}", key.0, key.1);
    let channel = channel(application, project.id)?;
    let live = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .is_live(key);
    if !live {
        return channel.leave(&member).map_err(|error| error.to_string());
    }
    let Some(path) = &thread.worktree_path else {
        return Ok(());
    };
    let nick = channel
        .join(&member, path)
        .map_err(|error| error.to_string())?;
    let cursor = velocity_airc::deliver_messages(
        &channel,
        &nick,
        thread.airc_cursor,
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
        .map(|_| ())
        .map_err(|error| error.to_string())
}
