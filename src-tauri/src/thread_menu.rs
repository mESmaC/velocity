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
use velocity_store::deleted_worktrees::DeletedWorktree;
use velocity_threads::deletion::{AGENT_EXIT_WAIT, UNDO_WINDOW, close_pull_request};
use velocity_threads::{
    Deletion, DeletionPlan, PendingDeletions, StopOutcome, ThreadKey, plan_deletion,
};

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
    thread: u32,
) -> Result<Option<ThreadMessageMode>, String> {
    let directory = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let entries = velocity_airc::Channel::open(&directory, &project.to_string())
        .and_then(|channel| channel.read(0))
        .map_err(|error| error.to_string())?;
    // Nicks may be shortened or collision-suffixed: use the stable member identity.
    let member = format!("thread-{thread}");
    Ok(entries
        .iter()
        .rev()
        .find(|entry| entry.member == member && matches!(entry.kind.as_str(), "join" | "leave"))
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
    let project_identifier = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .active_project()
    .map(|project| project.id)
    .ok_or("Select a project first")?;
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
    let (_, thread) = match find_thread(&application, project_identifier, thread_identifier) {
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
    let message_mode = thread_message_mode(&application, project_identifier, thread_identifier)?;
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
        message_mode,
        swarm: false,
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
    crate::layout::off_ui_thread(move || {
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
    crate::layout::off_ui_thread(move || {
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
    let (repository, thread) = match find_saved_thread(application, key) {
        Ok(found) => found,
        // Already gone: nothing is left to delete.
        Err(_) => return Vec::new(),
    };
    let plan = plan_deletion(&repository, &thread);
    let stop = stop_agent(application, key).await;
    let (kept, mut problems) = run_effects(plan, &thread, stop).await;
    if let Some(change) = &thread.rosen_change
        && let Err(error) = crate::rosen::abandon(application, key.0, &thread, change).await
    {
        problems.push(format!("its Rosen change stays open ({error})"));
    }
    if let Err(error) = crate::airc_claims::release(application, key) {
        problems.push(format!("its claims were not released: {error}"));
    }
    let removed = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .and_then(|mut store| {
        store
            .remove_deleted_thread(key.0, key.1, kept)
            .map_err(|error| error.to_string())
    });
    match removed {
        Ok(_) => problems
            .into_iter()
            .map(|problem| format!("Deleted “{}”, but {problem}", thread.title))
            .collect(),
        Err(error) => Vec::from([format!("Could not delete “{}”: {error}", thread.title)]),
    }
}

/// How the thread's agent took the stop.
enum AgentStop {
    Exited,
    /// Its worktree is still being set up; the launch removes it once setup ends.
    SettingUp,
    /// It has not exited (or its exit was not confirmed) yet, so its worktree is left for
    /// the background retry, which waits until it has.
    StillRunning,
}

async fn stop_agent(application: &AppHandle, key: ThreadKey) -> AgentStop {
    application
        .state::<velocity_interview::Interviews>()
        .stop_agent(key);
    let stopped = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .map(|mut runtimes| runtimes.stop(key));
    let outcome = match stopped {
        Ok(StopOutcome::Stopping(ended)) => {
            match tokio::time::timeout(AGENT_EXIT_WAIT, ended).await {
                Ok(Ok(())) => AgentStop::Exited,
                _ => AgentStop::StillRunning,
            }
        }
        Ok(StopOutcome::SettingUp) => AgentStop::SettingUp,
        Ok(StopOutcome::NotRunning) => AgentStop::Exited,
        Err(_) => AgentStop::StillRunning,
    };
    let unconfirmed = lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )
    .map_or(true, |runtimes| runtimes.shutdown_failed(key));
    match outcome {
        AgentStop::Exited if unconfirmed => AgentStop::StillRunning,
        outcome => outcome,
    }
}

/// Removes the worktree and closes the PR off the async runtime. Returns the worktree when
/// it is still on disk, to be retried, and what went wrong.
async fn run_effects(
    plan: DeletionPlan,
    thread: &ThreadItem,
    stop: AgentStop,
) -> (Option<DeletedWorktree>, Vec<String>) {
    let worktree = plan.remove_worktree.as_ref().and_then(|_| {
        let path = thread.worktree_path.clone()?;
        Some(DeletedWorktree::new(
            thread.id,
            path,
            thread.branch_name.clone(),
        ))
    });
    let worktree = match stop {
        AgentStop::SettingUp => None,
        _ => worktree,
    };
    let exited = matches!(stop, AgentStop::Exited);
    let kept = worktree.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let mut problems = Vec::new();
        let removed = match (&plan.remove_worktree, &kept) {
            (Some(path), Some(_)) if exited => velocity_threads::deletion::remove_owned_worktree(
                &plan.repository,
                path,
                &plan.branch,
            )
            .map_err(|error| error.to_string()),
            (Some(_), Some(_)) => Err("its agent is still exiting".to_owned()),
            _ => Ok(()),
        };
        if let Err(error) = &removed {
            problems.push(format!(
                "its worktree is kept until it can be removed ({error})"
            ));
        }
        if let Some(number) = plan.close_pull_request
            && let Err(error) = close_pull_request(&plan.repository, number)
        {
            problems.push(format!("its PR stays open ({error})"));
        }
        (removed.err().and(kept), problems)
    })
    .await;
    outcome.unwrap_or_else(|error| (worktree, Vec::from([error.to_string()])))
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
