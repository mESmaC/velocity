//! F6's Rosen thread projection and header actions.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;
use specta::Type;
use tauri::{AppHandle, Manager};
use velocity_core::thread_view_model::{RosenChangeStage, RosenChangeView};
use velocity_rosen::thread_change::{self, ThreadChangeQuery};
use velocity_store::{ReviewStage, ThreadItem, ThreadState};
use velocity_threads::ThreadKey;

use crate::threads::{ManagedThreads, action_lock, lock, report};

pub(crate) async fn refresh(
    application: &AppHandle,
    project: u32,
    change: Option<&str>,
    force: bool,
) -> Result<(), String> {
    refresh_matching(application, project, change, force, false).await
}

/// Refreshes only landed, not-yet-verified threads on `Built` and `Green` events (#555).
pub(crate) async fn refresh_landed(application: &AppHandle, project: u32) -> Result<(), String> {
    refresh_matching(application, project, None, false, true).await
}

async fn refresh_matching(
    application: &AppHandle,
    project: u32,
    change: Option<&str>,
    force: bool,
    landed_only: bool,
) -> Result<(), String> {
    let change_filter = change.map(str::to_owned);
    let queries = crate::state::with_store(application, move |store| {
        let Some(project) = store.project(project) else {
            return Vec::new();
        };
        project
            .threads
            .iter()
            .filter_map(|thread| {
                let (generation, query) =
                    query(thread, change_filter.as_deref(), force, landed_only)?;
                let finished = matches!(
                    thread.state,
                    ThreadState::Done | ThreadState::Closed | ThreadState::Merged
                );
                Some((generation, query, finished))
            })
            .collect::<Vec<_>>()
    })
    .await?;
    if queries.is_empty() {
        return Ok(());
    }
    let server = crate::rosen::server(application, project).await?;
    let mut meta = BTreeMap::new();
    let mut batch_queries = Vec::with_capacity(queries.len());
    for (generation, query, finished) in queries {
        meta.insert(query.identifier.clone(), (generation, finished));
        batch_queries.push(query);
    }
    let read = match thread_change::read_batch(&server, batch_queries).await {
        Ok(results) => results,
        Err(error) => {
            report::<()>(Err(error.to_string()));
            return Ok(());
        }
    };
    let mut views = BTreeMap::new();
    let mut missing = BTreeMap::new();
    for (change, view) in read {
        let Some(&(generation, finished)) = meta.get(&change) else {
            continue;
        };
        match view {
            Some(view) => {
                views.insert(view.identifier.clone(), (generation, view));
            }
            None if finished => {
                missing.insert(change, generation);
            }
            None => eprintln!("Rosen project {project}: no change matches {change}"),
        }
    }
    let refresh = crate::state::try_with_store(application, move |store| {
        let retired = store
            .forget_missing_rosen_changes(project, &missing)
            .map_err(|error| error.to_string())?;
        let refreshed = store
            .save_rosen_change_views(project, views)
            .map_err(|error| error.to_string())?;
        Ok((retired || refreshed).then(|| store.layout_refresh()))
    })
    .await?;
    if let Some(refresh) = refresh {
        let handle = application.clone();
        tauri::async_runtime::spawn_blocking(move || {
            crate::rosen_events::publish_progress(&handle, refresh)
        })
        .await
        .map_err(|error| error.to_string())??;
    }
    Ok(())
}

fn is_settled(view: &RosenChangeView) -> bool {
    match view.stage {
        RosenChangeStage::Green | RosenChangeStage::Abandoned => true,
        RosenChangeStage::Landed => view.lands.last().is_some_and(|land| {
            land.revert.is_none()
                && (land.checks_passed || (land.build_passed && land.receipt.is_some()))
        }),
        _ => false,
    }
}

fn query(
    thread: &ThreadItem,
    change: Option<&str>,
    force: bool,
    landed_only: bool,
) -> Option<(u64, ThreadChangeQuery)> {
    let identifier = thread.rosen_change.as_ref()?;
    if change.is_some_and(|change| change != identifier) {
        return None;
    }
    let previous = thread
        .rosen_change_view
        .as_ref()
        .filter(|view| &view.identifier == identifier);
    if !force && previous.is_some_and(is_settled) {
        return None;
    }
    if landed_only {
        let awaiting_verification = match previous {
            Some(view) => view.stage == RosenChangeStage::Landed,
            None => matches!(thread.state, ThreadState::Merged | ThreadState::Done),
        };
        if !awaiting_verification {
            return None;
        }
    }
    Some((
        thread.rosen_progress_generation,
        ThreadChangeQuery {
            identifier: identifier.clone(),
            fixing: thread.rosen_land_refusal.is_some()
                || thread.review_stage == Some(ReviewStage::Fixing),
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
    if matches!(action, RosenThreadAction::Resolve) {
        let _thread = open_thread(&application, key, &change).await?;
        let server = crate::rosen::server(&application, key.0).await?;
        return queue_resolution(&application, &server, key, &change).await;
    }
    let lease = action_lock(&application, key)?;
    let _action = lease.lock().await;
    let thread = open_thread(&application, key, &change).await?;
    let server = crate::rosen::server(&application, key.0).await?;
    let runtimes = application.state::<ManagedThreads>();
    let reserved = if matches!(action, RosenThreadAction::Abandon) {
        lock(runtimes.inner(), "thread sessions")?
            .begin_rebase_update(key)
            .is_ok()
    } else {
        lock(runtimes.inner(), "thread sessions")?.begin_workspace_update(key)
    };
    if !reserved {
        return Err("Wait for the agent to finish its current turn".into());
    }
    let result = update_workspace(&application, &server, key, &thread, &change, action).await;
    lock(runtimes.inner(), "thread sessions")?.finish_workspace_update(key);
    result
}

async fn open_thread(
    application: &AppHandle,
    key: ThreadKey,
    change: &str,
) -> Result<ThreadItem, String> {
    let change = change.to_owned();
    crate::state::try_with_store(application, move |store| {
        store
            .project(key.0)
            .and_then(|project| {
                project.threads.iter().find(|thread| {
                    thread.id == key.1
                        && thread.rosen_change.as_deref() == Some(change.as_str())
                        && !matches!(thread.state, ThreadState::Merged | ThreadState::Done)
                })
            })
            .cloned()
            .ok_or_else(|| "This thread no longer has that open Rosen change".into())
    })
    .await
}

async fn queue_resolution(
    application: &AppHandle,
    server: &velocity_rosen::ProjectServer,
    key: ThreadKey,
    change: &str,
) -> Result<(), String> {
    let Some(message) = thread_change::resolution_prompt(server, change)
        .await
        .map_err(|error| error.to_string())?
    else {
        let progress = crate::state::try_with_store(application, move |store| {
            let cleared = store
                .clear_rosen_conflict_follow_up(key.0, key.1)
                .map_err(|error| error.to_string())?;
            Ok(cleared.then(|| store.layout_refresh()))
        })
        .await?;
        if let Some(progress) = progress {
            let handle = application.clone();
            let _ = tauri::async_runtime::spawn_blocking(move || {
                report(crate::rosen_events::publish_progress(&handle, progress));
            })
            .await;
        }
        report(refresh(application, key.0, Some(change), true).await);
        return Err("Rosen has no open conflicts for this change".to_owned());
    };
    let live_len = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .view(key)
    .map_or(0, |view| view.blocks.len());
    let change_owned = change.to_owned();
    let (queued, progress) = crate::state::try_with_store(application, move |store| {
        let outcome = store
            .queue_rosen_resolution(key.0, key.1, &change_owned, message, live_len)
            .map_err(|error| error.to_string())?;
        if outcome == velocity_store::rosen::QueueResolution::Unavailable {
            return Err("This thread no longer has that open Rosen change".into());
        }
        Ok((outcome, store.layout_refresh()))
    })
    .await?;
    let handle = application.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::rosen_events::publish_progress(&handle, progress)
    })
    .await
    .map_err(|error| error.to_string())??;
    crate::rosen_events::deliver_conflict_now(
        application,
        key,
        queued == velocity_store::rosen::QueueResolution::AlreadyPending,
    )
    .await
}

async fn update_workspace(
    application: &AppHandle,
    server: &velocity_rosen::ProjectServer,
    key: ThreadKey,
    thread: &ThreadItem,
    change: &str,
    action: RosenThreadAction,
) -> Result<(), String> {
    if matches!(action, RosenThreadAction::Abandon) {
        let root = thread.worktree_path.as_deref().map(Path::new);
        let result = crate::rosen::abandon(application, key.0, thread, change).await;
        if result.is_ok() {
            let _ = crate::airc_claims::release(application, key);
        }
        if result.is_err()
            && let Some(root) = root
            && root.is_dir()
        {
            report(crate::rosen::watch(application, server, root).await);
        }
        return result;
    }
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
            apply_waiting_rebase(application, server, key, root).await?;
            thread_change::rebase(server, root, change)
                .await
                .map_err(|error| error.to_string())?;
            apply_waiting_rebase(application, server, key, root).await
        }
        RosenThreadAction::Abandon | RosenThreadAction::Resolve => Ok(()),
    }
}

async fn apply_waiting_rebase(
    application: &AppHandle,
    server: &velocity_rosen::ProjectServer,
    key: ThreadKey,
    root: &Path,
) -> Result<(), String> {
    crate::rosen::apply_rebase(application, key.0, root).await?;
    let agent = crate::rosen::thread_agent(application, key, root);
    if velocity_rosen::rebase_pending(server, &agent, root)
        .await
        .map_err(|error| error.to_string())?
    {
        return Err("The rebased head is waiting for the workspace to finish syncing".into());
    }
    Ok(())
}
