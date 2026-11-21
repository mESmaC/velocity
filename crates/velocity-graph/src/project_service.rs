//! Local graph worker. All connections for a project share warm live indexes across
//! concurrent workspaces (#541).

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Mutex, mpsc};
use std::time::{Duration, Instant};

use rosen_client::Endpoint;
use rosen_client::protocol::transport::{self, BoxStream, Listener};
use rosen_client::snapshot::WorkspaceMarker;
use rosen_core::ObjectId;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;
use velocity_rosen::mcp_latency::duration_ms;

use crate::cache::Cache;
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
const IDLE_TIMEOUT: Duration = Duration::from_secs(600);
/// Keep `base` plus several concurrent agent workspaces warm in memory without thrashing (#541).
const MAX_WARM_VIEWS: usize = 6;

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
    #[serde(default)]
    pub rosen_connect_ms: u64,
    #[serde(default)]
    pub work_ms: u64,
}

impl Reply {
    fn plain(text: String) -> Self {
        Self {
            text,
            outcome: ToolOutcome::Other,
            failed: false,
            mentions: Vec::new(),
            signature_warnings: Vec::new(),
            rosen_connect_ms: 0,
            work_ms: 0,
        }
    }
}

struct PersistentClient {
    runtime: tokio::runtime::Runtime,
    stream: Option<BufReader<BoxStream>>,
}

pub(crate) struct Project {
    cache: PathBuf,
    executable: PathBuf,
    address: Endpoint,
    rosen: Option<Endpoint>,
    client: Mutex<Option<PersistentClient>>,
}

impl Project {
    pub fn new(cache: PathBuf, executable: PathBuf, rosen: Option<Endpoint>) -> Self {
        let address = address(&cache);
        Self {
            cache,
            executable,
            address,
            rosen,
            client: Mutex::new(None),
        }
    }

    pub fn request(
        &self,
        workspace: &Path,
        changed: Vec<String>,
        action: Action,
        timeout: Duration,
    ) -> Result<Reply, String> {
        self.request_timed(workspace, changed, action, timeout)
            .map(|(reply, _)| reply)
    }

    /// Sends one request over a persistent pipe connection to `--graph-project`, returning
    /// the reply and time spent connecting to the worker (0 ms when already warm).
    pub fn request_timed(
        &self,
        workspace: &Path,
        changed: Vec<String>,
        action: Action,
        timeout: Duration,
    ) -> Result<(Reply, Duration), String> {
        let request = Request {
            workspace: workspace.to_path_buf(),
            rosen: self.rosen.as_ref().map(ToString::to_string),
            changed,
            action,
        };
        let mut guard = self
            .client
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if guard.is_none() {
            let runtime = runtime().map_err(|error| error.to_string())?;
            *guard = Some(PersistentClient {
                runtime,
                stream: None,
            });
        }
        let Some(client) = guard.as_mut() else {
            return Err("The project graph client is unavailable".into());
        };
        client.runtime.block_on(async {
            tokio::time::timeout(timeout, self.exchange(&mut client.stream, &request))
                .await
                .map_err(|_| "The project graph is busy".to_owned())?
        })
    }

    async fn exchange(
        &self,
        cached: &mut Option<BufReader<BoxStream>>,
        request: &Request,
    ) -> Result<(Reply, Duration), String> {
        let mut connect_elapsed = Duration::ZERO;
        // Try the already-open stream first; on EOF/broken pipe reconnect once transparently.
        for attempt in 0..2 {
            let reused = cached.is_some();
            if cached.is_none() {
                let started = Instant::now();
                let stream = self.connect().await.map_err(|error| error.to_string())?;
                connect_elapsed = connect_elapsed.saturating_add(started.elapsed());
                *cached = Some(BufReader::new(stream));
            }
            let Some(reader) = cached.as_mut() else {
                return Err("Project graph connection missing".into());
            };
            match send_and_receive(reader, request).await {
                Ok(reply) => return reply.map(|reply| (reply, connect_elapsed)),
                Err(error) => {
                    *cached = None;
                    if !reused || attempt == 1 {
                        return Err(error.to_string());
                    }
                }
            }
        }
        Err("Project graph connection closed".into())
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

async fn send_and_receive(
    reader: &mut BufReader<BoxStream>,
    request: &Request,
) -> io::Result<Result<Reply, String>> {
    write(reader.get_mut(), request).await?;
    read_buffered::<Result<Reply, String>>(reader).await
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

async fn read_buffered<T: serde::de::DeserializeOwned>(
    reader: &mut BufReader<BoxStream>,
) -> io::Result<T> {
    let mut bytes = Vec::new();
    let count = reader
        .take(FRAME_LIMIT)
        .read_until(b'\n', &mut bytes)
        .await?;
    if count == 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "Project graph connection closed",
        ));
    }
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
    stream.write_all(&bytes).await?;
    stream.flush().await
}

type WorkItem = (Request, oneshot::Sender<Result<Reply, String>>);

/// Requests are handled serially on the main thread (outside Tokio, so Rosen requests never
/// nest runtimes), while a Tokio acceptor thread serves persistent client connections (#541).
pub fn serve_project(arguments: &[OsString]) -> Result<(), Error> {
    let [cache] = arguments else {
        return Err(Error::Invalid(
            "Usage: --graph-project <project-cache>".into(),
        ));
    };
    let cache = Path::new(cache);
    let endpoint = address(cache);
    let (bound_tx, bound_rx) = mpsc::sync_channel::<io::Result<()>>(1);
    let (work_tx, work_rx) = mpsc::channel::<WorkItem>();
    let _acceptor = std::thread::Builder::new()
        .name("graph-project-accept".into())
        .spawn(move || run_acceptor(endpoint, bound_tx, work_tx));
    match bound_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(error)) if error.kind() == io::ErrorKind::AddrInUse => return Ok(()),
        Ok(Err(error)) => return Err(error.into()),
        Err(_) => return Err(Error::Invalid("Project graph listener stopped".into())),
    }
    let mut graph = ProjectGraph::default();
    while let Ok((request, reply_tx)) = work_rx.recv_timeout(IDLE_TIMEOUT) {
        let response = graph.handle(cache, request);
        let _ = reply_tx.send(response);
    }
    Ok(())
}

fn run_acceptor(
    endpoint: Endpoint,
    bound: mpsc::SyncSender<io::Result<()>>,
    work: mpsc::Sender<WorkItem>,
) {
    let runtime = match runtime() {
        Ok(runtime) => runtime,
        Err(error) => {
            let _ = bound.send(Err(error));
            return;
        }
    };
    runtime.block_on(async move {
        let mut listener = match Listener::bind(&endpoint) {
            Ok(listener) => {
                let _ = bound.send(Ok(()));
                listener
            }
            Err(error) => {
                let _ = bound.send(Err(error));
                return;
            }
        };
        while let Ok(stream) = listener.accept().await {
            let work = work.clone();
            tokio::spawn(serve_client(BufReader::new(stream), work));
        }
    });
}

async fn serve_client(mut reader: BufReader<BoxStream>, work: mpsc::Sender<WorkItem>) {
    loop {
        let Ok(request) = read_buffered::<Request>(&mut reader).await else {
            break;
        };
        let (reply_tx, reply_rx) = oneshot::channel();
        if work.send((request, reply_tx)).is_err() {
            break;
        }
        let Ok(response) = reply_rx.await else {
            break;
        };
        let written = tokio::time::timeout(IO_TIMEOUT, write(reader.get_mut(), &response)).await;
        if !matches!(written, Ok(Ok(()))) {
            break;
        }
    }
}

#[derive(Clone, PartialEq, Eq)]
enum ViewKey {
    Base,
    Workspace(PathBuf),
}

struct WarmView {
    key: ViewKey,
    tree: ObjectId,
    tools: Tools,
}

#[derive(Default)]
struct ProjectGraph {
    endpoint: Option<String>,
    cache: Option<Cache>,
    rosen_views: Vec<WarmView>,
    git_view: Option<Tools>,
    mention_refresh: Option<(PathBuf, Instant)>,
}

impl ProjectGraph {
    fn handle(&mut self, cache: &Path, request: Request) -> Result<Reply, String> {
        crate::rosen::reset_connect_duration();
        let started = Instant::now();
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.run(cache, request)))
                .unwrap_or_else(|_| {
                    Err(Error::Invalid(
                        "The project graph stopped on an internal error".into(),
                    ))
                });
        let connect_ms = duration_ms(crate::rosen::take_connect_duration());
        let total_ms = duration_ms(started.elapsed());
        match result {
            Ok(mut reply) => {
                reply.rosen_connect_ms = connect_ms;
                reply.work_ms = total_ms.saturating_sub(connect_ms);
                Ok(reply)
            }
            Err(error) => {
                self.git_view = None;
                Err(error.to_string())
            }
        }
    }

    fn run(&mut self, cache: &Path, request: Request) -> Result<Reply, Error> {
        let (tools, is_rosen) = self.select(cache, &request)?;
        Ok(match request.action {
            Action::Refresh | Action::WarmBase => Reply::plain(String::new()),
            Action::Map => Reply::plain(tools.map()),
            Action::Task(title) => Reply::plain(tools.task_matches(&title).unwrap_or_default()),
            Action::Tool { name, arguments } => run_tool(tools, is_rosen, &name, &arguments),
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

    fn shared_cache(&mut self, folder: &Path) -> Cache {
        self.cache
            .get_or_insert_with(|| Cache::open(folder))
            .clone()
    }

    fn select(&mut self, cache: &Path, request: &Request) -> Result<(&mut Tools, bool), Error> {
        let mentions = matches!(request.action, Action::Mentions(_));
        let signature_query = matches!(
            request.action,
            Action::ChangeSignatureWarnings { .. } | Action::DispatchSignatureWarnings { .. }
        );
        let base = matches!(request.action, Action::WarmBase)
            || (mentions || signature_query) && request.rosen.is_some();
        let rosen = base || WorkspaceMarker::path(&request.workspace).is_file();
        if rosen {
            let address = request
                .rosen
                .as_deref()
                .ok_or_else(|| Error::Invalid("Missing project Rosen endpoint".into()))?;
            let tools = self.select_rosen(cache, address, &request.workspace, base)?;
            return Ok((tools, true));
        }
        let tools = self.select_git(cache, request, mentions)?;
        Ok((tools, false))
    }

    fn select_rosen(
        &mut self,
        cache: &Path,
        address: &str,
        workspace: &Path,
        base: bool,
    ) -> Result<&mut Tools, Error> {
        if self.endpoint.as_deref() != Some(address) {
            self.rosen_views.clear();
            self.endpoint = Some(address.to_owned());
        }
        let target_tree = if base {
            crate::rosen::Source::new(Endpoint::new(address), false)?.revision_tree("base")?
        } else {
            WorkspaceMarker::read(workspace)?.tree
        };
        let key = if base {
            ViewKey::Base
        } else {
            ViewKey::Workspace(workspace.to_path_buf())
        };
        let root = if base { Path::new("base") } else { workspace };
        if self.reuse_warm_view(&key, root, target_tree, base)? {
            return Ok(&mut self.rosen_views[0].tools);
        }
        let index = self.build_rosen_index(cache, address, root, target_tree, base)?;
        self.insert_view(WarmView {
            key,
            tree: target_tree,
            tools: Tools::new(index),
        });
        Ok(&mut self.rosen_views[0].tools)
    }

    /// Reuses an exact `(key, tree)` view, another view at the exact same immutable tree,
    /// or refreshes the same workspace's existing view incrementally in place (#541).
    fn reuse_warm_view(
        &mut self,
        key: &ViewKey,
        root: &Path,
        target_tree: ObjectId,
        base: bool,
    ) -> Result<bool, Error> {
        if let Some(position) = self
            .rosen_views
            .iter()
            .position(|view| view.key == *key && view.tree == target_tree)
        {
            self.promote_view(position);
            return Ok(true);
        }
        if let Some(position) = self
            .rosen_views
            .iter()
            .position(|view| view.tree == target_tree)
        {
            self.promote_view(position);
            self.rosen_views[0].tools.set_rosen_root(root, !base, true);
            return Ok(true);
        }
        if let Some(position) = self.rosen_views.iter().position(|view| view.key == *key) {
            self.promote_view(position);
            let view = &mut self.rosen_views[0];
            view.tools.set_rosen_root(root, !base, false);
            view.tools.refresh_to_tree(target_tree)?;
            view.tree = target_tree;
            return Ok(true);
        }
        Ok(false)
    }

    fn build_rosen_index(
        &mut self,
        cache: &Path,
        address: &str,
        root: &Path,
        target_tree: ObjectId,
        base: bool,
    ) -> Result<Index, Error> {
        if let Some(donor) = self
            .rosen_views
            .iter()
            .position(|view| view.key == ViewKey::Base)
            .or_else(|| (!self.rosen_views.is_empty()).then_some(0))
        {
            return self.rosen_views[donor]
                .tools
                .index()
                .branch_to_tree(root, target_tree, !base);
        }
        let shared = self.shared_cache(cache);
        Index::open_rosen_tree_with_cache(
            root,
            cache,
            shared,
            Endpoint::new(address),
            target_tree,
            !base,
        )
    }

    fn promote_view(&mut self, position: usize) {
        if position > 0 {
            let view = self.rosen_views.remove(position);
            self.rosen_views.insert(0, view);
        }
    }

    fn insert_view(&mut self, view: WarmView) {
        self.rosen_views.insert(0, view);
        self.rosen_views.truncate(MAX_WARM_VIEWS);
    }

    fn select_git(
        &mut self,
        cache: &Path,
        request: &Request,
        mentions: bool,
    ) -> Result<&mut Tools, Error> {
        let same_workspace = self
            .git_view
            .as_ref()
            .is_some_and(|tools| tools.index().root() == request.workspace);
        if self.git_view.is_none() {
            let index = Index::open_workspace(&request.workspace, cache, None)?;
            self.git_view = Some(Tools::new(index));
        }
        let tools = self
            .git_view
            .as_mut()
            .ok_or_else(|| Error::Invalid("Project graph unavailable".into()))?;
        tools.select_workspace(&request.workspace)?;
        let refresh = mentions
            && self.mention_refresh.as_ref().is_none_or(|(root, checked)| {
                root != &request.workspace || checked.elapsed() >= MENTION_REFRESH_INTERVAL
            });
        if refresh {
            if same_workspace {
                tools.refresh(&[String::new()])?;
            }
            self.mention_refresh = Some((request.workspace.clone(), Instant::now()));
        } else {
            tools.refresh(&request.changed)?;
        }
        Ok(tools)
    }
}

fn run_tool(tools: &mut Tools, is_rosen: bool, name: &str, arguments: &Value) -> Reply {
    let outcome = if is_rosen {
        tools.measured_call_synced(name, arguments)
    } else {
        tools.measured_call(name, arguments)
    };
    match outcome {
        Ok((text, outcome)) => Reply {
            outcome,
            ..Reply::plain(text)
        },
        Err(error) => Reply {
            outcome: error.outcome,
            failed: true,
            ..Reply::plain(error.message)
        },
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
