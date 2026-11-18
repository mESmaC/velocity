//! Local graph worker. All connections for a project share one live index.

use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rosen_client::Endpoint;
use rosen_client::protocol::transport::{self, BoxStream, Listener};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::usage::ToolOutcome;
use crate::{Error, Index, Tools};

pub const PROJECT_SWITCH: &str = "--graph-project";
const FRAME_LIMIT: u64 = 256 * 1024;
const IO_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const QUERY_TIMEOUT: Duration = Duration::from_secs(90);
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Serialize, Deserialize)]
pub(crate) enum Action {
    Refresh,
    Map,
    Task(String),
    Tool { name: String, arguments: Value },
    WarmBase,
}

#[derive(Serialize, Deserialize)]
struct Request {
    workspace: PathBuf,
    rosen: Option<String>,
    changed: Vec<String>,
    action: Action,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Reply {
    pub text: String,
    pub outcome: ToolOutcome,
    pub failed: bool,
}

impl Reply {
    fn plain(text: String) -> Self {
        Self {
            text,
            outcome: ToolOutcome::Other,
            failed: false,
        }
    }
}

pub(crate) struct Project {
    cache: PathBuf,
    executable: PathBuf,
    address: Endpoint,
    rosen: Option<Endpoint>,
}

impl Project {
    pub fn new(cache: PathBuf, executable: PathBuf, rosen: Option<Endpoint>) -> Self {
        let address = address(&cache);
        Self {
            cache,
            executable,
            address,
            rosen,
        }
    }

    pub fn request(
        &self,
        workspace: &Path,
        changed: Vec<String>,
        action: Action,
        timeout: Duration,
    ) -> Result<Reply, String> {
        let request = Request {
            workspace: workspace.to_path_buf(),
            rosen: self.rosen.as_ref().map(ToString::to_string),
            changed,
            action,
        };
        runtime()
            .map_err(|error| error.to_string())?
            .block_on(async {
                tokio::time::timeout(timeout, self.exchange(request))
                    .await
                    .map_err(|_| "The project graph is busy".to_owned())?
            })
    }

    async fn exchange(&self, request: Request) -> Result<Reply, String> {
        let mut stream = self.connect().await.map_err(|error| error.to_string())?;
        write(&mut stream, &request)
            .await
            .map_err(|error| error.to_string())?;
        read::<Result<Reply, String>>(&mut stream)
            .await
            .map_err(|error| error.to_string())?
    }

    async fn connect(&self) -> io::Result<BoxStream> {
        match transport::connect(&self.address).await {
            Ok(stream) => return Ok(stream),
            Err(error) if transport::is_not_running(&error) => {}
            Err(error) => return Err(error),
        }
        std::fs::create_dir_all(&self.cache)?;
        let startup = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.cache.join("worker-start.lock"))?;
        let started = tokio::time::Instant::now();
        let mut launched = false;
        loop {
            match transport::connect(&self.address).await {
                Err(error)
                    if transport::is_not_running(&error) && started.elapsed() < IO_TIMEOUT =>
                {
                    if !launched && startup.try_lock().is_ok() {
                        self.start()?;
                        launched = true;
                    }
                    tokio::time::sleep(Duration::from_millis(25)).await;
                }
                result => return result,
            }
        }
    }

    fn start(&self) -> io::Result<()> {
        let mut child = crate::files::background_command(&self.executable)
            .arg(PROJECT_SWITCH)
            .arg(&self.cache)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

fn address(cache: &Path) -> Endpoint {
    let key = crate::status::workspace_key(cache);
    if cfg!(windows) {
        Endpoint::new(format!("velocity-graph-v1-{key}"))
    } else {
        Endpoint::new(
            std::env::temp_dir()
                .join(format!("velocity-graph-{}", &key[..32]))
                .join("worker.sock")
                .to_string_lossy(),
        )
    }
}

fn runtime() -> io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
}

async fn read<T: serde::de::DeserializeOwned>(stream: &mut BoxStream) -> io::Result<T> {
    let mut bytes = Vec::new();
    BufReader::new(stream)
        .take(FRAME_LIMIT)
        .read_until(b'\n', &mut bytes)
        .await?;
    if bytes.last() != Some(&b'\n') {
        return Err(io::Error::other("Incomplete project graph message"));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

async fn write(stream: &mut BoxStream, message: &impl Serialize) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(message)?;
    if bytes.len() as u64 >= FRAME_LIMIT {
        return Err(io::Error::other(
            "Project graph message exceeds its frame limit",
        ));
    }
    bytes.push(b'\n');
    stream.write_all(&bytes).await
}

/// Requests are handled serially, so a snapshot cannot change during source rendering.
pub fn serve_project(arguments: &[OsString]) -> Result<(), Error> {
    let [cache] = arguments else {
        return Err(Error::Invalid(
            "Usage: --graph-project <project-cache>".into(),
        ));
    };
    let cache = Path::new(cache);
    let runtime = runtime()?;
    let mut listener = match runtime.block_on(async { Listener::bind(&address(cache)) }) {
        Err(error) if error.kind() == io::ErrorKind::AddrInUse => return Ok(()),
        result => result?,
    };
    let mut graph = ProjectGraph::default();
    while let Ok(stream) =
        runtime.block_on(async { tokio::time::timeout(IDLE_TIMEOUT, listener.accept()).await })
    {
        let mut stream = stream?;
        let Ok(Ok(request)) = runtime.block_on(async {
            tokio::time::timeout(IO_TIMEOUT, read::<Request>(&mut stream)).await
        }) else {
            continue;
        };
        let response = graph.handle(cache, request);
        let _ = runtime.block_on(async {
            tokio::time::timeout(IO_TIMEOUT, write(&mut stream, &response)).await
        });
    }
    Ok(())
}

#[derive(Default)]
struct ProjectGraph {
    endpoint: Option<String>,
    tools: Option<Tools>,
}

impl ProjectGraph {
    fn handle(&mut self, cache: &Path, request: Request) -> Result<Reply, String> {
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run(cache, request)))
                .unwrap_or_else(|_| {
                    Err(Error::Invalid(
                        "The project graph stopped on an internal error".into(),
                    ))
                });
        if result.is_err() {
            self.tools = None;
        }
        result.map_err(|error| error.to_string())
    }

    fn run(&mut self, cache: &Path, request: Request) -> Result<Reply, Error> {
        let tools = self.select(cache, &request)?;
        Ok(match request.action {
            Action::Refresh | Action::WarmBase => Reply::plain(String::new()),
            Action::Map => Reply::plain(tools.map()),
            Action::Task(title) => Reply::plain(tools.task_matches(&title).unwrap_or_default()),
            Action::Tool { name, arguments } => match tools.measured_call(&name, &arguments) {
                Ok((text, outcome)) => Reply {
                    text,
                    outcome,
                    failed: false,
                },
                Err(error) => Reply {
                    text: error.message,
                    outcome: error.outcome,
                    failed: true,
                },
            },
        })
    }

    fn select(&mut self, cache: &Path, request: &Request) -> Result<&mut Tools, Error> {
        let warm = matches!(request.action, Action::WarmBase);
        let endpoint = request.rosen.clone().map(Endpoint::new);
        let rosen =
            warm || rosen_client::snapshot::WorkspaceMarker::path(&request.workspace).is_file();
        if self.tools.as_ref().is_some_and(|tools| {
            tools.index().is_rosen() != rosen || self.endpoint != request.rosen
        }) {
            self.tools = None;
        }
        if self.tools.is_none() {
            let index = if warm {
                let endpoint = endpoint
                    .ok_or_else(|| Error::Invalid("Missing project Rosen endpoint".into()))?;
                Index::open_revision(endpoint, "base", cache)?
            } else {
                Index::open_workspace(&request.workspace, cache, endpoint)?
            };
            self.tools = Some(Tools::new(index));
            self.endpoint = request.rosen.clone();
        }
        let tools = self
            .tools
            .as_mut()
            .ok_or_else(|| Error::Invalid("Project graph unavailable".into()))?;
        if warm {
            tools.select_base()?;
        } else {
            tools.select_workspace(&request.workspace)?;
            if !rosen {
                tools.refresh(&request.changed)?;
            }
        }
        Ok(tools)
    }
}

pub fn warm_project(executable: &Path, app_data: &Path, project: &str, endpoint: Endpoint) {
    let project = Project::new(
        crate::usage::folder(app_data, project),
        executable.into(),
        Some(endpoint),
    );
    let _ = project.request(
        Path::new("base"),
        Vec::new(),
        Action::WarmBase,
        QUERY_TIMEOUT,
    );
}
