//! Rosen workspaces for threads (F5). A project's agents get Rosen changes and
//! workspaces from the project's own Rosen server, which Velocity starts; git worktrees
//! are the fallback when the project chooses git or Rosen can't start.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tauri::{AppHandle, Manager};
use velocity_acp::client::{AgentLaunch, SessionSetup};
use velocity_git::WorktreeSetupRequest;
use velocity_rosen::{ImportSource, ProjectServer, Servers, Watches};
use velocity_store::{ThreadItem, ThreadState, VersionControl};
use velocity_threads::ThreadKey;

use crate::state::ManagedStateStore;
use crate::threads::{lock, report};

/// Each project's Rosen server and the watchers that keep agents' workspaces stored.
pub(crate) struct ManagedRosen {
    servers: Servers,
    watches: Watches,
    /// Set while the servers are stopped for an update or exit, so watchers aren't
    /// started again, which would start the servers again.
    paused: AtomicBool,
}

/// How often every open Rosen thread is checked for a running watcher: one that ended,
/// or never started, is started again. Its first snapshot covers the whole workspace,
/// so the edits made meanwhile are stored then.
const WATCH_CHECK: Duration = Duration::from_secs(30);

pub(crate) fn initialize(application: &AppHandle) -> anyhow::Result<()> {
    let app_data = application.path().app_data_dir()?;
    application.manage(ManagedRosen {
        servers: Servers::new(app_data),
        watches: Watches::default(),
        paused: AtomicBool::new(false),
    });
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            watch_open_threads(&handle).await;
            tokio::time::sleep(WATCH_CHECK).await;
        }
    });
    Ok(())
}

/// A thread's Rosen workspace, ready for its agent.
pub(crate) struct ThreadWorkspace {
    pub change: String,
    pub root: PathBuf,
    /// `ROSEN_ENDPOINT`, `ROSEN_AGENT` and `PATH` for the agent's process.
    pub environment: Vec<(String, String)>,
    /// The per-agent setup after the take: untracked files, build cache, setup command.
    pub setup: Result<(), String>,
}

/// Whether `project`'s agents work in Rosen workspaces.
pub(crate) fn uses_rosen(application: &AppHandle, project: u32) -> bool {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()
    .and_then(|store| store.project(project).map(|item| item.version_control))
    .is_some_and(|choice| choice == VersionControl::Rosen)
}

/// The project's server, started and imported.
async fn server(application: &AppHandle, project: u32) -> Result<ProjectServer, String> {
    let source = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let item = store.project(project).ok_or("Unknown project")?;
        let repository = PathBuf::from(&item.repository_path);
        let branch = item.default_branch.clone();
        (repository, branch)
    };
    let (repository, branch) = source;
    let source = tauri::async_runtime::spawn_blocking(move || {
        ImportSource::for_checkout(&repository, &branch)
    })
    .await
    .map_err(|error| error.to_string())?;
    let managed = application.state::<ManagedRosen>();
    managed
        .servers
        .ready(project, &source)
        .await
        .map_err(|error| error.to_string())
}

/// Creates the thread's change and workspace at the folder its worktree would have had,
/// starts its watcher, and runs the per-agent setup there. An error means Rosen could not
/// give the thread a workspace, and the launch falls back to a git worktree.
pub(crate) async fn take_workspace(
    application: &AppHandle,
    key: ThreadKey,
    request: &WorktreeSetupRequest,
) -> Result<ThreadWorkspace, String> {
    let server = server(application, key.0).await?;
    let agent = velocity_rosen::agent_name(&request.worktree_path);
    let workspace = velocity_rosen::create_workspace(&server, &agent, &request.worktree_path, None)
        .await
        .map_err(|error| error.to_string())?;
    // An agent whose edits aren't watched, or a thread that doesn't record its change,
    // would lose track of work: the change is abandoned and the thread falls back to git.
    let kept = watch(application, &server, &workspace.root)
        .await
        .and_then(|()| record_change(application, key, &workspace.change).map(|_| ()));
    if let Err(error) = kept {
        application
            .state::<ManagedRosen>()
            .watches
            .stop(&workspace.root)
            .await;
        let _ = velocity_rosen::abandon_change(&server, &agent, &workspace.change).await;
        return Err(error);
    }
    let mut setup_request = request.clone();
    setup_request.worktree_path = workspace.root.clone();
    let setup = tauri::async_runtime::spawn_blocking(move || {
        velocity_git::finish_worktree_setup(&setup_request).map(|_| ())
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|setup| setup.map_err(|error| error.to_string()));
    Ok(ThreadWorkspace {
        change: workspace.change,
        environment: velocity_rosen::agent_environment(&server, &agent),
        root: workspace.root,
        setup,
    })
}

/// Saves the change the thread works on, so deletion abandons it and a restart watches
/// its workspace again.
pub(crate) fn record_change(
    application: &AppHandle,
    key: ThreadKey,
    change: &str,
) -> Result<bool, String> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |thread| {
        thread.rosen_change = Some(change.to_owned());
    })
    .map_err(|error| error.to_string())
}

/// Stops the thread's watcher and abandons its change; Rosen then deletes the workspace
/// under its deletion guard, keeping it while any edit there is not stored (R4).
pub(crate) async fn abandon(
    application: &AppHandle,
    project: u32,
    thread: &ThreadItem,
    change: &str,
) -> Result<(), String> {
    if thread.state == ThreadState::Merged {
        return Ok(());
    }
    let root = thread.worktree_path.as_deref().map(PathBuf::from);
    let agent = root
        .as_deref()
        .map(velocity_rosen::agent_name)
        .unwrap_or_default();
    if let Some(root) = &root {
        application.state::<ManagedRosen>().watches.stop(root).await;
    }
    let server = server(application, project).await?;
    velocity_rosen::abandon_change(&server, &agent, change)
        .await
        .map_err(|error| error.to_string())
}

/// Prepares a resumed thread's session when its folder is a Rosen workspace: checks the
/// workspace is there, and gives the agent Rosen's environment and note again. `None`
/// for a git thread, whose worktree the caller verifies as before.
pub(crate) fn prepare_resume(
    application: &AppHandle,
    project: u32,
    thread: &ThreadItem,
    agent: &mut AgentLaunch,
    setup: &mut SessionSetup,
) -> Option<Result<(), String>> {
    let change = thread.rosen_change.as_deref()?;
    let root = Path::new(thread.worktree_path.as_deref()?);
    if !root.is_dir() {
        return Some(Err(format!(
            "Rosen workspace {} is missing",
            root.display()
        )));
    }
    let app_data = match application.path().app_data_dir() {
        Ok(folder) => folder,
        Err(error) => return Some(Err(error.to_string())),
    };
    let server = ProjectServer::new(&app_data, project);
    let name = velocity_rosen::agent_name(root);
    agent
        .env
        .extend(velocity_rosen::agent_environment(&server, &name));
    setup.preface = Some(velocity_rosen::workspace_note(change));
    // The watcher started at launch may have failed; the agent's edits are stored only
    // while one runs.
    let handle = application.clone();
    let root = root.to_path_buf();
    tauri::async_runtime::spawn(async move {
        report(watch_thread(&handle, project, &root).await);
    });
    Some(Ok(()))
}

/// Abandons the change of a thread deleted while its workspace was being made.
pub(crate) async fn abandon_unstarted(
    application: &AppHandle,
    project: u32,
    root: &Path,
    change: &str,
) -> Result<(), String> {
    application.state::<ManagedRosen>().watches.stop(root).await;
    let server = server(application, project).await?;
    let agent = velocity_rosen::agent_name(root);
    velocity_rosen::abandon_change(&server, &agent, change)
        .await
        .map_err(|error| error.to_string())
}

/// Tells you a thread fell back to a git worktree, and why.
pub(crate) fn fell_back(application: &AppHandle, error: &str) {
    let message =
        format!("Rosen couldn't make a workspace, so this thread uses a git worktree: {error}");
    eprintln!("{message}");
    report(
        crate::thread_menu::update_layout(application, |layout| layout.show_toast(message, None))
            .map(|_| ()),
    );
}

/// Watches the workspace of every Rosen thread whose change is still open and has no
/// watcher: after a restart, so edits made while Velocity was closed are stored too, and
/// after a watcher ended or failed to start.
async fn watch_open_threads(application: &AppHandle) {
    let open: Vec<(u32, PathBuf)> = match lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) {
        Ok(store) => store
            .state()
            .projects
            .iter()
            .filter(|project| project.version_control == VersionControl::Rosen)
            .flat_map(|project| {
                project
                    .threads
                    .iter()
                    .filter(|thread| thread.rosen_change.is_some())
                    .filter(|thread| thread.state != ThreadState::Merged)
                    .filter_map(|thread| thread.worktree_path.as_deref())
                    .map(|path| (project.id, PathBuf::from(path)))
            })
            .filter(|(_, root)| root.is_dir())
            .collect(),
        Err(_) => return,
    };
    for (project, root) in open {
        report(watch_thread(application, project, &root).await);
    }
}

/// Starts the watcher on a Rosen thread's workspace unless one runs, or the servers are
/// stopped for an update or exit.
async fn watch_thread(application: &AppHandle, project: u32, root: &Path) -> Result<(), String> {
    let managed = application.state::<ManagedRosen>();
    if managed.paused.load(Ordering::SeqCst) || managed.watches.is_watching(root) {
        return Ok(());
    }
    let server = server(application, project).await?;
    watch(application, &server, root).await
}

async fn watch(application: &AppHandle, server: &ProjectServer, root: &Path) -> Result<(), String> {
    let agent = velocity_rosen::agent_name(root);
    application
        .state::<ManagedRosen>()
        .watches
        .start(server, &agent, root)
        .await
        .map_err(|error| error.to_string())
}

/// Stops the project servers this run used, so an update's installer can replace their
/// binaries. Waits at most ten seconds.
pub(crate) async fn shutdown_servers(application: &AppHandle) {
    let Some(managed) = application.try_state::<ManagedRosen>() else {
        return;
    };
    managed.paused.store(true, Ordering::SeqCst);
    let _ = tokio::time::timeout(Duration::from_secs(10), managed.servers.shutdown_all()).await;
}

/// Starts the servers again when an update [`shutdown_servers`] made way for didn't
/// install.
pub(crate) async fn restart_servers(application: &AppHandle) {
    if let Some(managed) = application.try_state::<ManagedRosen>() {
        managed.servers.restart_all().await;
        managed.paused.store(false, Ordering::SeqCst);
    }
}

/// [`shutdown_servers`] for the exit event, which runs outside the async runtime.
pub(crate) fn shutdown(application: &AppHandle) {
    tauri::async_runtime::block_on(shutdown_servers(application));
}
