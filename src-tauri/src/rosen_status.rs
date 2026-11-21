use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use velocity_rosen::status::{Status, StatusFeed};

use crate::state::ManagedStateStore;
use crate::threads::lock;

#[derive(Default)]
pub(crate) struct StatusWatch {
    task: Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
    owners_changed: tokio::sync::Notify,
}

pub(crate) fn ownership_changed(application: &AppHandle) {
    application
        .state::<StatusWatch>()
        .owners_changed
        .notify_one();
}

#[derive(Clone, Serialize, specta::Type)]
pub struct OwningThread {
    pub identifier: String,
    pub title: String,
}

#[derive(Clone, Serialize, specta::Type, Event)]
#[tauri_specta(event_name = "rosen://status")]
pub struct RosenStatusChanged {
    pub project_identifier: u32,
    pub status: Status,
    pub owners: BTreeMap<String, OwningThread>,
    pub error: Option<String>,
}

#[tauri::command]
#[specta::specta]
pub fn watch_rosen_status(
    application: AppHandle,
    project_identifier: Option<u32>,
) -> Result<bool, String> {
    let watch = application.state::<StatusWatch>();
    let mut watch = lock(&watch.task, "Rosen status watch")?;
    let project = {
        let store = application.state::<ManagedStateStore>();
        let store = lock(store.inner(), "persisted state")?;
        let selected = store.active_project();
        if selected.map(|project| project.id) != project_identifier {
            return Err("The selected project changed".into());
        }
        selected
            .filter(|project| project.rosen_in_use())
            .map(|project| project.id)
    };
    let server = project
        .map(|project| crate::rosen::observation_server(&application, project))
        .transpose()?;
    if let Some(old) = watch.take() {
        old.abort();
    }
    if let Some((project, server)) = project.zip(server) {
        crate::collapse_mirror::on_rosen_pane_opened(&application, project);
        let application = application.clone();
        *watch = Some(tauri::async_runtime::spawn(async move {
            follow(&application, project, server).await;
        }));
    }
    Ok(project.is_some())
}

/// Switches a `before-land` Rosen project to `review = "after-land"` in `rosen.toml` (#524).
#[tauri::command]
#[specta::specta]
pub async fn switch_to_after_land(
    application: AppHandle,
    project_identifier: u32,
) -> Result<String, String> {
    if !crate::rosen::uses_rosen(&application, project_identifier) {
        return Err("Only Rosen projects use rosen.toml review modes".into());
    }
    let check_folder = pre_land_check_folder(&application, project_identifier)?;
    let server = crate::rosen::server(&application, project_identifier).await?;
    let outcome = velocity_rosen::after_land_switch::switch_to_after_land(&server, &check_folder)
        .await
        .map_err(|error| error.to_string())?;
    crate::rosen_landing::wake(&application);
    Ok(match outcome {
        velocity_rosen::after_land_switch::SwitchOutcome::AlreadyAfterLand => {
            "Project already uses review = \"after-land\"".into()
        }
        velocity_rosen::after_land_switch::SwitchOutcome::Switched { commit } => {
            let short = velocity_core::project_health::short_commit(&commit);
            format!("Switched to review = \"after-land\" at {short}")
        }
    })
}

fn pre_land_check_folder(
    application: &AppHandle,
    project_identifier: u32,
) -> Result<std::path::PathBuf, String> {
    let store = application.state::<ManagedStateStore>();
    let store = lock(store.inner(), "persisted state")?;
    let project = store
        .landing_projects()
        .into_iter()
        .find(|entry| entry.project.id == project_identifier)
        .ok_or_else(|| "Project not found".to_owned())?;
    velocity_threads::project_workspace_root(&project.project, &project.worktrees)
        .map(|root| root.join("pre-land-switch"))
        .ok_or_else(|| "Could not resolve the project's workspace root".to_owned())
}

async fn follow(application: &AppHandle, project: u32, server: velocity_rosen::ProjectServer) {
    let mut status = Status::default();
    loop {
        let result = if crate::rosen::paused(application) {
            Ok(())
        } else {
            match StatusFeed::open(server.clone()).await {
                Ok(feed) => connected(application, project, feed, &mut status).await,
                Err(error) => Err(error.to_string()),
            }
        };
        status.server = None;
        publish(application, project, &status, result.err());
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

async fn connected(
    application: &AppHandle,
    project: u32,
    mut feed: StatusFeed,
    status: &mut Status,
) -> Result<(), String> {
    let watch = application.state::<StatusWatch>();
    let mut changes = true;
    loop {
        let override_target = crate::rosen::pool_target(application, project).await;
        let mut updated = status.clone();
        let expiry = feed
            .read(&mut updated, changes, override_target)
            .await
            .map_err(|err| err.to_string())?;
        if changes || updated != *status {
            *status = updated;
            publish(application, project, status, None);
        }
        changes = tokio::select! {
            refresh = feed.next(expiry) => refresh.ok_or("Rosen disconnected. Reconnecting…")?,
            () = watch.owners_changed.notified() => {
                publish(application, project, status, None);
                false
            }
        };
    }
}

fn publish(application: &AppHandle, project: u32, status: &Status, error: Option<String>) {
    let store = application.state::<ManagedStateStore>();
    let Ok(store) = lock(store.inner(), "persisted state") else {
        return;
    };
    let Some(item) = store
        .active_project()
        .filter(|item| item.id == project && item.rosen_in_use())
    else {
        return;
    };
    let owners = item
        .threads
        .iter()
        .filter_map(|thread| {
            Some((
                thread.rosen_change.clone()?,
                OwningThread {
                    identifier: thread.id.to_string(),
                    title: thread.title.clone(),
                },
            ))
        })
        .collect();
    drop(store);
    crate::threads::report(
        RosenStatusChanged {
            project_identifier: project,
            status: status.clone(),
            owners,
            error,
        }
        .emit(application)
        .map_err(|error| error.to_string()),
    );
}
