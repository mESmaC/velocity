//! Each project's Rosen server. Velocity starts it with an explicit data folder and
//! endpoint, since one server holds one project, and imports the project's repository once.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rosen_client::protocol::ErrorCode;
use rosen_client::{Client, Endpoint, Error as ClientError};

use crate::RosenError;
use crate::connection::{Connection, requests};
use crate::server_update::ServerUpdate;

/// A folder holding `rosen-server` and `rosen`, for a build that doesn't ship them beside
/// Velocity's own executable (a development build).
pub const BINARY_FOLDER_ENV: &str = "VELOCITY_ROSEN_BIN";

/// Written once the repository is imported, so a restart doesn't read the whole git
/// history again only to be told the project already has a base.
const PROJECT_FOLDER: &str = "velocity-project-folder";
const IMPORTED_MARKER: &str = "velocity-imported";
/// rosen-server exits with this when another server already holds the endpoint.
const EXIT_ALREADY_RUNNING: i32 = 3;
const START_TIMEOUT: Duration = Duration::from_secs(20);

/// Where one project's server keeps its data and listens.
#[derive(Clone, Debug)]
pub struct ProjectServer {
    endpoint: Endpoint,
    data_dir: PathBuf,
    /// Where the server keeps its warm pool, when it is started from here.
    warm_pool_dir: Option<PathBuf>,
}

impl ProjectServer {
    /// The server for `project`, keeping its data under `<app data>\rosen\<project>`.
    pub fn new(app_data: &Path, project: u32) -> Self {
        let data_dir = app_data.join("rosen").join(project.to_string());
        if let Ok(folder) = std::fs::read_to_string(data_dir.join(PROJECT_FOLDER)) {
            return Self::folder_data(Path::new(&folder));
        }
        Self::at(data_dir, project, false)
    }

    pub(crate) fn for_folder(root: &Path) -> Result<Self, RosenError> {
        let folder = std::fs::canonicalize(root).map_err(|source| RosenError::Io {
            action: "resolve",
            path: root.to_path_buf(),
            source,
        })?;
        Ok(Self::folder_data(&folder))
    }

    fn folder_data(folder: &Path) -> Self {
        Self::at(folder.join(".rosen").join("server"), 0, true)
    }

    pub(crate) fn link_folder(app_data: &Path, project: u32, root: &Path) -> std::io::Result<()> {
        use std::io::Write;
        let root = std::fs::canonicalize(root)?;
        let directory = app_data.join("rosen").join(project.to_string());
        std::fs::create_dir_all(&directory)?;
        let mut temporary = tempfile::NamedTempFile::new_in(&directory)?;
        temporary.write_all(root.to_string_lossy().as_bytes())?;
        temporary.persist(directory.join(PROJECT_FOLDER))?;
        Ok(())
    }

    fn at(data_dir: PathBuf, project: u32, folder_backed: bool) -> Self {
        // The endpoint carries a hash of the data folder, so two Velocity installs with
        // different app data (a development build beside the installed app) never share
        // a server. FNV-1a, because the name must stay the same across Velocity versions.
        let lowered = data_dir.to_string_lossy().to_lowercase();
        let hash = lowered
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
            });
        let name = format!("velocity-rosen-{project}-{hash:016x}");
        // A pipe name on Windows. Elsewhere a socket path, which must be absolute: the
        // server runs from its data folder, Velocity from its own.
        let endpoint = if cfg!(windows) {
            Endpoint::new(name)
        } else {
            let socket_dir = if folder_backed {
                std::env::temp_dir()
            } else {
                data_dir.clone()
            };
            Endpoint::new(socket_dir.join(format!("{name}.sock")).to_string_lossy())
        };
        Self {
            endpoint,
            data_dir,
            warm_pool_dir: None,
        }
    }

    /// Keeps the server's warm pool under `folder` (Rosen R3). A ready workspace is handed
    /// out by renaming it, which works only within one volume, so the pool has to sit on
    /// the volume the project's workspaces are made on; left in the data folder, on another
    /// volume, it is never used and every workspace is a full copy with nothing built.
    /// It takes effect when this starts the server: one already running keeps its own.
    pub fn with_warm_pool_dir(mut self, folder: Option<PathBuf>) -> Self {
        self.warm_pool_dir = folder;
        self
    }

    pub(crate) fn warm_pool_dir(&self) -> Option<&Path> {
        self.warm_pool_dir.as_deref()
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Raises this machine's workspace disk budget (Rosen R3 "Load limits") to what is
    /// charged to it now plus `room` bytes, when it is lower. Never lowers it.
    ///
    /// Rosen admits a new workspace against a budget of logical bytes, 100 GiB unless
    /// raised, and a request over it waits rather than fails. Every pooled and taken
    /// workspace counts in full with its build output, block clones included, so a pool
    /// and a dozen agents on a project whose built workspace is several GiB would leave
    /// the next thread waiting with nothing to say why.
    pub async fn raise_disk_budget(&self, room: u64) -> Result<(), RosenError> {
        let client = requests(&self.endpoint).await?;
        let load = client.load_limits(None, None).await?;
        let wanted = load.charged_bytes.saturating_add(room);
        if load.settings.disk_bytes < wanted {
            client.load_limits(None, Some(wanted)).await?;
        }
        Ok(())
    }

    /// Raises this machine's pool target for a swarm, retaining the project's configured
    /// standing pool. The caller retains its largest launch target across concurrent swarms.
    pub async fn reserve_pool(&self, count: usize) -> Result<(), RosenError> {
        let client = self.connect().await?;
        let standing = client.base_config().await?.warm_pool.size as usize;
        client.set_pool_size(count.max(standing)).await?;
        Ok(())
    }

    /// Connects to the server, starting it first when none listens.
    pub async fn connect(&self) -> Result<Arc<Connection>, RosenError> {
        self.connect_noting_start()
            .await
            .map(|(connection, _)| connection)
    }

    async fn connect_noting_start(&self) -> Result<(Arc<Connection>, bool), RosenError> {
        match requests(&self.endpoint).await {
            Err(ClientError::NotRunning(_)) => {}
            connected => return Ok((connected?, false)),
        }
        let mut child = self.start()?;
        Ok((self.wait_until_listening(&mut child).await?, true))
    }

    fn start(&self) -> Result<Child, RosenError> {
        self.start_binary(&binary_folder()?.join(executable("rosen-server")))
    }

    /// Starts the server from `binary`, on this project's data folder and endpoint.
    pub(crate) fn start_binary(&self, binary: &Path) -> Result<Child, RosenError> {
        std::fs::create_dir_all(&self.data_dir).map_err(|source| RosenError::Io {
            action: "create",
            path: self.data_dir.clone(),
            source,
        })?;
        let spawned = match self.server_command(binary, true)?.spawn() {
            Ok(child) => Ok(child),
            Err(_) => self.server_command(binary, false)?.spawn(),
        };
        spawned.map_err(|error| RosenError::Start(format!("{}: {error}", binary.display())))
    }

    /// The server's command line, writing its output to `server.log` in its data folder.
    fn server_command(&self, binary: &Path, leave_job: bool) -> Result<Command, RosenError> {
        let log_path = self.data_dir.join("server.log");
        let log = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .map_err(|source| RosenError::Io {
                action: "open",
                path: log_path.clone(),
                source,
            })?;
        let error_log = log.try_clone().map_err(|source| RosenError::Io {
            action: "open",
            path: log_path,
            source,
        })?;
        let mut command = crate::process::server_command(binary, leave_job);
        command
            .arg("--data-dir")
            .arg(&self.data_dir)
            .arg("--endpoint")
            .arg(self.endpoint.as_str());
        if let Some(folder) = &self.warm_pool_dir {
            command.arg("--warm-pool-dir").arg(folder);
        }
        self.add_listener(&mut command)?;
        // The server Velocity starts for a project is the one that pushes its mirror (Rosen
        // R1, rosen#401). Without the mark a new or re-imported project's server never
        // pushes; every push still checks the remote tip first, so a second machine's
        // server is refused rather than overwriting. The variable, unlike the flag, is
        // ignored by a Rosen older than #401 instead of stopping it from starting.
        command.env("ROSEN_MIRROR_OWNER", &self.data_dir);
        command
            .current_dir(&self.data_dir)
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(error_log);
        Ok(command)
    }

    fn add_listener(&self, command: &mut Command) -> Result<(), RosenError> {
        let listen_path = self.data_dir.join("velocity-listen");
        match std::fs::read_to_string(&listen_path) {
            Ok(address) => {
                command.arg("--listen").arg(address.trim());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(RosenError::Io {
                    action: "read",
                    path: listen_path,
                    source,
                });
            }
        }
        Ok(())
    }

    pub(crate) async fn wait_until_listening(
        &self,
        child: &mut Child,
    ) -> Result<Arc<Connection>, RosenError> {
        let deadline = tokio::time::Instant::now() + START_TIMEOUT;
        let mut delay = Duration::from_millis(20);
        loop {
            match requests(&self.endpoint).await {
                Err(ClientError::NotRunning(_)) => {}
                connected => return Ok(connected?),
            }
            if let Ok(Some(status)) = child.try_wait()
                && status.code() != Some(EXIT_ALREADY_RUNNING)
            {
                return Err(RosenError::Start(format!(
                    "it exited with {status}; see {}",
                    self.data_dir.join("server.log").display()
                )));
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(RosenError::Start(format!(
                    "it did not listen on {} within {START_TIMEOUT:?}",
                    self.endpoint
                )));
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_millis(250));
        }
    }

    /// Asks the server to exit, if one listens, and waits until it has.
    pub(crate) async fn stop(&self) {
        if let Ok(client) = Connection::dedicated(&self.endpoint).await {
            let _ = client.shutdown().await;
        }
        self.wait_until_stopped().await;
        crate::connection::forget(&self.endpoint);
    }

    /// Waits, a few seconds at most, until the server no longer accepts connections: its
    /// shutdown is answered before it has drained and exited, and an update or a restart
    /// must not meet the old server.
    async fn wait_until_stopped(&self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            if let Err(ClientError::NotRunning(_)) = Connection::dedicated(&self.endpoint).await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Imports the project's history (Rosen R1), once per project. Its default branch
    /// becomes the base, and every land is pushed to its mirror.
    pub async fn import(&self, client: &Client, source: &ImportSource) -> Result<(), RosenError> {
        self.import_noting_fresh(client, source).await.map(|_| ())
    }

    async fn import_noting_fresh(
        &self,
        client: &Client,
        source: &ImportSource,
    ) -> Result<bool, RosenError> {
        let marker = self.data_dir.join(IMPORTED_MARKER);
        if marker.is_file() {
            return Ok(false);
        }
        let mut imported = Err(ClientError::Disconnected);
        // Reading git history runs git many times; one retry rides out a git process that
        // failed to start.
        for attempt in 0..2 {
            if attempt > 0 {
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            imported = client
                .import(
                    &source.source,
                    Some(source.branch.as_str()),
                    source.mirror.as_deref(),
                )
                .await;
            let already = matches!(&imported, Err(ClientError::Server(body)) if body.code == ErrorCode::Refused);
            if imported.is_ok() || already {
                break;
            }
        }
        match imported {
            Ok(_) => {}
            // The project already has a base: an earlier import finished before its
            // marker was written.
            Err(ClientError::Server(body)) if body.code == ErrorCode::Refused => {}
            Err(error) => return Err(error.into()),
        }
        std::fs::write(&marker, b"").map_err(|source| RosenError::Io {
            action: "write",
            path: marker,
            source,
        })?;
        Ok(true)
    }
}

/// Where a project's history is imported from, and where lands are pushed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportSource {
    pub source: String,
    pub branch: String,
    pub mirror: Option<String>,
}

impl ImportSource {
    /// The checkout's `origin` when it has one, so Rosen's base starts at the remote's
    /// tip and every push to it is a fast-forward (R1); a local checkout's own history
    /// otherwise, with no mirror.
    pub fn for_checkout(repository: &Path, branch: &str) -> Self {
        let origin = origin_url(repository).map(|url| absolute_remote(&url, repository));
        Self {
            source: origin
                .clone()
                .unwrap_or_else(|| repository.to_string_lossy().into_owned()),
            branch: branch.to_owned(),
            mirror: origin,
        }
    }
}

/// `url` unchanged when it names a remote, or a local path made absolute against the
/// checkout: the server resolves relative paths from its own data folder, not the
/// checkout's. A remote has a scheme (`https://`) or is scp-like (`git@host:path`); a
/// Windows drive path (`C:\...`) has its colon second and is local.
pub(crate) fn absolute_remote(url: &str, repository: &Path) -> String {
    let scp_like = url
        .find(':')
        .is_some_and(|colon| colon > 1 && !url[..colon].contains(['/', '\\']));
    let path = Path::new(url);
    if url.contains("://") || scp_like || path.is_absolute() {
        return url.to_owned();
    }
    repository.join(path).to_string_lossy().into_owned()
}

fn origin_url(repository: &Path) -> Option<String> {
    let mut command = crate::process::hidden_command("git");
    command
        .arg("-C")
        .arg(repository)
        .args(["remote", "get-url", "origin"])
        .stdin(Stdio::null());
    let output = command.output().ok()?;
    let url = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !url.is_empty()).then_some(url)
}

/// The servers Velocity has made ready this run. Making one project's server ready is
/// serialised, so two threads starting at once never race to start it or import twice;
/// other projects' servers and the registry never wait on it. Stopping them for an update
/// or exit takes the same per-project lock, so a start already under way finishes first
/// and is stopped, and none begins until they are started again.
#[derive(Debug)]
pub struct Servers {
    app_data: PathBuf,
    registry: std::sync::Mutex<BTreeMap<u32, ProjectServer>>,
    starting: std::sync::Mutex<BTreeMap<u32, Arc<tokio::sync::Mutex<()>>>>,
    stopped: AtomicBool,
    /// Projects whose running server this run has compared with its own Rosen build (#515).
    compared: std::sync::Mutex<BTreeSet<u32>>,
    /// Projects whose server process was freshly started or imported since last checked (#576).
    started: std::sync::Mutex<BTreeSet<u32>>,
    /// What became of each server found running another build, until the app shows it.
    updates: std::sync::Mutex<BTreeMap<u32, ServerUpdate>>,
}

impl Servers {
    pub fn new(app_data: PathBuf) -> Self {
        Self {
            app_data,
            registry: std::sync::Mutex::new(BTreeMap::new()),
            starting: std::sync::Mutex::new(BTreeMap::new()),
            stopped: AtomicBool::new(false),
            compared: std::sync::Mutex::new(BTreeSet::new()),
            started: std::sync::Mutex::new(BTreeSet::new()),
            updates: std::sync::Mutex::new(BTreeMap::new()),
        }
    }

    /// Whether the servers are stopped for an update or exit; [`Self::ready`] refuses until
    /// [`Self::restart_all`].
    pub fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    fn start_lock(&self, project: u32) -> Arc<tokio::sync::Mutex<()>> {
        self.starting
            .lock()
            .map(|mut starting| Arc::clone(starting.entry(project).or_default()))
            .unwrap_or_default()
    }

    /// The project's server, started and imported, with its warm pool under
    /// `warm_pool_dir` when given. A server that stopped since is started again.
    pub async fn ready(
        &self,
        project: u32,
        source: &ImportSource,
        warm_pool_dir: Option<PathBuf>,
    ) -> Result<ProjectServer, RosenError> {
        let server = ProjectServer::new(&self.app_data, project).with_warm_pool_dir(warm_pool_dir);
        self.ready_server(project, server, Some(source)).await
    }

    pub async fn ready_local(
        &self,
        project: u32,
        root: &Path,
        warm_pool_dir: Option<PathBuf>,
    ) -> Result<ProjectServer, RosenError> {
        let server = ProjectServer::for_folder(root)?.with_warm_pool_dir(warm_pool_dir);
        self.ready_server(project, server, None).await
    }

    async fn ready_server(
        &self,
        project: u32,
        server: ProjectServer,
        source: Option<&ImportSource>,
    ) -> Result<ProjectServer, RosenError> {
        let start_lock = self.start_lock(project);
        let _starting = start_lock.lock().await;
        // Checked under the lock [`Self::shutdown_all`] takes, so no start slips in after it.
        if self.stopped() {
            return Err(RosenError::Stopped);
        }
        self.update_once(project, &server).await;
        let (client, started_new) = server.connect_noting_start().await?;
        // Stopped while this start was connecting: the server it started goes too.
        if self.stopped() {
            server.stop().await;
            return Err(RosenError::Stopped);
        }
        // Recorded before the import, so a server whose import fails is still stopped at
        // exit.
        if let Ok(mut registry) = self.registry.lock() {
            registry.insert(project, server.clone());
        }
        let imported_fresh = if let Some(source) = source {
            server.import_noting_fresh(&client, source).await?
        } else {
            client.project_config(None).await?;
            false
        };
        if (started_new || imported_fresh)
            && let Ok(mut started) = self.started.lock()
        {
            started.insert(project);
        }
        Ok(server)
    }

    /// Replaces `project`'s running server when it is another Rosen build than this
    /// Velocity's (#515). Once per run, the first time the project is made ready: every
    /// watcher and land this run starts on the server comes after that, so none is cut
    /// short, and a server that could not be replaced is left alone until the next start.
    async fn update_once(&self, project: u32, server: &ProjectServer) {
        let first = self
            .compared
            .lock()
            .is_ok_and(|mut compared| compared.insert(project));
        if !first {
            return;
        }
        if let Some(update) = server.update_running().await
            && let Ok(mut updates) = self.updates.lock()
        {
            updates.insert(project, update);
        }
    }

    /// What became of `project`'s server when it was found running another Rosen build,
    /// handed over once for the app to show.
    pub fn take_update(&self, project: u32) -> Option<ServerUpdate> {
        self.updates.lock().ok()?.remove(&project)
    }

    /// Reports once when `project`'s server was freshly started or imported (#576).
    pub fn take_started(&self, project: u32) -> bool {
        self.started
            .lock()
            .is_ok_and(|mut started| started.remove(&project))
    }

    /// Every `(project, server)` pair this run has started (#576).
    pub fn registered_projects(&self) -> Vec<(u32, ProjectServer)> {
        self.registry
            .lock()
            .map(|registry| {
                registry
                    .iter()
                    .map(|(&project, server)| (project, server.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    fn registered(&self) -> Vec<ProjectServer> {
        self.registry
            .lock()
            .map(|registry| registry.values().cloned().collect())
            .unwrap_or_default()
    }

    /// Starts again every server this run used, after [`Self::shutdown_all`] when the
    /// update it made way for didn't install. Watchers journal meanwhile and catch up.
    pub async fn restart_all(&self) {
        self.stopped.store(false, Ordering::SeqCst);
        for (project, server) in self.registered_projects() {
            if let Ok((_, true)) = server.connect_noting_start().await
                && let Ok(mut started) = self.started.lock()
            {
                started.insert(project);
            }
        }
    }

    /// Stops every server this run started or began to start, so an update can replace
    /// their binaries, and keeps them stopped until [`Self::restart_all`]. Agents' watchers
    /// journal offline until the next start (Rosen R2).
    pub async fn shutdown_all(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        // The servers that started first, without their start locks: a long first import
        // holds one, and stopping its server ends that import.
        for server in self.registered() {
            server.stop().await;
        }
        // Then, under its lock, each project that began to start, for a start that was
        // still connecting. Such a start also stops its own server once connected.
        let projects: Vec<u32> = self
            .starting
            .lock()
            .map(|starting| starting.keys().copied().collect())
            .unwrap_or_default();
        for project in projects {
            let start_lock = self.start_lock(project);
            let _starting = start_lock.lock().await;
            ProjectServer::new(&self.app_data, project).stop().await;
        }
    }
}

/// The folder holding Rosen's binaries: [`BINARY_FOLDER_ENV`], then the copy built into
/// a release of Velocity, then beside Velocity's executable, then the first `PATH` folder
/// that has them.
pub fn binary_folder() -> Result<PathBuf, RosenError> {
    program_folder("rosen-server").ok_or(RosenError::MissingBinary)
}

/// Queue commands need only the CLI: a git project may have `rosen run` installed
/// without a Rosen server. Use the same configured and bundled locations for both.
pub(crate) fn program_folder(program: &str) -> Option<PathBuf> {
    let program = executable(program);
    let configured = std::env::var_os(BINARY_FOLDER_ENV).map(PathBuf::from);
    let configured = configured.or_else(crate::bundled::folder);
    let beside_app = std::env::current_exe()
        .ok()
        .and_then(|path| path.parent().map(Path::to_path_buf));
    let on_path = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    configured
        .into_iter()
        .chain(beside_app)
        .chain(on_path)
        .find(|folder| {
            !crate::agent_run::is_command_folder(folder) && folder.join(&program).is_file()
        })
}

pub(crate) fn executable(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}
