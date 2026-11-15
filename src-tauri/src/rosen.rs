//! Rosen workspaces for threads (F5). A project's agents get Rosen changes and
//! workspaces from the project's own Rosen server, which Velocity starts; git worktrees
//! are used only when the project chooses git. Failed Rosen launches stay retryable.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use agent_client_protocol::v1::schema::{EnvVariable, McpServer, McpServerStdio};
use tauri::{AppHandle, Manager};
use velocity_acp::client::{AgentLaunch, SessionSetup};
use velocity_git::WorktreeSetupRequest;
use velocity_rosen::{ImportSource, ProjectServer, Servers, Watches};
use velocity_store::{ProjectItem, ThreadItem, ThreadState, VersionControl};
use velocity_threads::ThreadKey;

use crate::state::ManagedStateStore;
use crate::threads::{lock, report};

/// Each project's Rosen server and the watchers that keep agents' workspaces stored.
pub(crate) struct ManagedRosen {
    servers: Servers,
    watches: Watches,
    /// Projects whose event stream a follower reads (F6).
    following: std::sync::Mutex<BTreeSet<u32>>,
    /// Projects whose disk budget this run has sized.
    budgeted: std::sync::Mutex<BTreeSet<u32>>,
    /// Largest swarm requested this run. A later, smaller launch must not shrink its pool.
    pool_sizes: tokio::sync::Mutex<BTreeMap<u32, usize>>,
    /// Held while a pass runs Rosen events' follow-ups (`rosen_events`).
    pub(crate) follow_ups: tokio::sync::Mutex<()>,
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
        following: std::sync::Mutex::new(BTreeSet::new()),
        budgeted: std::sync::Mutex::new(BTreeSet::new()),
        pool_sizes: tokio::sync::Mutex::new(BTreeMap::new()),
        follow_ups: tokio::sync::Mutex::new(()),
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
    /// `ROSEN_ENDPOINT`, `ROSEN_AGENT`, `ROSEN_CHANGE` and `PATH` for the agent's process.
    pub environment: Vec<(String, String)>,
    /// The per-agent setup after the take: untracked files, build cache, setup command.
    pub setup: Result<(), String>,
}

/// Rosen's MCP server for the agent in workspace `root`, whose process has `environment`:
/// the `rosen` commands as tools (Rosen R10). `None` when Rosen's CLI isn't installed.
pub(crate) fn mcp_server(root: &Path, environment: &[(String, String)]) -> Option<McpServer> {
    let command = velocity_rosen::mcp_command(root, environment)?;
    let variables = command
        .environment
        .into_iter()
        .map(|(name, value)| EnvVariable::new(name, value))
        .collect();
    let server = McpServerStdio::new("rosen", command.program)
        .args(command.arguments)
        .env(variables);
    Some(McpServer::Stdio(server))
}

/// Whether `project`'s new threads work in Rosen workspaces.
pub(crate) fn uses_rosen(application: &AppHandle, project: u32) -> bool {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()
    .and_then(|store| store.project(project).map(|item| item.version_control))
    .is_some_and(|choice| choice == VersionControl::Rosen)
}

/// Whether Rosen still serves `project`: its choice, or a Rosen thread from before it
/// switched to git (F5).
pub(crate) fn rosen_in_use(application: &AppHandle, project: u32) -> bool {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()
    .and_then(|store| store.project(project).map(ProjectItem::rosen_in_use))
    .unwrap_or(false)
}

/// Git launches and pool refreshes must carry forward lands even without a mirror.
/// Runs on their blocking workers; projects Rosen never imported are unchanged.
pub(crate) fn git_base(
    application: &AppHandle,
    project: u32,
    repository: &Path,
    branch: &str,
) -> Result<Option<String>, String> {
    let app_data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let server = ProjectServer::new(&app_data, project);
    velocity_rosen::synchronize_git_base(&server, repository, branch)
        .map_err(|error| error.to_string())
}

/// Whether the servers are stopped for an update or exit, and must not be started.
pub(crate) fn paused(application: &AppHandle) -> bool {
    application
        .try_state::<ManagedRosen>()
        .is_none_or(|managed| managed.servers.stopped())
}

/// Records that a follower reads `project`'s events; false when one already does.
pub(crate) fn begin_following(application: &AppHandle, project: u32) -> bool {
    application
        .try_state::<ManagedRosen>()
        .and_then(|managed| {
            managed
                .following
                .lock()
                .ok()
                .map(|mut following| following.insert(project))
        })
        .unwrap_or(false)
}

/// Records that `project`'s follower stopped, so a later Rosen thread starts another.
pub(crate) fn end_following(application: &AppHandle, project: u32) {
    if let Some(managed) = application.try_state::<ManagedRosen>()
        && let Ok(mut following) = managed.following.lock()
    {
        following.remove(&project);
    }
}

/// Writes the server's rebase into the workspace at `root`, if one waits, between its
/// watcher's stop and restart (Rosen R2). Returns the head written, if any.
pub(crate) async fn apply_rebase(
    application: &AppHandle,
    project: u32,
    root: &Path,
) -> Result<Option<String>, String> {
    let server = server(application, project).await?;
    let agent = velocity_rosen::agent_name(root);
    application
        .state::<ManagedRosen>()
        .watches
        .apply_rebase(&server, &agent, root)
        .await
        .map_err(|error| error.to_string())
}

/// Stops the watcher on a workspace whose change has closed.
pub(crate) async fn stop_watch(application: &AppHandle, root: &Path) {
    application.state::<ManagedRosen>().watches.stop(root).await;
}

/// The folder, among a project's workspaces, that Rosen keeps its warm pool under.
const WARM_POOL_FOLDER: &str = ".rosen-pool";

/// The project's server, started and imported.
pub(crate) async fn server(application: &AppHandle, project: u32) -> Result<ProjectServer, String> {
    let source = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let item = store.project(project).ok_or("Unknown project")?;
        let repository = PathBuf::from(&item.repository_path);
        let branch = item.default_branch.clone();
        // Beside the project's workspaces, so the pool is on their volume (Rosen R3).
        let preferences = store.worktree_preferences(item);
        let pool = velocity_threads::project_workspace_root(item, &preferences)
            .map(|root| root.join(WARM_POOL_FOLDER));
        (repository, branch, pool)
    };
    let (repository, branch, pool) = source;
    let source = tauri::async_runtime::spawn_blocking(move || {
        ImportSource::for_checkout(&repository, &branch)
    })
    .await
    .map_err(|error| error.to_string())?;
    let managed = application.state::<ManagedRosen>();
    let server = managed
        .servers
        .ready(project, &source, pool.clone())
        .await
        .map_err(|error| error.to_string())?;
    let first = managed
        .budgeted
        .lock()
        .is_ok_and(|mut budgeted| budgeted.insert(project));
    if let Some(folder) = pool.filter(|_| first) {
        size_disk_budget(&server, folder).await;
    }
    Ok(server)
}

/// Lets Rosen's workspaces use most of the free space on their volume (see
/// [`ProjectServer::raise_disk_budget`]). A budget that can't be sized keeps Rosen's own.
async fn size_disk_budget(server: &ProjectServer, workspaces: PathBuf) {
    let free = tauri::async_runtime::spawn_blocking(move || free_bytes(&workspaces)).await;
    let Ok(Some(free)) = free else {
        return;
    };
    if let Err(error) = server.raise_disk_budget(free / 10 * 8).await {
        eprintln!("Rosen disk budget: {error}");
    }
}

/// A swarm launch raises the local daemon's target before its agents take workspaces.
pub(crate) async fn reserve_pool(
    application: &AppHandle,
    project: u32,
    count: usize,
) -> Result<(), String> {
    let server = server(application, project).await?;
    let managed = application.state::<ManagedRosen>();
    let mut sizes = managed.pool_sizes.lock().await;
    let wanted = count.max(sizes.get(&project).copied().unwrap_or(0));
    server
        .reserve_pool(wanted)
        .await
        .map_err(|error| error.to_string())?;
    sizes.insert(project, wanted);
    Ok(())
}

/// The free bytes of the drive `folder` is on.
fn free_bytes(folder: &Path) -> Option<u64> {
    let folder = folder.to_string_lossy().to_lowercase();
    velocity_git::inventory::read_drives()
        .ok()?
        .into_iter()
        .filter(|drive| folder.starts_with(&drive.root.to_lowercase()))
        .max_by_key(|drive| drive.root.len())
        .map(|drive| drive.free_bytes)
}

/// Creates or recovers the thread's workspace, retrying transient Rosen failures (F5).
pub(crate) async fn take_workspace(
    application: &AppHandle,
    key: ThreadKey,
    request: &WorktreeSetupRequest,
) -> Result<ThreadWorkspace, String> {
    velocity_rosen::retry_workspace(|| async {
        if crate::threads::setup_no_longer_wanted(application, key) {
            return Err("The Rosen workspace launch was cancelled".into());
        }
        take_workspace_once(application, key, request).await
    })
    .await
}

async fn take_workspace_once(
    application: &AppHandle,
    key: ThreadKey,
    request: &WorktreeSetupRequest,
) -> Result<ThreadWorkspace, String> {
    let server = server(application, key.0).await?;
    let agent = velocity_rosen::agent_name(&request.worktree_path);
    let workspace = acquire_workspace(application, key, request, &server, &agent).await?;
    // Install local ignore rules and links before the first watcher can store them.
    let setup = finish_setup(application, key.0, request, &workspace.root).await;
    crate::rosen_issue::record_source(application, key, &server, &agent, &workspace.change).await?;
    watch(application, &server, &workspace.root).await?;
    crate::rosen_events::follow(application, key.0);
    Ok(ThreadWorkspace {
        environment: velocity_rosen::agent_environment(&server, &agent, &workspace.change),
        change: workspace.change,
        root: workspace.root,
        setup,
    })
}

async fn acquire_workspace(
    application: &AppHandle,
    key: ThreadKey,
    request: &WorktreeSetupRequest,
    server: &ProjectServer,
    agent: &str,
) -> Result<velocity_rosen::AgentWorkspace, String> {
    let existing = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .follow_through_thread(key.0, key.1)
    .map_err(|error| error.to_string())?
    .1
    .rosen_change;
    let created = if let Some(change) = existing {
        velocity_rosen::reopen_workspace(server, agent, &change, &request.worktree_path)
            .await
            .map_err(|error| error.to_string())?;
        Ok(velocity_rosen::AgentWorkspace::new(
            change,
            request.worktree_path.clone(),
            "reopened",
            false,
        ))
    } else {
        velocity_rosen::create_workspace_retaining_change(
            server,
            agent,
            &request.worktree_path,
            None,
        )
        .await
    };
    keep_workspace(application, key, server, agent, created).await
}

async fn keep_workspace(
    application: &AppHandle,
    key: ThreadKey,
    server: &ProjectServer,
    agent: &str,
    created: Result<velocity_rosen::AgentWorkspace, velocity_rosen::RosenError>,
) -> Result<velocity_rosen::AgentWorkspace, String> {
    // Record even a partially created workspace before returning a retryable error.
    let change = match &created {
        Ok(workspace) => &workspace.change,
        Err(velocity_rosen::RosenError::WorkspaceNotCreated { change, .. }) => change,
        Err(error) => return Err(error.to_string()),
    };
    match record_change(application, key, change) {
        Ok(true) => created.map_err(|error| error.to_string()),
        saved => {
            let _ = velocity_rosen::abandon_change(server, agent, change).await;
            Err(saved
                .err()
                .unwrap_or_else(|| "The thread was deleted during Rosen workspace creation".into()))
        }
    }
}

/// Save the opening prompt before workspace creation, so even a restart can retry it.
pub(crate) fn begin_launch(
    application: &AppHandle,
    key: ThreadKey,
    prompt: &str,
) -> Result<(), String> {
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let (project, thread) = store
        .follow_through_thread(key.0, key.1)
        .map_err(|error| error.to_string())?;
    if project.version_control != VersionControl::Rosen && thread.pending_rosen_launch.is_none() {
        return Ok(());
    }
    store
        .update_thread(key.0, key.1, |thread| {
            thread
                .pending_rosen_launch
                .get_or_insert_with(|| prompt.to_owned());
        })
        .map(|_| ())
        .map_err(|error| error.to_string())
}

pub(crate) fn launch_pending(application: &AppHandle, key: ThreadKey) -> Result<bool, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let (_, thread) = store
        .follow_through_thread(key.0, key.1)
        .map_err(|error| error.to_string())?;
    Ok(thread.pending_rosen_launch.is_some())
}

pub(crate) fn finish_launch(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_thread(key.0, key.1, |thread| thread.pending_rosen_launch = None)
    .map(|_| ())
    .map_err(|error| error.to_string())
}

/// Path-dependent setup stays outside Rosen's prepared template. Even an unprepared take
/// gets these steps; a failed setup retains the change and prevents the agent from starting.
async fn finish_setup(
    application: &AppHandle,
    project: u32,
    request: &WorktreeSetupRequest,
    root: &Path,
) -> Result<(), String> {
    let application = application.clone();
    let mut request = request.clone();
    request.worktree_path = root.to_path_buf();
    tauri::async_runtime::spawn_blocking(move || setup_once(&application, project, &request))
        .await
        .map_err(|error| error.to_string())?
}

/// Kept under Rosen's local metadata, so a rebase preserves completion but a reopened or
/// stacked workspace runs setup at its new path. Failure leaves the marker absent for retry.
fn setup_once(
    application: &AppHandle,
    project: u32,
    request: &WorktreeSetupRequest,
) -> Result<(), String> {
    let marker = request.worktree_path.join(".rosen/velocity-setup");
    if marker.is_file() {
        return Ok(());
    }
    crate::skills::setup_workspace(application, project, request)?;
    std::fs::write(marker, b"complete\n").map_err(|error| error.to_string())
}

fn setup_resumed_workspace(
    application: &AppHandle,
    project: u32,
    root: &Path,
) -> Result<(), String> {
    let request = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let item = store.project(project).ok_or("Unknown project")?;
        let preferences = store.worktree_preferences(item);
        let mut settings = velocity_git::default_settings();
        settings.share_build_cache = preferences.share_build_cache;
        settings.copy_untracked_files = !preferences.copy_files.trim().is_empty();
        settings.run_setup_command = !preferences.setup_command.trim().is_empty();
        WorktreeSetupRequest::detached(
            PathBuf::from(&item.repository_path),
            root.to_path_buf(),
            String::new(),
            settings,
            preferences.copy_files.clone(),
            preferences.setup_command.clone(),
            root.join("target"),
        )
    };
    setup_once(application, project, &request)
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
    if thread.pending_rosen_launch.is_some() {
        return Some(Err(
            "Retry Rosen workspace from the thread's right-click menu".into(),
        ));
    }
    let change = thread.rosen_change.as_deref()?;
    let root = Path::new(thread.worktree_path.as_deref()?);
    if !root.is_dir() {
        return Some(Err(format!(
            "Rosen workspace {} is missing",
            root.display()
        )));
    }
    if let Err(error) = setup_resumed_workspace(application, project, root) {
        return Some(Err(error));
    }
    let app_data = match application.path().app_data_dir() {
        Ok(folder) => folder,
        Err(error) => return Some(Err(error.to_string())),
    };
    let server = ProjectServer::new(&app_data, project);
    let name = velocity_rosen::agent_name(root);
    agent
        .env
        .extend(velocity_rosen::agent_environment(&server, &name, change));
    setup.shared_servers.extend(mcp_server(root, &agent.env));
    setup.preface = Some(crate::rosen_issue::workspace_note(
        application,
        (project, thread.id),
        change,
    ));
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

/// Watches the workspace of every Rosen thread whose change is still open and has no
/// watcher: after a restart, so edits made while Velocity was closed are stored too, and
/// after a watcher ended or failed to start.
async fn watch_open_threads(application: &AppHandle) {
    let Some(open) = open_threads(application) else {
        return;
    };
    // Every Rosen project is followed, including ones with no threads, so a mirror
    // failure is visible. Projects retaining Rosen threads are followed too, whether their
    // workspace is still on disk: Rosen deletes a landed change's workspace, a land that
    // happened while Velocity was closed is only read from the stream, and a landed change
    // can still be reverted after the tests that follow its land.
    for project in followed_projects(application) {
        crate::rosen_events::follow(application, project);
        // Follow-ups a failed step left: retried here as well as on reconnect.
        crate::rosen_events::run_follow_ups(application, project).await;
        report(crate::rosen_mirror::refresh(application, project).await);
    }
    for (project, root) in open {
        if root.is_dir() {
            report(watch_thread(application, project, &root).await);
        }
    }
    crate::rosen_rebase::apply_pending(application).await;
}

/// Every Rosen project, including ones with no threads whose mirror still needs watching.
fn followed_projects(application: &AppHandle) -> BTreeSet<u32> {
    let Ok(store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) else {
        return BTreeSet::new();
    };
    store
        .state()
        .projects
        .iter()
        // Retained Rosen threads still need their lands and reverts after switching to git.
        .filter(|project| project.rosen_in_use())
        .map(|project| project.id)
        .collect()
}

/// Each open Rosen thread's project and workspace root.
fn open_threads(application: &AppHandle) -> Option<Vec<(u32, PathBuf)>> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()?;
    let open = store
        .state()
        .projects
        .iter()
        // A thread keeps Rosen after its project switches to git, until it lands.
        .flat_map(|project| {
            project
                .threads
                .iter()
                .filter(|thread| thread.rosen_change.is_some())
                // Ownership is saved before setup; its launch starts the first watcher
                // after installing ignores and links. Do not watch a partial workspace.
                .filter(|thread| thread.pending_rosen_launch.is_none())
                .filter(|thread| thread.state != ThreadState::Merged)
                .map(|thread| {
                    let root = thread.worktree_path.as_deref().unwrap_or_default();
                    (project.id, PathBuf::from(root))
                })
        })
        .collect();
    Some(open)
}

/// Starts the watcher on a Rosen thread's workspace unless one runs, or the servers are
/// stopped for an update or exit.
async fn watch_thread(application: &AppHandle, project: u32, root: &Path) -> Result<(), String> {
    let managed = application.state::<ManagedRosen>();
    // A stopped server refuses to start again anyway; this only saves the attempt.
    if managed.servers.stopped() || managed.watches.is_watching(root) {
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
    let _ = tokio::time::timeout(Duration::from_secs(10), managed.servers.shutdown_all()).await;
}

/// Starts the servers again when an update [`shutdown_servers`] made way for didn't
/// install.
pub(crate) async fn restart_servers(application: &AppHandle) {
    if let Some(managed) = application.try_state::<ManagedRosen>() {
        managed.servers.restart_all().await;
    }
}

/// [`shutdown_servers`] for the exit event, which runs outside the async runtime.
pub(crate) fn shutdown(application: &AppHandle) {
    tauri::async_runtime::block_on(shutdown_servers(application));
}
