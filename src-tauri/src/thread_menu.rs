//! The sidebar thread menu and thread deletion with Undo (SPEC.md F12).

use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use tauri::{AppHandle, Manager};
use tauri_plugin_clipboard_manager::ClipboardExt;
use tauri_plugin_opener::OpenerExt;
use velocity_core::layout_view::LayoutSnapshot;
use velocity_core::thread_menu::{ThreadMenuAction, ThreadMenuEntry, ThreadMenuFacts, thread_menu};
use velocity_store::ThreadItem;
use velocity_threads::deletion::{AGENT_EXIT_WAIT, UNDO_WINDOW};
use velocity_threads::{Deletion, PendingDeletions, StopOutcome, ThreadKey};

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
    pub message_mode: Option<ThreadMessageMode>,
    /// The row is a swarm or goal swarm: its Delete is `delete_swarm` (F10, F12).
    pub swarm: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type)]
pub struct ThreadMessageMode {
    pub nick: String,
    pub mode: velocity_airc::MessageMode,
}

fn thread_message_mode(
    application: &AppHandle,
    project: u32,
    thread: &ThreadItem,
) -> Result<Option<ThreadMessageMode>, String> {
    let entries = crate::airc_access::open_channel(application, project)?
        .read(0)
        .map_err(|error| error.to_string())?;
    // Match the thread's unified agent name as well as pre-#532 `thread-<id>` logs.
    Ok(entries
        .iter()
        .rev()
        .find(|entry| {
            thread.matches_airc_member(&entry.member)
                && matches!(entry.kind.as_str(), "join" | "leave")
        })
        .filter(|entry| entry.kind == "join")
        .map(|entry| ThreadMessageMode {
            nick: entry.nick.clone(),
            mode: velocity_airc::mode_for(&entries, &entry.nick),
        }))
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
    let project_identifier = crate::state::try_with_store(&application, |store| {
        store
            .active_project()
            .map(|project| project.id)
            .ok_or_else(|| "Select a project first".to_owned())
    })
    .await?;
    if let Some(entries) =
        crate::swarm_deletion::swarm_menu(&application, project_identifier, thread_identifier)
            .await?
    {
        return Ok(Some(ThreadMenuView {
            project_identifier,
            thread_identifier,
            entries,
            message_mode: None,
            swarm: true,
        }));
    }
    let key = (project_identifier, thread_identifier);
    let handle = application.clone();
    let (_, thread) = match tauri::async_runtime::spawn_blocking(move || {
        find_thread(&handle, project_identifier, thread_identifier)
    })
    .await
    .map_err(|error| error.to_string())?
    {
        Ok(found) => found,
        Err(error) if error == "The thread no longer exists" => return Ok(None),
        Err(error) => return Err(error),
    };
    let agent_running = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .is_live(key)
        || application
            .state::<velocity_interview::Interviews>()
            .is_running(key);
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
    let message_mode = thread_message_mode(&application, project_identifier, &thread)?;
    let facts = ThreadMenuFacts {
        retry_workspace: thread.pending_rosen_launch.is_some(),
        read_only: thread.role.is_read_only(),
        worktree_path: thread.worktree_path,
        agent_running,
        worktree_bytes,
        ..ThreadMenuFacts::new(thread.state.into())
    };
    Ok(thread_menu(&facts).map(|entries| ThreadMenuView {
        project_identifier,
        thread_identifier,
        entries,
        message_mode,
        swarm: false,
    }))
}

/// Open workspace in Explorer and Copy workspace path. Delete has its own
/// command because it can be undone.
#[tauri::command]
#[specta::specta]
pub async fn run_thread_menu_action(
    application: AppHandle,
    project_identifier: u32,
    thread_identifier: u32,
    action: ThreadMenuAction,
) -> Result<(), String> {
    if action == ThreadMenuAction::Delete
        && crate::swarm_deletion::is_swarm(&application, project_identifier, thread_identifier)?
    {
        return crate::swarm_deletion::delete_swarm_now(
            application,
            project_identifier,
            thread_identifier,
        )
        .map(|_| ());
    }
    let (_, thread) = find_thread(&application, project_identifier, thread_identifier)?;
    let worktree = thread.worktree_path.clone().unwrap_or_default();
    match action {
        ThreadMenuAction::RetryWorkspace => {
            crate::threads::retry_workspace(&application, (project_identifier, thread_identifier))
                .await
        }
        ThreadMenuAction::OpenWorkspace => application
            .opener()
            .open_path(worktree, None::<&str>)
            .map_err(|error| error.to_string()),
        ThreadMenuAction::CopyWorkspacePath => {
            application
                .clipboard()
                .write_text(worktree.clone())
                .map_err(|error| error.to_string())?;
            show_toast(&application, format!("Copied {worktree}"), None)
        }
        ThreadMenuAction::Delete => {
            delete_thread_now(application, project_identifier, thread_identifier).map(|_| ())
        }
    }
}

/// Hides the thread at once and offers Undo; the deletion lands after 5 s (F12).
#[tauri::command]
#[specta::specta]
pub async fn delete_thread(
    application: AppHandle,
    project_identifier: u32,
    thread_identifier: u32,
) -> Result<LayoutSnapshot, String> {
    crate::layout::run_concurrently("delete_thread", move || {
        delete_thread_now(application, project_identifier, thread_identifier)
    })
    .await
}

/// [`delete_thread`] on the calling thread, for callers already off the UI thread.
pub(crate) fn delete_thread_now(
    application: AppHandle,
    project_identifier: u32,
    thread_identifier: u32,
) -> Result<LayoutSnapshot, String> {
    let (_, thread) = find_thread(&application, project_identifier, thread_identifier)?;
    let worktree = !thread.role.is_read_only()
        && velocity_core::thread_menu::owns_worktree(
            thread.state.into(),
            thread.worktree_path.as_deref(),
        )
        && thread
            .worktree_path
            .as_deref()
            .is_some_and(|path| std::path::Path::new(path).is_dir());
    let message = format!(
        "Deleted “{}”{}",
        thread.title,
        if worktree { " and its workspace" } else { "" }
    );
    begin_deletion(
        &application,
        Deletion::Thread((project_identifier, thread_identifier)),
        message,
    )
}

/// Hides the deletion's rows, offers Undo, and lands it when the 5 s are up (F12).
pub(crate) fn begin_deletion(
    application: &AppHandle,
    deletion: Deletion,
    message: String,
) -> Result<LayoutSnapshot, String> {
    let project = deletion.project();
    let rows = deletion.rows();
    let token = {
        let mut deletions = lock(
            application.state::<ManagedDeletions>().inner(),
            "pending deletions",
        )?;
        if rows
            .iter()
            .any(|row| deletions.is_pending((project, *row as u32)))
        {
            return Err("It is already being deleted".into());
        }
        deletions.begin(deletion)
    };
    let snapshot = update_layout(application, |layout| {
        layout.set_threads_hidden(project, &rows, true);
        layout.show_toast(message, Some(token));
    })?;
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(UNDO_WINDOW).await;
        land(&handle, token).await;
    });
    Ok(snapshot)
}

/// Undo within the 5 s: the thread or swarm is back exactly as it was, since nothing was
/// touched.
#[tauri::command]
#[specta::specta]
pub async fn undo_delete_thread(
    application: AppHandle,
    deletion: u32,
) -> Result<LayoutSnapshot, String> {
    crate::layout::run_concurrently("undo_delete_thread", move || {
        let undone = lock(
            application.state::<ManagedDeletions>().inner(),
            "pending deletions",
        )?
        .undo(deletion);
        update_layout(&application, |layout| {
            if let Some(undone) = undone {
                layout.set_threads_hidden(undone.project(), &undone.rows(), false);
            }
            layout.clear_deletion_toast(deletion);
        })
    })
    .await
}

/// The undo window is over: the deletion lands unless Undo took it first.
async fn land(application: &AppHandle, token: u32) {
    let landed = lock(
        application.state::<ManagedDeletions>().inner(),
        "pending deletions",
    )
    .map(|mut deletions| deletions.land(token));
    let Ok(Some(deletion)) = landed else {
        return;
    };
    report(update_layout(application, |layout| layout.clear_deletion_toast(token)).map(|_| ()));
    match deletion {
        Deletion::Thread(key) => {
            let problems = land_thread(application, key).await;
            show_problems(application, key.0, &[key.1 as usize], &problems);
        }
        Deletion::Swarm {
            project,
            swarm,
            members,
        } => crate::swarm_deletion::land(application, project, swarm, members).await,
    }
}

/// Stops the agent, removes the worktree, closes the PR and drops the record. The record
/// always goes (#283): a worktree that cannot be removed yet is saved with the deletion and
/// retried in the background, and an agent that is slow to exit or a PR that will not close
/// is reported rather than bringing the thread back. Returns what went wrong.
pub(crate) async fn land_thread(application: &AppHandle, key: ThreadKey) -> Vec<String> {
    let action = match crate::threads::action_lock(application, key) {
        Ok(action) => action,
        Err(error) => return Vec::from([error]),
    };
    let _action = action.lock_owned().await;
    let (_repository, thread) = match find_saved_thread(application, key) {
        Ok(found) => found,
        // Already gone: nothing is left to delete.
        Err(_) => return Vec::new(),
    };
    let _stop = stop_agent(application, key).await;
    let mut problems = Vec::new();
    let kept_browser_profile = delete_browser_profile(
        application,
        key,
        thread.browser_location.as_deref(),
        &mut problems,
    )
    .await;
    if let Some(change) = &thread.rosen_change
        && let Err(error) = crate::rosen::abandon(application, key.0, &thread, change).await
    {
        problems.push(format!("its Rosen change stays open ({error})"));
    }
    if let Err(error) = crate::airc_claims::release(application, key) {
        problems.push(format!("its claims were not released: {error}"));
    }
    let persist_pending = kept_browser_profile.is_some();
    let removed = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .and_then(|mut store| {
        store
            .remove_deleted_thread(key.0, key.1, None, kept_browser_profile)
            .map_err(|error| error.to_string())
    });
    match removed {
        Ok(_) => {
            if persist_pending && let Err(error) = crate::state::save_now(application) {
                crate::diagnostics::record("browser", &error);
            }
            problems
                .into_iter()
                .map(|problem| format!("Deleted “{}”, but {problem}", thread.title))
                .collect()
        }
        Err(error) => Vec::from([format!("Could not delete “{}”: {error}", thread.title)]),
    }
}

async fn delete_browser_profile(
    application: &AppHandle,
    key: ThreadKey,
    location: Option<&str>,
    problems: &mut Vec<String>,
) -> Option<velocity_store::deleted_browser_profiles::DeletedBrowserProfile> {
    match crate::browser::delete(application, key, location).await {
        Ok(velocity_browser::DeleteOutcome::Removed) => None,
        Ok(velocity_browser::DeleteOutcome::Pending(pending)) => {
            crate::diagnostics::record(
                "browser",
                &format!(
                    "Deferred browser profile cleanup for thread {}-{}: {}",
                    key.0, key.1, pending.error
                ),
            );
            problems.push(format!(
                "its browser profile is still in use and will be removed in the background ({})",
                pending.error
            ));
            Some(
                velocity_store::deleted_browser_profiles::DeletedBrowserProfile::new(
                    key.1,
                    pending.path.to_string_lossy().into_owned(),
                ),
            )
        }
        Err(error) => {
            problems.push(format!(
                "its browser profile could not be removed ({error})"
            ));
            None
        }
    }
}

async fn stop_agent(application: &AppHandle, key: ThreadKey) {
    application
        .state::<velocity_interview::Interviews>()
        .stop_agent(key);
    let stopped = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .map(|mut runtimes| runtimes.stop(key));
    if let Ok(StopOutcome::Stopping(ended)) = stopped {
        let _ = tokio::time::timeout(AGENT_EXIT_WAIT, ended).await;
    }
}

/// The deletion's rows leave their hidden state (their records are gone, unless dropping
/// one failed), and anything that went wrong is shown.
pub(crate) fn show_problems(
    application: &AppHandle,
    project: u32,
    rows: &[usize],
    problems: &[String],
) {
    report(
        update_layout(application, |layout| {
            layout.set_threads_hidden(project, rows, false);
            if !problems.is_empty() {
                layout.show_toast(problems.join(" · "), None);
            }
        })
        .map(|_| ()),
    );
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
pub(crate) fn update_layout(
    application: &AppHandle,
    change: impl FnOnce(&mut velocity_core::layout_state::LayoutState),
) -> Result<LayoutSnapshot, String> {
    crate::layout::refresh_with_change_and_publish(application, change)
}
