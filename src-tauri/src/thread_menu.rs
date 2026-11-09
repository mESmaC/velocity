//! The sidebar thread menu and thread deletion with Undo (SPEC.md F12).

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use tauri::{AppHandle, Manager};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_opener::OpenerExt;
use velocity_core::layout_view::LayoutSnapshot;
use velocity_core::thread_menu::{ThreadMenuAction, ThreadMenuEntry, ThreadMenuFacts, thread_menu};
use velocity_github::PullRequestClient;
use velocity_store::ThreadItem;
use velocity_threads::deletion::{
    AGENT_EXIT_WAIT, UNDO_WINDOW, close_pull_request, remove_worktree,
};
use velocity_threads::{PendingDeletions, StopOutcome, ThreadKey, plan_deletion};

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::{ManagedThreads, lock, report};

pub type ManagedDeletions = Mutex<PendingDeletions>;

/// One thread's right-click menu, built from saved state and what is on disk.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct ThreadMenuView {
    /// The project the menu was opened in; actions check it is still selected.
    pub project_identifier: u32,
    pub thread_identifier: u32,
    pub entries: Vec<ThreadMenuEntry>,
}

/// The saved thread and its project's checkout, checked against the selected project.
fn find_thread(
    application: &AppHandle,
    project_identifier: u32,
    thread_identifier: u32,
) -> Result<(PathBuf, ThreadItem), String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let project = store
        .active_project()
        .filter(|project| project.id == project_identifier)
        .ok_or("The selected project changed")?;
    let thread = project
        .threads
        .iter()
        .find(|thread| thread.id == thread_identifier)
        .ok_or("The thread no longer exists")?;
    Ok((PathBuf::from(&project.repository_path), thread.clone()))
}

#[tauri::command]
#[specta::specta]
pub async fn get_thread_menu(
    application: AppHandle,
    thread_identifier: u32,
) -> Result<Option<ThreadMenuView>, String> {
    let project_identifier = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .active_project()
    .map(|project| project.id)
    .ok_or("Select a project first")?;
    let key = (project_identifier, thread_identifier);
    let (_, thread) = find_thread(&application, project_identifier, thread_identifier)?;
    let agent_running = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .is_live(key);
    // Measuring walks the worktree, so it runs off the main thread (F5's live sizes do too).
    let path = thread.worktree_path.clone();
    let worktree_bytes = tauri::async_runtime::spawn_blocking(move || {
        let path = PathBuf::from(path?);
        if !path.is_dir() {
            return None;
        }
        velocity_git::inventory::measure_worktree(&path, &AtomicBool::new(false)).ok()
    })
    .await
    .map_err(|error| error.to_string())?;
    let facts = ThreadMenuFacts {
        worktree_path: thread.worktree_path,
        pull_request_number: thread.pull_request_number,
        agent_running,
        worktree_bytes,
        ..ThreadMenuFacts::new(thread.state.into())
    };
    Ok(thread_menu(&facts).map(|entries| ThreadMenuView {
        project_identifier,
        thread_identifier,
        entries,
    }))
}

/// Open worktree in Explorer, Copy worktree path and Open PR on GitHub. Delete has its own
/// command because it can be undone.
#[tauri::command]
#[specta::specta]
pub async fn run_thread_menu_action(
    application: AppHandle,
    project_identifier: u32,
    thread_identifier: u32,
    action: ThreadMenuAction,
) -> Result<(), String> {
    let (repository, thread) = find_thread(&application, project_identifier, thread_identifier)?;
    let worktree = thread.worktree_path.clone().unwrap_or_default();
    match action {
        ThreadMenuAction::OpenWorktree => application
            .opener()
            .open_path(worktree, None::<&str>)
            .map_err(|error| error.to_string()),
        ThreadMenuAction::CopyWorktreePath => {
            application
                .clipboard()
                .write_text(worktree.clone())
                .map_err(|error| error.to_string())?;
            show_toast(&application, format!("Copied {worktree}"), None)
        }
        ThreadMenuAction::OpenPullRequest => {
            let number = thread.pull_request_number.ok_or("The thread has no PR")?;
            tauri::async_runtime::spawn_blocking(move || {
                PullRequestClient::discover(&repository)?.open_in_browser(number)
            })
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())
        }
        ThreadMenuAction::Delete => {
            delete_thread(application, project_identifier, thread_identifier).map(|_| ())
        }
    }
}

/// Hides the thread at once and offers Undo; the deletion lands after 5 s (F12).
#[tauri::command]
#[specta::specta]
pub fn delete_thread(
    application: AppHandle,
    project_identifier: u32,
    thread_identifier: u32,
) -> Result<LayoutSnapshot, String> {
    let key = (project_identifier, thread_identifier);
    let (_, thread) = find_thread(&application, project_identifier, thread_identifier)?;
    let deletions = application.state::<ManagedDeletions>();
    let token = {
        let mut deletions = lock(deletions.inner(), "pending deletions")?;
        if deletions.is_pending(key) {
            return Err("The thread is already being deleted".into());
        }
        deletions.begin(key)
    };
    let worktree = velocity_core::thread_menu::owns_worktree(
        thread.state.into(),
        thread.worktree_path.as_deref(),
    ) && thread
        .worktree_path
        .as_deref()
        .is_some_and(|path| std::path::Path::new(path).is_dir());
    let message = format!(
        "Deleted “{}”{}",
        thread.title,
        if worktree { " and its worktree" } else { "" }
    );
    let snapshot = update_layout(&application, |layout| {
        layout.set_threads_hidden(project_identifier, &[thread_identifier as usize], true);
        layout.show_toast(message, Some(token));
    })?;
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(UNDO_WINDOW).await;
        land(&handle, token).await;
    });
    Ok(snapshot)
}

/// Undo within the 5 s: the thread is back exactly as it was, since nothing was touched.
#[tauri::command]
#[specta::specta]
pub fn undo_delete_thread(application: AppHandle, deletion: u32) -> Result<LayoutSnapshot, String> {
    let undone = lock(
        application.state::<ManagedDeletions>().inner(),
        "pending deletions",
    )?
    .undo(deletion);
    update_layout(&application, |layout| {
        if let Some((project, thread)) = undone {
            layout.set_threads_hidden(project, &[thread as usize], false);
        }
        layout.clear_deletion_toast(deletion);
    })
}

/// The undo window is over: stop the agent, remove the worktree, close the PR and drop the
/// record. A worktree that cannot be removed keeps the thread, so nothing is lost silently.
async fn land(application: &AppHandle, token: u32) {
    let landed = lock(
        application.state::<ManagedDeletions>().inner(),
        "pending deletions",
    )
    .map(|mut deletions| deletions.land(token));
    let Ok(Some(key)) = landed else {
        return;
    };
    report(update_layout(application, |layout| layout.clear_deletion_toast(token)).map(|_| ()));
    let outcome = match land_effects(application, key).await {
        Ok(()) => remove_record(application, key),
        Err(error) => Err(error),
    };
    if let Err(error) = outcome {
        report(
            update_layout(application, |layout| {
                layout.set_threads_hidden(key.0, &[key.1 as usize], false);
                layout.show_toast(error.clone(), None);
            })
            .map(|_| ()),
        );
    }
}

async fn land_effects(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    let (repository, thread) = find_saved_thread(application, key)?;
    let mut plan = plan_deletion(&repository, &thread);
    let stopped = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .stop(key);
    match stopped {
        StopOutcome::Stopping(ended) => {
            // An agent that ignores the stop is cut off; its folder removal retries below.
            let _ = tokio::time::timeout(AGENT_EXIT_WAIT, ended).await;
        }
        // The launch removes the worktree itself once setup ends.
        StopOutcome::SettingUp => plan.remove_worktree = None,
        StopOutcome::NotRunning => {}
    }
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(worktree) = &plan.remove_worktree {
            remove_worktree(&plan.repository, worktree)?;
        }
        if let Some(number) = plan.close_pull_request {
            close_pull_request(&plan.repository, number)?;
        }
        Ok::<(), velocity_threads::DeletionError>(())
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}

/// Like [`find_thread`], but for any saved project: the user may have switched projects
/// during the undo window.
fn find_saved_thread(
    application: &AppHandle,
    key: ThreadKey,
) -> Result<(PathBuf, ThreadItem), String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let project = store.project(key.0).ok_or("The project no longer exists")?;
    let thread = project
        .threads
        .iter()
        .find(|thread| thread.id == key.1)
        .ok_or("The thread no longer exists")?;
    Ok((PathBuf::from(&project.repository_path), thread.clone()))
}

fn remove_record(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .remove_thread(key.0, key.1)
    .map_err(|error| error.to_string())?;
    update_layout(application, |layout| {
        // The row is gone from the store, so the hidden mark has nothing left to hide.
        layout.set_threads_hidden(key.0, &[key.1 as usize], false);
    })
    .map(|_| ())
}

fn show_toast(
    application: &AppHandle,
    message: String,
    undo_deletion: Option<u32>,
) -> Result<(), String> {
    update_layout(application, |layout| {
        layout.show_toast(message, undo_deletion)
    })
    .map(|_| ())
}

/// Applies `change` to the main window over fresh saved state and publishes the snapshot.
fn update_layout(
    application: &AppHandle,
    change: impl FnOnce(&mut velocity_core::layout_state::LayoutState),
) -> Result<LayoutSnapshot, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let mut layout = lock(
        application.state::<MainLayout>().inner(),
        "main-window state",
    )?;
    store.refresh_layout(&mut layout);
    change(&mut layout);
    let snapshot = layout.snapshot();
    crate::layout::publish(application, &snapshot, None)?;
    Ok(snapshot)
}
