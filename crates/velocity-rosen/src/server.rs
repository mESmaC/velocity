//! Each project's Rosen server. Velocity starts it with an explicit data folder and
//! endpoint, since one server holds one project, and imports the project's repository once.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use rosen_client::protocol::ErrorCode;
use rosen_client::{Client, Endpoint, Error as ClientError};

use crate::RosenError;

/// A folder holding `rosen-server` and `rosen`, for a build that doesn't ship them beside
/// Velocity's own executable (a development build).
pub const BINARY_FOLDER_ENV: &str = "VELOCITY_ROSEN_BIN";

/// Written once the repository is imported, so a restart doesn't read the whole git
/// history again only to be told the project already has a base.
const IMPORTED_MARKER: &str = "velocity-imported";
/// rosen-server exits with this when another server already holds the endpoint.
const EXIT_ALREADY_RUNNING: i32 = 3;
const START_TIMEOUT: Duration = Duration::from_secs(20);

/// Where one project's server keeps its data and listens.
#[derive(Clone, Debug)]
pub struct ProjectServer {
    endpoint: Endpoint,
    data_dir: PathBuf,
}

impl ProjectServer {
    /// The server for `project`, keeping its data under `<app data>\rosen\<project>`.
    pub fn new(app_data: &Path, project: u32) -> Self {
        let data_dir = app_data.join("rosen").join(project.to_string());
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
            Endpoint::new(data_dir.join(format!("{name}.sock")).to_string_lossy())
        };
        Self { endpoint, data_dir }
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    /// Connects to the server, starting it first when none listens.
    pub async fn connect(&self) -> Result<Client, RosenError> {
        match Client::connect(&self.endpoint).await {
            Err(ClientError::NotRunning(_)) => {}
            connected => return Ok(connected?),
        }
        let mut child = self.start()?;
        self.wait_until_listening(&mut child).await
    }

    fn start(&self) -> Result<Child, RosenError> {
        std::fs::create_dir_all(&self.data_dir).map_err(|source| RosenError::Io {
            action: "create",
            path: self.data_dir.clone(),
            source,
        })?;
        let binary = binary_folder()?.join(executable("rosen-server"));
        let spawned = match self.server_command(&binary, true)?.spawn() {
            Ok(child) => Ok(child),
            Err(_) => self.server_command(&binary, false)?.spawn(),
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
            .arg(self.endpoint.as_str())
            .current_dir(&self.data_dir)
            .stdin(Stdio::null())
            .stdout(log)
            .stderr(error_log);
        Ok(command)
    }

    async fn wait_until_listening(&self, child: &mut Child) -> Result<Client, RosenError> {
        let deadline = tokio::time::Instant::now() + START_TIMEOUT;
        let mut delay = Duration::from_millis(20);
        loop {
            match Client::connect(&self.endpoint).await {
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

    /// Waits, a few seconds at most, until the server no longer accepts connections: its
    /// shutdown is answered before it has drained and exited, and an update or a restart
    /// must not meet the old server.
    async fn wait_until_stopped(&self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while tokio::time::Instant::now() < deadline {
            if let Err(ClientError::NotRunning(_)) = Client::connect(&self.endpoint).await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Imports the project's history (Rosen R1), once per project. Its default branch
    /// becomes the base, and every land is pushed to its mirror.
    pub async fn import(&self, client: &Client, source: &ImportSource) -> Result<(), RosenError> {
        let marker = self.data_dir.join(IMPORTED_MARKER);
        if marker.is_file() {
            return Ok(());
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
        })
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
fn absolute_remote(url: &str, repository: &Path) -> String {
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

/// The servers Velocity has made ready this run. Making one ready is serialised, so two
/// threads starting at once never race to start the server or import twice.
#[derive(Debug)]
pub struct Servers {
    app_data: PathBuf,
    ready: tokio::sync::Mutex<BTreeMap<u32, ProjectServer>>,
}

impl Servers {
    pub fn new(app_data: PathBuf) -> Self {
        Self {
            app_data,
            ready: tokio::sync::Mutex::new(BTreeMap::new()),
        }
    }

    /// The project's server, started and imported. A server that stopped since is
    /// started again.
    pub async fn ready(
        &self,
        project: u32,
        source: &ImportSource,
    ) -> Result<ProjectServer, RosenError> {
        let mut ready = self.ready.lock().await;
        let server = ProjectServer::new(&self.app_data, project);
        let client = server.connect().await?;
        // Recorded before the import, so a server whose import fails is still stopped at
        // exit.
        ready.insert(project, server.clone());
        server.import(&client, source).await?;
        Ok(server)
    }

    /// The project's server if this run already made it ready.
    pub async fn get(&self, project: u32) -> Option<ProjectServer> {
        self.ready.lock().await.get(&project).cloned()
    }

    /// Starts again every server this run used, after [`Self::shutdown_all`] when the
    /// update it made way for didn't install. Watchers journal meanwhile and catch up.
    pub async fn restart_all(&self) {
        let servers: Vec<ProjectServer> = self.ready.lock().await.values().cloned().collect();
        for server in servers {
            let _ = server.connect().await;
        }
    }

    /// Stops every server this run started or used, so an update can replace their
    /// binaries. Agents' watchers journal offline until the next start (Rosen R2).
    pub async fn shutdown_all(&self) {
        let servers: Vec<ProjectServer> = self.ready.lock().await.values().cloned().collect();
        for server in servers {
            if let Ok(client) = Client::connect(server.endpoint()).await {
                let _ = client.shutdown().await;
            }
            server.wait_until_stopped().await;
        }
    }
}

/// The folder holding Rosen's binaries: [`BINARY_FOLDER_ENV`], then beside Velocity's
/// executable (the installed app), then the first `PATH` folder that has them.
pub fn binary_folder() -> Result<PathBuf, RosenError> {
    let server = executable("rosen-server");
    let configured = std::env::var_os(BINARY_FOLDER_ENV).map(PathBuf::from);
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
        .find(|folder| folder.join(&server).is_file())
        .ok_or(RosenError::MissingBinary)
}

fn executable(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}
