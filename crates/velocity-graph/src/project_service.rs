//! Local graph worker. All connections for a project share one live index.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use rosen_client::Endpoint;
use rosen_client::protocol::transport::{self, BoxStream, Listener};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

use crate::signature_impact::StaleReferenceWarning;
use crate::usage::ToolOutcome;
use crate::{Error, Index, Tools};

pub const PROJECT_SWITCH: &str = "--graph-project";
const FRAME_LIMIT: u64 = 256 * 1024;
const MENTION_BATCH_BYTES: usize = 64 * 1024;
const MENTION_REFRESH_INTERVAL: Duration = Duration::from_secs(30);
const IO_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const QUERY_TIMEOUT: Duration = Duration::from_secs(90);
const SIGNATURE_QUERY_TIMEOUT: Duration = Duration::from_secs(5);
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Serialize, Deserialize)]
pub(crate) enum Action {
    Refresh,
    Map,
    Task(String),
    Tool {
        name: String,
        arguments: Value,
    },
    WarmBase,
    Mentions(Vec<(String, String)>),
    ChangeSignatureWarnings {
        change: String,
    },
    DispatchSignatureWarnings {
        texts: Vec<String>,
        exclude_change: Option<String>,
    },
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    mentions: Vec<BTreeSet<String>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    signature_warnings: Vec<StaleReferenceWarning>,
}

impl Reply {
    fn plain(text: String) -> Self {
        Self {
            text,
            outcome: ToolOutcome::Other,
            failed: false,
            mentions: Vec::new(),
            signature_warnings: Vec::new(),
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
    mention_refresh: Option<(PathBuf, Instant)>,
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
                    mentions: Vec::new(),
                    signature_warnings: Vec::new(),
                },
                Err(error) => Reply {
                    text: error.message,
                    outcome: error.outcome,
                    failed: true,
                    mentions: Vec::new(),
                    signature_warnings: Vec::new(),
                },
            },
            Action::Mentions(texts) => {
                let graph = &tools.index().graph;
                let mentions = texts
                    .into_iter()
                    .map(|(title, body)| {
                        let mut paths = graph.mentioned_paths(&title);
                        paths.extend(graph.mentioned_paths(&body));
                        paths
                    })
                    .collect();
                Reply {
                    mentions,
                    ..Reply::plain(String::new())
                }
            }
            Action::ChangeSignatureWarnings { change } => {
                let signature_warnings = tools
                    .index()
                    .change_signature_warnings(&change)
                    .unwrap_or_default();
                Reply {
                    signature_warnings,
                    ..Reply::plain(String::new())
                }
            }
            Action::DispatchSignatureWarnings {
                texts,
                exclude_change,
            } => {
                let signature_warnings = tools
                    .index()
                    .dispatch_signature_warnings(&texts, exclude_change.as_deref())
                    .unwrap_or_default();
                Reply {
                    signature_warnings,
                    ..Reply::plain(String::new())
                }
            }
        })
    }

    fn select(&mut self, cache: &Path, request: &Request) -> Result<&mut Tools, Error> {
        let mentions = matches!(request.action, Action::Mentions(_));
        let signature_query = matches!(
            request.action,
            Action::ChangeSignatureWarnings { .. } | Action::DispatchSignatureWarnings { .. }
        );
        let base = matches!(request.action, Action::WarmBase)
            || (mentions || signature_query) && request.rosen.is_some();
        let rosen =
            base || rosen_client::snapshot::WorkspaceMarker::path(&request.workspace).is_file();
        let address = request.rosen.as_ref().filter(|_| rosen);
        if self.tools.as_ref().is_some_and(|tools| {
            tools.index().is_rosen() != rosen || self.endpoint.as_ref() != address
        }) {
            self.tools = None;
        }
        let same_workspace = self
            .tools
            .as_ref()
            .is_some_and(|tools| tools.index().root() == request.workspace);
        if self.tools.is_none() {
            let endpoint = address.cloned().map(Endpoint::new);
            let index = if base {
                let endpoint = endpoint
                    .ok_or_else(|| Error::Invalid("Missing project Rosen endpoint".into()))?;
                Index::open_revision(endpoint, "base", cache)?
            } else {
                Index::open_workspace(&request.workspace, cache, endpoint)?
            };
            self.tools = Some(Tools::new(index));
            self.endpoint = address.cloned();
        }
        let tools = self
            .tools
            .as_mut()
            .ok_or_else(|| Error::Invalid("Project graph unavailable".into()))?;
        if base {
            tools.select_base()?;
        } else {
            tools.select_workspace(&request.workspace)?;
            if !rosen {
                let refresh = mentions
                    && self.mention_refresh.as_ref().is_none_or(|(root, checked)| {
                        root != &request.workspace || checked.elapsed() >= MENTION_REFRESH_INTERVAL
                    });
                if refresh {
                    // Issue queries have no connection watching this checkout for edits.
                    if same_workspace {
                        tools.refresh(&[String::new()])?;
                    }
                    self.mention_refresh = Some((request.workspace.clone(), Instant::now()));
                } else {
                    tools.refresh(&request.changed)?;
                }
            }
        }
        Ok(tools)
    }
}

/// Resolve issue references against the same worker that serves agent graph queries.
pub fn mentioned_paths(
    executable: &Path,
    app_data: &Path,
    project: &str,
    root: &Path,
    endpoint: Option<Endpoint>,
    texts: &[(&str, &str)],
) -> Vec<BTreeSet<String>> {
    let project = Project::new(
        crate::usage::folder(app_data, project),
        executable.into(),
        endpoint,
    );
    let mut paths = vec![BTreeSet::new(); texts.len()];
    let mut remaining = texts;
    let mut offset = 0;
    while let Some(first) = remaining.first() {
        let mut bytes = 0;
        let length = remaining
            .iter()
            .take_while(|text| {
                bytes += mention_size(text);
                bytes <= MENTION_BATCH_BYTES
            })
            .count()
            .max(1);
        let (batch, rest) = remaining.split_at(length);
        if length > 1 || mention_size(first) <= FRAME_LIMIT as usize - 1024 {
            let action = Action::Mentions(
                batch
                    .iter()
                    .map(|&(title, body)| (title.to_owned(), body.to_owned()))
                    .collect(),
            );
            let Ok(reply) = project.request(root, Vec::new(), action, QUERY_TIMEOUT) else {
                break;
            };
            for (path, found) in paths[offset..offset + length]
                .iter_mut()
                .zip(reply.mentions)
            {
                *path = found;
            }
        }
        remaining = rest;
        offset += length;
    }
    paths
}

fn mention_size(text: &(&str, &str)) -> usize {
    serde_json::to_vec(text).map_or(usize::MAX, |bytes| bytes.len() + 1)
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

/// Checks a Rosen change for stale references against open and recently landed changes (#542).
pub fn change_signature_warnings(
    executable: &Path,
    app_data: &Path,
    project: &str,
    endpoint: &str,
    change: &str,
) -> Vec<StaleReferenceWarning> {
    if !crate::usage::enabled(app_data, project) {
        return Vec::new();
    }
    let project = Project::new(
        crate::usage::folder(app_data, project),
        executable.into(),
        Some(Endpoint::new(endpoint)),
    );
    let action = Action::ChangeSignatureWarnings {
        change: change.to_owned(),
    };
    project
        .request(
            Path::new("base"),
            Vec::new(),
            action,
            SIGNATURE_QUERY_TIMEOUT,
        )
        .map(|reply| reply.signature_warnings)
        .unwrap_or_default()
}

/// Checks whether open or recently landed Rosen changes alter signatures touched by work `texts` (#542).
pub fn dispatch_signature_warnings(
    executable: &Path,
    app_data: &Path,
    project: &str,
    endpoint: &str,
    texts: &[String],
    exclude_change: Option<&str>,
) -> Vec<StaleReferenceWarning> {
    if !crate::usage::enabled(app_data, project) {
        return Vec::new();
    }
    let project = Project::new(
        crate::usage::folder(app_data, project),
        executable.into(),
        Some(Endpoint::new(endpoint)),
    );
    let action = Action::DispatchSignatureWarnings {
        texts: texts.to_vec(),
        exclude_change: exclude_change.map(str::to_owned),
    };
    project
        .request(
            Path::new("base"),
            Vec::new(),
            action,
            SIGNATURE_QUERY_TIMEOUT,
        )
        .map(|reply| reply.signature_warnings)
        .unwrap_or_default()
}
