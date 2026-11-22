//! Rosen workspaces for threads (F5). A project's agents get Rosen changes and
//! workspaces from the project's own Rosen server, which Velocity starts; git worktrees
//! are used only when the project chooses git. Failed Rosen launches stay retryable.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    pub(crate) servers: Servers,
    watches: Watches,
    /// Projects whose event stream a follower reads (F6).
    following: std::sync::Mutex<BTreeSet<u32>>,
    /// Projects whose disk budget this run has sized.
    budgeted: std::sync::Mutex<BTreeSet<u32>>,
    /// Largest swarm requested this run. A later, smaller launch must not shrink its pool.
    pool_sizes: tokio::sync::Mutex<BTreeMap<u32, usize>>,
    /// Held while a pass runs Rosen events' follow-ups (`rosen_events`).
    pub(crate) follow_ups: tokio::sync::Mutex<()>,
    /// Each project's catch-up from its mirror this run, done once on its first start (#501).
    catch_ups: std::sync::Mutex<BTreeMap<u32, std::sync::Arc<tokio::sync::OnceCell<()>>>>,
    /// Projects whose catch-up on start failed and is tried again until Rosen answers (#501).
    catch_up_retries: std::sync::Mutex<BTreeSet<u32>>,
    /// Per-project lock serializing mirror tip checks and catch-ups (#520).
    catch_up_locks: std::sync::Mutex<BTreeMap<u32, Arc<tokio::sync::Mutex<()>>>>,
    /// Last mirror rejection sequence for which automatic catch-up recovery ran (#520).
    recovered_rejections: std::sync::Mutex<BTreeMap<u32, u64>>,
    /// Projects whose Collapse relay allow-list has been applied for the running server (#576).
    relay_synced: std::sync::Mutex<BTreeSet<u32>>,
}

impl ManagedRosen {
    /// Records that `project`'s failed catch-up on start is being tried again.
    pub(crate) fn begin_catch_up_retry(&self, project: u32) {
        if let Ok(mut retries) = self.catch_up_retries.lock() {
            retries.insert(project);
        }
    }

    /// Whether `project`'s catch-up is still being tried again.
    pub(crate) fn retries_catch_up(&self, project: u32) -> bool {
        self.catch_up_retries
            .lock()
            .is_ok_and(|retries| retries.contains(&project))
    }

    /// Ends `project`'s retries once Rosen answered a catch-up; true when one was waiting.
    pub(crate) fn end_catch_up_retry(&self, project: u32) -> bool {
        self.catch_up_retries
            .lock()
            .is_ok_and(|mut retries| retries.remove(&project))
    }

    /// The project's catch-up for this run, shared so concurrent first starts wait for one.
    pub(crate) fn catch_up_once(&self, project: u32) -> std::sync::Arc<tokio::sync::OnceCell<()>> {
        self.catch_ups
            .lock()
            .map(|mut catch_ups| std::sync::Arc::clone(catch_ups.entry(project).or_default()))
            .unwrap_or_default()
    }

    /// Serializes mirror catch-up work per project across startup, polls, lands and retries.
    pub(crate) fn catch_up_lock(&self, project: u32) -> Arc<tokio::sync::Mutex<()>> {
        self.catch_up_locks
            .lock()
            .map(|mut locks| Arc::clone(locks.entry(project).or_default()))
            .unwrap_or_default()
    }

    /// Claims one automatic catch-up recovery pass for mirror rejection `sequence` (#520).
    pub(crate) fn claim_rejection_recovery(&self, project: u32, sequence: u64) -> bool {
        self.recovered_rejections
            .lock()
            .is_ok_and(|mut seen| seen.insert(project, sequence) != Some(sequence))
    }

    /// Clears the rejection recovery marker once the project's mirror is healthy again.
    pub(crate) fn clear_rejection_recovery(&self, project: u32) {
        if let Ok(mut seen) = self.recovered_rejections.lock() {
            seen.remove(&project);
        }
    }
}

/// How often every open Rosen thread is checked for a running watcher: one that ended,
/// or never started, is started again. Its first snapshot covers the whole workspace,
/// so the edits made meanwhile are stored then.
const WATCH_CHECK: Duration = Duration::from_secs(30);

/// How long a thread's workspace may take on Velocity's shared Rosen connection before a
/// second attempt starts on a connection of its own, and how long both may then take
/// before the launch fails with the reason (#514). A workspace usually takes seconds.
const WORKSPACE_WAIT_LIMIT: Duration = Duration::from_secs(3 * 60);

/// How often a thread whose workspace is still being made is told what it waits on.
const WORKSPACE_WAIT_CHECK: Duration = Duration::from_secs(10);

/// Shown on the thread once the second, direct attempt at its workspace has started.
const DIRECT_ATTEMPT_NOTE: &str = "no workspace after 3 min, so a direct attempt runs too";

pub(crate) fn initialize(application: &AppHandle) -> anyhow::Result<()> {
    let app_data = application.path().app_data_dir()?;
    application.manage(ManagedRosen {
        servers: Servers::new(app_data),
        watches: Watches::default(),
        following: std::sync::Mutex::new(BTreeSet::new()),
        budgeted: std::sync::Mutex::new(BTreeSet::new()),
        pool_sizes: tokio::sync::Mutex::new(BTreeMap::new()),
        follow_ups: tokio::sync::Mutex::new(()),
        catch_ups: std::sync::Mutex::new(BTreeMap::new()),
        catch_up_retries: std::sync::Mutex::new(BTreeSet::new()),
        catch_up_locks: std::sync::Mutex::new(BTreeMap::new()),
        recovered_rejections: std::sync::Mutex::new(BTreeMap::new()),
        relay_synced: std::sync::Mutex::new(BTreeSet::new()),
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

/// Rosen's MCP server for a builder agent in workspace `root` (Rosen R10).
pub(crate) fn mcp_server(root: &Path, environment: &[(String, String)]) -> Option<McpServer> {
    mcp_server_for_role(
        root,
        environment,
        velocity_core::tool_policy::ThreadRole::Builder,
    )
}

/// Rosen's role-filtered MCP server for workspace `root` (#531, #533).
/// `None` when Rosen's CLI isn't installed.
pub(crate) fn mcp_server_for_role(
    root: &Path,
    environment: &[(String, String)],
    role: velocity_core::tool_policy::ThreadRole,
) -> Option<McpServer> {
    let command = std::env::current_exe()
        .ok()
        .and_then(|executable| {
            velocity_rosen::mcp_proxy::proxy_mcp_command_for_role(
                &executable,
                Some(root),
                environment,
                role,
                false,
            )
        })
        .or_else(|| velocity_rosen::mcp_command(root, environment))?;
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

/// Read-only Rosen MCP server for question threads on a Rosen project, without a workspace
/// or change (#533). Returns `None` for non-Rosen projects or when the Rosen CLI is absent.
pub(crate) fn read_only_mcp_server(application: &AppHandle, project: u32) -> Option<McpServer> {
    if !uses_rosen(application, project) {
        return None;
    }
    let app_data = application.path().app_data_dir().ok()?;
    let server = ProjectServer::new(&app_data, project);
    let executable = std::env::current_exe().ok()?;
    let environment = vec![(
        String::from("ROSEN_ENDPOINT"),
        server.endpoint().as_str().to_owned(),
    )];
    let command = velocity_rosen::mcp_proxy::proxy_mcp_command_for_role(
        &executable,
        None,
        &environment,
        velocity_core::tool_policy::ThreadRole::Question,
        false,
    )?;
    let variables = command
        .environment
        .into_iter()
        .map(|(name, value)| EnvVariable::new(name, value))
        .collect();
    Some(McpServer::Stdio(
        McpServerStdio::new("rosen", command.program)
            .args(command.arguments)
            .env(variables),
    ))
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
/// migrated (F5).
pub(crate) fn rosen_in_use(application: &AppHandle, project: u32) -> bool {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()
    .and_then(|store| store.project(project).map(ProjectItem::rosen_in_use))
    .unwrap_or(false)
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

/// One Rosen agent name for thread `key`, falling back to its workspace leaf for
/// pre-#532 threads that still own a change created under the folder name.
pub(crate) fn thread_agent(application: &AppHandle, key: ThreadKey, root: &Path) -> String {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()
    .and_then(|store| {
        store
            .project(key.0)?
            .threads
            .iter()
            .find(|thread| thread.id == key.1)
            .map(ThreadItem::rosen_agent_name)
    })
    .unwrap_or_else(|| velocity_rosen::agent_name(root))
}

/// Resolves the Rosen agent name for workspace `root` in `project`.
pub(crate) fn agent_for_root(application: &AppHandle, project: u32, root: &Path) -> String {
    let target = root.to_string_lossy();
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()
    .and_then(|store| {
        store
            .project(project)?
            .threads
            .iter()
            .find(|thread| thread.worktree_path.as_deref() == Some(target.as_ref()))
            .map(ThreadItem::rosen_agent_name)
    })
    .unwrap_or_else(|| velocity_rosen::agent_name(root))
}

fn agent_for_workspace(application: &AppHandle, root: &Path) -> String {
    let target = root.to_string_lossy();
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()
    .and_then(|store| {
        store
            .state()
            .projects
            .iter()
            .flat_map(|project| project.threads.iter())
            .find(|thread| thread.worktree_path.as_deref() == Some(target.as_ref()))
            .map(ThreadItem::rosen_agent_name)
    })
    .unwrap_or_else(|| velocity_rosen::agent_name(root))
}

/// Writes the server's rebase into the workspace at `root`, if one waits, between its
/// watcher's stop and restart (Rosen R2). Returns the head written, if any.
pub(crate) async fn apply_rebase(
    application: &AppHandle,
    project: u32,
    root: &Path,
) -> Result<Option<String>, String> {
    let server = server(application, project).await?;
    let agent = agent_for_root(application, project, root);
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

/// Describes the daemon without starting a server just to display its status.
pub(crate) fn observation_server(
    application: &AppHandle,
    project: u32,
) -> Result<ProjectServer, String> {
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let pool = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let item = store.project(project).ok_or("Unknown project")?;
        let preferences = store.worktree_preferences(item);
        velocity_threads::project_workspace_root(item, &preferences)
            .map(|root| root.join(WARM_POOL_FOLDER))
    };
    Ok(ProjectServer::new(&data, project).with_warm_pool_dir(pool))
}

pub(crate) async fn pool_target(application: &AppHandle, project: u32) -> u32 {
    let managed = application.state::<ManagedRosen>();
    let sizes = managed.pool_sizes.lock().await;
    u32::try_from(sizes.get(&project).copied().unwrap_or(0)).unwrap_or(u32::MAX)
}

/// The project's server, started, imported and, once per run, caught up from its mirror.
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
    let managed = application.state::<ManagedRosen>();
    let server = if velocity_rosen::project::has_local_server(&repository) {
        managed
            .servers
            .ready_local(project, &repository, pool.clone())
            .await
    } else {
        let source = tauri::async_runtime::spawn_blocking(move || {
            ImportSource::for_checkout(&repository, &branch)
        })
        .await
        .map_err(|error| error.to_string())?;
        managed.servers.ready(project, &source, pool.clone()).await
    }
    .map_err(|error| error.to_string())?;
    if let Some(update) = managed.servers.take_update(project) {
        report(show_server_update(application, project, &update));
    }
    let first = managed
        .budgeted
        .lock()
        .is_ok_and(|mut budgeted| budgeted.insert(project));
    if let Some(folder) = pool.filter(|_| first) {
        size_disk_budget(&server, folder).await;
    }
    // Before any caller can land: a base behind its mirror would push non-fast-forward.
    crate::rosen_mirror::catch_up_once(application, project, &server).await;
    sync_collapse_relay_if_needed(application, project, &server).await;
    Ok(server)
}

/// Re-applies the project's Collapse relay allow-list when this run first readies its
/// server or whenever Velocity starts/re-imports the server process (#576).
async fn sync_collapse_relay_if_needed(
    application: &AppHandle,
    project: u32,
    server: &ProjectServer,
) {
    let managed = application.state::<ManagedRosen>();
    let started = managed.servers.take_started(project);
    let already_synced = !started
        && managed
            .relay_synced
            .lock()
            .is_ok_and(|synced| synced.contains(&project));
    if already_synced {
        return;
    }
    if crate::collapse_mirror::sync_relay_on_start(application, project, server).await
        && let Ok(mut synced) = managed.relay_synced.lock()
    {
        synced.insert(project);
    }
}

/// Tells the owner what became of a project server found running another Rosen build than
/// this Velocity's (#515): a toast once it is replaced, or a notice on the project, until
/// Velocity restarts, saying which build still runs and why.
fn show_server_update(
    application: &AppHandle,
    project: u32,
    update: &velocity_rosen::ServerUpdate,
) -> Result<(), String> {
    let name = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .project(project)
    .map(|item| item.name.clone())
    .unwrap_or_default();
    let headline = update.headline();
    crate::diagnostics::record(
        "rosen server",
        &format!(
            "{name}: {headline}{}",
            update
                .reason()
                .map(|reason| format!(" ({reason})"))
                .unwrap_or_default()
        ),
    );
    let mut layout = lock(
        application.state::<crate::layout::MainLayout>().inner(),
        "layout",
    )?;
    match update.reason() {
        None => layout.show_toast(format!("{name}: {headline}"), None),
        Some(reason) => layout.set_rosen_server_notice(
            project,
            velocity_core::layout_view::RosenServerNotice::new(headline, reason.to_owned()),
        ),
    }
    crate::layout::publish(application, &layout.snapshot(), None)
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

/// Blocking wrapper for launch callers already running off the UI thread (F5 Swarm launches).
pub(crate) fn reserve_pool_sync(application: &AppHandle, project: u32, count: usize) {
    if !uses_rosen(application, project) {
        return;
    }
    if let Err(error) = tauri::async_runtime::block_on(reserve_pool(application, project, count)) {
        eprintln!("Rosen warm pool reservation: {error}");
    }
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
    let agent = thread_agent(application, key, &request.worktree_path);
    let workspace = acquire_workspace(application, key, request, &server, &agent).await?;
    // Install local ignore rules and links before the first watcher can store them.
    let setup = finish_setup(application, key.0, request, &workspace.root).await;
    crate::rosen_issue::record_source(application, key, &server, &agent, &workspace.change).await?;
    watch_as(application, &server, &agent, &workspace.root).await?;
    crate::rosen_events::follow(application, key.0);
    let mut environment = velocity_rosen::agent_environment(&server, &agent, &workspace.change);
    append_issue_host_env(application, key.0, &mut environment);
    Ok(ThreadWorkspace {
        environment,
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
    // A change the thread already has is a retry: only its workspace is still missing.
    let change = match existing {
        Some(change) => change,
        None => {
            velocity_rosen::ensure_no_existing_workspace(&request.worktree_path)
                .map_err(|error| error.to_string())?;
            let change = velocity_rosen::create_change(server, agent, None)
                .await
                .map_err(|error| error.to_string())?;
            keep_change(application, key, server, agent, &change).await?;
            change
        }
    };
    let fell_back = Arc::new(AtomicBool::new(false));
    let reporter = tauri::async_runtime::spawn(report_workspace_wait(
        application.clone(),
        key,
        Arc::clone(&fell_back),
    ));
    let opened = velocity_rosen::open_workspace_within(
        server,
        agent,
        &change,
        &request.worktree_path,
        WORKSPACE_WAIT_LIMIT,
        || {
            fell_back.store(true, Ordering::Relaxed);
            direct_attempt_started(application, key, &request.worktree_path);
        },
    )
    .await;
    reporter.abort();
    opened.map_err(|error| {
        format!("created Rosen change {change} but could not prepare its workspace: {error}")
    })
}

fn direct_attempt_started(application: &AppHandle, key: ThreadKey, root: &Path) {
    crate::diagnostics::record(
        "rosen workspace",
        &format!(
            "{} had no workspace after {} seconds; a direct attempt runs too",
            root.display(),
            WORKSPACE_WAIT_LIMIT.as_secs()
        ),
    );
    show_workspace_wait(application, key, Some(DIRECT_ATTEMPT_NOTE));
}

/// Saves the thread's new change before its workspace is made, so deletion abandons it and
/// a retry reuses it. The change is given up only when it cannot be saved, as when the
/// thread was deleted meanwhile. One the thread already has is never abandoned here: a
/// retry that found its change recorded used to be read as a deleted thread (#514).
async fn keep_change(
    application: &AppHandle,
    key: ThreadKey,
    server: &ProjectServer,
    agent: &str,
    change: &str,
) -> Result<(), String> {
    let Err(error) = record_change(application, key, change) else {
        return Ok(());
    };
    let _ = velocity_rosen::abandon_change(server, agent, change).await;
    Err(format!(
        "Rosen change {change} could not be saved on the thread, so it was abandoned: {error}"
    ))
}

/// While a thread's workspace is being made, says on the thread what the machine's run
/// queue shows it waits on: a warm pool prepare, or a starving queue (#514).
async fn report_workspace_wait(application: AppHandle, key: ThreadKey, fell_back: Arc<AtomicBool>) {
    loop {
        tokio::time::sleep(WORKSPACE_WAIT_CHECK).await;
        let wait = tauri::async_runtime::spawn_blocking(velocity_rosen::creation_wait)
            .await
            .ok()
            .flatten();
        let mut reasons = Vec::new();
        if let Some(wait) = wait {
            reasons.push(wait.describe(velocity_updates::local_clock_at));
        }
        if fell_back.load(Ordering::Relaxed) {
            reasons.push(DIRECT_ATTEMPT_NOTE.to_owned());
        }
        let reason = (!reasons.is_empty()).then(|| reasons.join(" · "));
        show_workspace_wait(&application, key, reason.as_deref());
    }
}

fn show_workspace_wait(application: &AppHandle, key: ThreadKey, reason: Option<&str>) {
    let changed = lock(
        application
            .state::<crate::threads::ManagedThreads>()
            .inner(),
        "thread sessions",
    )
    .is_ok_and(|mut runtimes| runtimes.workspace_waiting(key, reason));
    if changed {
        report(crate::threads::refresh(application, key, false).map(|_| ()));
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
    let recorded = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .update_rosen_work_thread(key.0, key.1, |thread| {
        thread.rosen_change = Some(change.to_owned());
    })
    .map_err(|error| error.to_string())?;
    if recorded {
        crate::fleet_ledger::bind_change(application, key, change);
        crate::rosen_status::ownership_changed(application);
    }
    Ok(recorded)
}

pub(crate) async fn record_work_sources(
    application: &AppHandle,
    key: ThreadKey,
    root: &Path,
    change: &str,
) -> Result<(), String> {
    let server = server(application, key.0).await?;
    let agent = thread_agent(application, key, root);
    crate::rosen_issue::record_source(application, key, &server, &agent, change).await
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
    let agent = thread.rosen_agent_name();
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
    if let Err(error) = velocity_rosen::verify_workspace_change(root, change) {
        return Some(Err(error.to_string()));
    }
    if let Err(error) = setup_resumed_workspace(application, project, root) {
        return Some(Err(error));
    }
    let app_data = match application.path().app_data_dir() {
        Ok(folder) => folder,
        Err(error) => return Some(Err(error.to_string())),
    };
    let server = ProjectServer::new(&app_data, project);
    let name = thread.rosen_agent_name();
    agent
        .env
        .extend(velocity_rosen::agent_environment(&server, &name, change));
    append_issue_host_env(application, project, &mut agent.env);
    setup
        .shared_servers
        .extend(mcp_server_for_role(root, &agent.env, thread.role));
    setup.session_card = Some(crate::rosen_issue::session_card(
        application,
        (project, thread.id),
        root,
        change,
    ));
    crate::handoff::configure(application, (project, thread.id), setup);
    // The watcher started at launch may have failed; the agent's edits are stored only
    // while one runs.
    let handle = application.clone();
    let root = root.to_path_buf();
    let key = (project, thread.id);
    let change = change.to_owned();
    tauri::async_runtime::spawn(async move {
        report(watch_thread(&handle, project, &root).await);
        report(record_work_sources(&handle, key, &root, &change).await);
    });
    Some(Ok(()))
}

/// Passes the project's effective issue host (`collapse` or `github`) into the agent's
/// environment so `workflow_run` uses Rosen check sets on Collapse projects (#591).
fn append_issue_host_env(
    application: &AppHandle,
    project: u32,
    environment: &mut Vec<(String, String)>,
) {
    let host = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()
    .and_then(|store| {
        store
            .project(project)
            .map(|item| match item.effective_issue_host() {
                velocity_core::collapse::IssueHost::Collapse => "collapse",
                velocity_core::collapse::IssueHost::GitHub => "github",
            })
    });
    if let Some(host) = host {
        environment.push((String::from("VELOCITY_ISSUE_HOST"), host.to_owned()));
    }
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
    let agent = agent_for_root(application, project, root);
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

pub(crate) async fn watch(
    application: &AppHandle,
    server: &ProjectServer,
    root: &Path,
) -> Result<(), String> {
    let agent = agent_for_workspace(application, root);
    watch_as(application, server, &agent, root).await
}

pub(crate) async fn watch_as(
    application: &AppHandle,
    server: &ProjectServer,
    agent: &str,
    root: &Path,
) -> Result<(), String> {
    application
        .state::<ManagedRosen>()
        .watches
        .start(server, agent, root)
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
        if let Ok(mut synced) = managed.relay_synced.lock() {
            synced.clear();
        }
        managed.servers.restart_all().await;
        for (project, server) in managed.servers.registered_projects() {
            sync_collapse_relay_if_needed(application, project, &server).await;
        }
    }
}

/// [`shutdown_servers`] for the exit event, which runs outside the async runtime.
pub(crate) fn shutdown(application: &AppHandle) {
    tauri::async_runtime::block_on(shutdown_servers(application));
}
