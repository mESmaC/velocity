//! F6's Rosen thread projection and header actions.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use specta::Type;
use tauri::{AppHandle, Manager};
use velocity_core::thread_view_model::RosenChangeStage;
use velocity_rosen::thread_change::{self, ThreadChangeQuery};
use velocity_store::{ReviewStage, ThreadItem, ThreadState};
use velocity_threads::ThreadKey;

use crate::state::ManagedStateStore;
use crate::threads::{ManagedThreads, action_lock, lock, report};

pub(crate) async fn refresh(
    application: &AppHandle,
    project: u32,
    change: Option<&str>,
    force: bool,
) -> Result<(), String> {
    let queries = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let Some(project) = store.project(project) else {
            return Ok(());
        };
        project
            .threads
            .iter()
            .filter_map(|thread| {
                let (generation, query) = query(thread, change, force)?;
                let finished = matches!(
                    thread.state,
                    ThreadState::Done | ThreadState::Closed | ThreadState::Merged
                );
                Some((generation, query, finished))
            })
            .collect::<Vec<_>>()
    };
    if queries.is_empty() {
        return Ok(());
    }
    let server = crate::rosen::server(application, project).await?;
    let mut views = BTreeMap::new();
    let mut missing = BTreeMap::new();
    for (generation, query, finished) in queries {
        let change = query.identifier.clone();
        match thread_change::read(&server, query).await {
            Ok(Some(view)) => {
                views.insert(view.identifier.clone(), (generation, view));
            }
            Ok(None) if finished => {
                missing.insert(change, generation);
            }
            Ok(None) => eprintln!("Rosen project {project}: no change matches {change}"),
            Err(error) => report::<()>(Err(error.to_string())),
        }
    }
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let retired = store
        .forget_missing_rosen_changes(project, &missing)
        .map_err(|error| error.to_string())?;
    let refreshed = store
        .save_rosen_change_views(project, views)
        .map_err(|error| error.to_string())?;
    if retired || refreshed {
        crate::rosen_events::publish_progress(application, &store)?;
    }
    Ok(())
}

fn query(
    thread: &ThreadItem,
    change: Option<&str>,
    force: bool,
) -> Option<(u64, ThreadChangeQuery)> {
    let identifier = thread.rosen_change.as_ref()?;
    if change.is_some_and(|change| change != identifier) {
        return None;
    }
    let previous = thread
        .rosen_change_view
        .as_ref()
        .filter(|view| &view.identifier == identifier);
    if !force
        && previous.is_some_and(|view| {
            matches!(
                view.stage,
                RosenChangeStage::Green | RosenChangeStage::Abandoned
            )
        })
    {
        return None;
    }
    Some((
        thread.rosen_progress_generation,
        ThreadChangeQuery {
            identifier: identifier.clone(),
            fixing: thread.review_stage == Some(ReviewStage::Fixing),
            imported_through: thread.rosen_import_event,
            previous: previous.cloned(),
        },
    ))
}

#[derive(Deserialize, Type)]
pub enum RosenThreadAction {
    Name { message: String },
    Rebase,
    Abandon,
    Resolve,
}

#[tauri::command]
#[specta::specta]
pub async fn run_rosen_thread_action(
    application: AppHandle,
    project_identifier: u32,
    thread_identifier: u32,
    change: String,
    action: RosenThreadAction,
) -> Result<(), String> {
    let key = (project_identifier, thread_identifier);
    let lease = action_lock(&application, key)?;
    let _action = lease.lock().await;
    let thread = open_thread(&application, key, &change)?;
    let server = crate::rosen::server(&application, key.0).await?;
    if matches!(action, RosenThreadAction::Resolve) {
        queue_resolution(&application, &server, key, &change).await?;
    } else {
        let runtimes = application.state::<ManagedThreads>();
        if !lock(runtimes.inner(), "thread sessions")?.begin_workspace_update(key) {
            return Err("Wait for the agent to finish its current turn".into());
        }
        let result = update_workspace(&application, &server, key, &thread, &change, action).await;
        lock(runtimes.inner(), "thread sessions")?.finish_workspace_update(key);
        result?;
    }
    Ok(())
}

fn open_thread(
    application: &AppHandle,
    key: ThreadKey,
    change: &str,
) -> Result<ThreadItem, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    store
        .project(key.0)
        .and_then(|project| {
            project.threads.iter().find(|thread| {
                thread.id == key.1
                    && thread.rosen_change.as_deref() == Some(change)
                    && !matches!(thread.state, ThreadState::Merged | ThreadState::Done)
            })
        })
        .cloned()
        .ok_or_else(|| "This thread no longer has that open Rosen change".into())
}

async fn queue_resolution(
    application: &AppHandle,
    server: &velocity_rosen::ProjectServer,
    key: ThreadKey,
    change: &str,
) -> Result<(), String> {
    let message = thread_change::resolution_prompt(server, change)
        .await
        .map_err(|error| error.to_string())?
        .ok_or_else(|| "Rosen has no open conflicts for this change".to_owned())?;
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .queue_rosen_resolution(key.0, key.1, change, message)
    .map_err(|error| error.to_string())?;
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        crate::rosen_events::run_follow_ups(&handle, key.0).await;
    });
    Ok(())
}

async fn update_workspace(
    application: &AppHandle,
    server: &velocity_rosen::ProjectServer,
    key: ThreadKey,
    thread: &ThreadItem,
    change: &str,
    action: RosenThreadAction,
) -> Result<(), String> {
    let root = thread
        .worktree_path
        .as_deref()
        .map(Path::new)
        .ok_or_else(|| "This thread has no workspace".to_owned())?;
    match action {
        RosenThreadAction::Name { message } => {
            let message = message.trim();
            if message.is_empty() {
                return Err("Enter a name for the change".into());
            }
            thread_change::name(server, root, change, message.to_owned())
                .await
                .map_err(|error| error.to_string())
        }
        RosenThreadAction::Rebase => {
            apply_waiting_rebase(application, server, key.0, root).await?;
            thread_change::rebase(server, root, change)
                .await
                .map_err(|error| error.to_string())?;
            apply_waiting_rebase(application, server, key.0, root).await
        }
        RosenThreadAction::Abandon => {
            let result = crate::rosen::abandon(application, key.0, thread, change).await;
            if result.is_err() && root.is_dir() {
                report(crate::rosen::watch(application, server, root).await);
            }
            result
        }
        RosenThreadAction::Resolve => Ok(()),
    }
}

async fn apply_waiting_rebase(
    application: &AppHandle,
    server: &velocity_rosen::ProjectServer,
    project: u32,
    root: &Path,
) -> Result<(), String> {
    crate::rosen::apply_rebase(application, project, root).await?;
    let agent = velocity_rosen::agent_name(root);
    if velocity_rosen::rebase_pending(server, &agent, root)
        .await
        .map_err(|error| error.to_string())?
    {
        return Err("The rebased head is waiting for the workspace to finish syncing".into());
    }
    Ok(())
}
