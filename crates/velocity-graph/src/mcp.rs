//! The `graph` MCP server every agent gets (F19 "Agent tools"): Velocity's own executable
//! started as `velocity --graph-mcp <app-data> <project-id> <worktree>`, serving one agent on
//! stdin/stdout.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};
use rosen_client::Endpoint;
use rosen_client::snapshot::WorkspaceMarker;
use serde_json::{Value, json};
use velocity_rosen::mcp_latency::{LatencyRecorder, duration_ms};

use crate::Error;
use crate::index::Index;
use crate::project_service::{Action, Project, QUERY_TIMEOUT, Reply};
use crate::status::WorkspaceStatus;
use crate::tools::{CallError, Tools};
use crate::usage::record_tool;

/// The switch that makes Velocity's executable serve the graph.
pub const SERVER_SWITCH: &str = "--graph-mcp";
/// The switch that makes Velocity's executable fill a project's parse cache and exit
/// (warming).
pub const WARM_SWITCH: &str = "--graph-warm";

/// The MCP server name agents see.
pub const SERVER_NAME: &str = "velocity-graph";

const USAGE: &str = "Usage: <app-data> <project-id> <worktree>";

/// Folders whose changes never matter to the graph: build output and dependencies, which git
/// ignores anyway. Skipping them here saves a listing per build.
const NOISE: [&str; 10] = [
    ".git",
    ".rosen",
    "target",
    "node_modules",
    ".svelte-kit",
    "dist",
    ".lake",
    "__pycache__",
    ".venv",
    ".pnpm-store",
];

/// Shared server guidance published once in the session preface rather than in every MCP
/// tool description (#538). Stated as facts, not orders (Graphify #2202).
pub const INSTRUCTIONS: &str = "The velocity-graph tools map the workspace's code and spec. graph_find locates symbols, files, spec sections and §7 items by name, alias or description. graph_read on a file gives a symbol outline with line ranges; graph_read on a symbol or spec alias (§7, §3.1, F16) gives its links and, with source: true, its lines. graph_spec returns the split spec's heading tree with addresses. graph_impact finds dependents, graph_run chains lookups, and graph_grep / graph_glob search text and files respecting gitignore. Inferred, ambiguous and unconfirmed links are leads to verify.";

pub fn server_arguments(app_data: &Path, project: &str, worktree: &Path) -> Vec<String> {
    vec![
        app_data.display().to_string(),
        project.to_owned(),
        worktree.display().to_string(),
    ]
}

/// Serves one agent from [`server_arguments`] on stdin/stdout until input ends.
pub fn serve_stdio(arguments: &[OsString]) -> Result<(), Error> {
    let [app_data, project, worktree, optional_thread @ ..] = arguments else {
        return Err(Error::Invalid(format!("{USAGE} [thread]")));
    };
    let thread = match optional_thread {
        [] => None,
        [thread] => Some(thread.to_string_lossy().into_owned()),
        _ => return Err(Error::Invalid(format!("{USAGE} [thread]"))),
    };
    let project = project
        .to_str()
        .ok_or_else(|| Error::Invalid("Invalid project identity".into()))?;
    let app_data = Path::new(app_data);
    let cache = crate::usage::folder(app_data, project);
    let endpoint = project_endpoint(app_data, project);
    let backend = Backend::Shared(Project::new(
        cache.clone(),
        std::env::current_exe()?,
        endpoint,
    ));
    serve_connection(
        PathBuf::from(worktree),
        cache,
        Some(app_data.to_path_buf()),
        backend,
        thread,
        std::io::BufReader::new(std::io::stdin()),
        std::io::stdout(),
    )
}

fn project_endpoint(app_data: &Path, project: &str) -> Option<Endpoint> {
    std::env::var("ROSEN_ENDPOINT")
        .ok()
        .map(Endpoint::new)
        .or_else(|| {
            Some(
                velocity_rosen::ProjectServer::new(app_data, project.parse().ok()?)
                    .endpoint()
                    .clone(),
            )
        })
}

/// Session-start workspace map (workspace path, file/symbol counts, biggest subsystems and
/// most-used symbols), fetched once for the session preface (#538).
pub fn workspace_map(
    executable: &Path,
    app_data: &Path,
    project: &str,
    workspace: &Path,
) -> Option<String> {
    let cache = crate::usage::folder(app_data, project);
    let endpoint = project_endpoint(app_data, project);
    let project = Project::new(cache, executable.into(), endpoint);
    let reply = project
        .request(workspace, Vec::new(), Action::Map, MAP_WAIT)
        .ok()?;
    (!reply.text.is_empty()).then_some(reply.text)
}

/// First-prompt context uses the same index, ranking and project cache as graph_find.
pub fn task_context(
    executable: &Path,
    app_data: &Path,
    project: &str,
    workspace: &Path,
    title: &str,
) -> Option<String> {
    let cache = crate::usage::folder(app_data, project);
    let endpoint = project_endpoint(app_data, project);
    let project = Project::new(cache, executable.into(), endpoint);
    let reply = project
        .request(workspace, Vec::new(), Action::Task(title.into()), MAP_WAIT)
        .ok()?;
    (!reply.text.is_empty()).then_some(reply.text)
}

/// How long session startup waits for the workspace map or initial task matches.
const MAP_WAIT: Duration = Duration::from_millis(1500);
const MAX_PENDING_PATH_BYTES: usize = 32 * 1024;

struct UsageRecord {
    name: String,
    outcome: crate::usage::ToolOutcome,
}

/// One agent's connection.
struct Connection {
    backend: Backend,
    root: PathBuf,
    cache: PathBuf,
    dirty: Mutex<HashSet<String>>,
    request: Mutex<()>,
    status: WorkspaceStatus,
    latency: LatencyRecorder,
    usage_tx: Option<mpsc::SyncSender<UsageRecord>>,
}

enum Backend {
    Local {
        tools: Box<Mutex<Option<Tools>>>,
        endpoint: Option<Endpoint>,
    },
    Shared(Project),
}

/// Serves one connection. The index is built in the background from the start, so the first
/// tool call usually finds it ready.
pub fn serve(
    root: PathBuf,
    cache: PathBuf,
    input: impl BufRead,
    output: impl Write,
) -> Result<(), Error> {
    let endpoint = std::env::var("ROSEN_ENDPOINT").ok().map(Endpoint::new);
    let backend = Backend::Local {
        tools: Box::default(),
        endpoint,
    };
    serve_connection(root, cache, None, backend, None, input, output)
}

fn serve_connection(
    root: PathBuf,
    cache: PathBuf,
    app_data: Option<PathBuf>,
    backend: Backend,
    thread: Option<String>,
    input: impl BufRead,
    mut output: impl Write,
) -> Result<(), Error> {
    let status = WorkspaceStatus::track(&cache, &root);
    let latency = LatencyRecorder::open(SERVER_NAME, app_data.as_deref());
    // Offload usage.json lock/rewrite so tool calls reply immediately (#541).
    let (usage_tx, usage_rx) = mpsc::sync_channel::<UsageRecord>(128);
    let usage_writer = {
        let cache = cache.clone();
        let root = root.clone();
        std::thread::spawn(move || {
            while let Ok(entry) = usage_rx.recv() {
                record_tool(&cache, &root, thread.as_deref(), &entry.name, entry.outcome);
            }
        })
    };
    let connection = Arc::new(Connection {
        backend,
        root,
        cache,
        dirty: Mutex::default(),
        request: Mutex::default(),
        status,
        latency,
        usage_tx: Some(usage_tx),
    });
    let (changes, pending) = mpsc::sync_channel(1);
    let watcher = watch(&connection, changes);
    let builder = {
        let connection = connection.clone();
        std::thread::spawn(move || {
            connection.catch_up();
        })
    };
    let updater = {
        let connection = connection.clone();
        std::thread::spawn(move || catch_up(&connection, pending))
    };
    let result = serve_requests(input, &mut output, &connection);
    drop(watcher);
    let _ = updater.join();
    // A connection can end while building; let its parses reach the cache.
    let _ = builder.join();
    drop(connection);
    let _ = usage_writer.join();
    result
}

fn serve_requests(
    input: impl BufRead,
    output: &mut impl Write,
    connection: &Connection,
) -> Result<(), Error> {
    for line in input.lines() {
        let line = line?;
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => respond(&request, connection),
            Err(_) => Some(failure(Value::Null, -32700, "Parse error")),
        };
        if let Some(response) = response {
            writeln!(output, "{response}")?;
            output.flush()?;
        }
    }
    Ok(())
}

/// Fills the project's parse cache from `worktree` in the background (F19 "Warming"): the
/// executable runs with [`WARM_SWITCH`], below normal priority, and exits once the index is
/// built. It holds no watcher, so a thread can take the ready worktree (a folder rename)
/// while it runs.
pub fn warm(
    executable: &Path,
    app_data: &Path,
    project: &str,
    worktree: &Path,
) -> std::io::Result<std::process::Child> {
    let mut command = crate::files::background_command(executable);
    command
        .arg(WARM_SWITCH)
        .args(server_arguments(app_data, project, worktree))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
}

/// The warming run from [`warm`]'s arguments: builds the index, which fills the cache.
pub fn warm_stdio(arguments: &[OsString]) -> Result<(), Error> {
    let [app_data, project, worktree] = arguments else {
        return Err(Error::Invalid(USAGE.into()));
    };
    let project = project
        .to_str()
        .ok_or_else(|| Error::Invalid("Invalid project identity".into()))?;
    let cache = PathBuf::from(app_data).join("graph").join(project);
    Index::open(Path::new(worktree), &cache).map(drop)
}

/// Builds the index. A panic in a parser becomes an error the tools report, rather than the
/// end of the agent's graph server.
fn build(root: &Path, cache: &Path, endpoint: Option<Endpoint>) -> Result<Tools, String> {
    match std::panic::catch_unwind(|| Index::open_workspace(root, cache, endpoint)) {
        Ok(Ok(index)) => Ok(Tools::new(index)),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("the indexer stopped on an internal error".to_owned()),
    }
}

fn ensure_tools<'tools>(
    connection: &Connection,
    tools: &'tools mut Option<Tools>,
    endpoint: Option<Endpoint>,
) -> Result<&'tools mut Tools, CallError> {
    if tools.is_none() {
        *tools = Some(
            build(&connection.root, &connection.cache, endpoint)
                .map_err(|error| format!("the code graph could not be built: {error}"))?,
        );
        let dirty = connection
            .dirty
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        connection.status.mark(dirty.is_empty());
    }
    tools
        .as_mut()
        .ok_or_else(|| "the code graph is unavailable".into())
}

fn watch(
    connection: &Arc<Connection>,
    changes: mpsc::SyncSender<()>,
) -> Option<notify::RecommendedWatcher> {
    let connection = connection.clone();
    let rosen_workspace = WorkspaceMarker::path(&connection.root).is_file();
    let (root, mode) = if rosen_workspace {
        (connection.root.join(".rosen"), RecursiveMode::NonRecursive)
    } else {
        (connection.root.clone(), RecursiveMode::Recursive)
    };
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        if event.kind.is_access() {
            return;
        }
        let mut dirty = connection
            .dirty
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let was_clean = dirty.is_empty();
        mark_changed(&mut dirty, &connection.root, rosen_workspace, event);
        if was_clean && !dirty.is_empty() {
            connection.status.mark(false);
            let _ = changes.try_send(());
        }
    })
    .ok()?;
    watcher.watch(&root, mode).ok()?;
    Some(watcher)
}

fn mark_changed(dirty: &mut HashSet<String>, root: &Path, rosen: bool, event: notify::Event) {
    if event.need_rescan() {
        dirty.clear();
        dirty.insert(String::new());
        return;
    }
    for path in event.paths {
        if dirty.contains("") {
            break;
        }
        if rosen {
            if path
                .file_name()
                .is_some_and(|name| name == "workspace.json")
            {
                dirty.insert(String::from(".rosen/workspace.json"));
            }
            continue;
        }
        let Ok(relative) = path.strip_prefix(root) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        if !relative.split('/').any(|part| NOISE.contains(&part)) {
            dirty.insert(relative);
        }
    }
    if dirty.iter().map(|path| path.len() + 1).sum::<usize>() > MAX_PENDING_PATH_BYTES {
        dirty.clear();
        dirty.insert(String::new());
    }
}

fn catch_up(connection: &Connection, pending: mpsc::Receiver<()>) {
    while pending.recv().is_ok() {
        // Group events from one save or build. A call still refreshes its own view immediately.
        std::thread::sleep(Duration::from_millis(150));
        connection.catch_up();
    }
}

fn respond(request: &Value, connection: &Connection) -> Option<Value> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request.get("method").and_then(Value::as_str);
    if request.get("jsonrpc").and_then(Value::as_str) != Some("2.0") || method.is_none() {
        return Some(failure(id, -32600, "Invalid request"));
    }
    // Notifications get no response.
    request.get("id")?;
    let result = match method.unwrap_or_default() {
        // Shared guidance and the workspace map are published once in the session preface
        // instead of MCP initialize instructions, which some harnesses copy onto every tool
        // description (#538).
        "initialize" => {
            connection
                .latency
                .record("initialize", Duration::ZERO, Duration::ZERO, true);
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "velocity-graph", "version": env!("CARGO_PKG_VERSION")},
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({"tools": tool_list()}),
        "tools/call" => call(&request["params"], connection),
        _ => return Some(failure(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

struct CallTiming {
    connect_ms: u64,
    work_ms: u64,
}

fn call(parameters: &Value, connection: &Connection) -> Value {
    let name = parameters["name"].as_str().unwrap_or_default();
    let started = Instant::now();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        connection.call(name, &parameters["arguments"])
    }))
    .unwrap_or_else(|_| Err("the code graph failed on this request".into()));
    let total_ms = duration_ms(started.elapsed());
    let (text, outcome, failed, timing) = match result {
        Ok((text, outcome, timing)) => (text, outcome, false, timing),
        Err(CallError { message, outcome }) => (
            message,
            outcome,
            true,
            CallTiming {
                connect_ms: 0,
                work_ms: total_ms,
            },
        ),
    };
    connection
        .latency
        .record_split(name, timing.connect_ms, timing.work_ms, total_ms, !failed);
    if let Some(usage_tx) = &connection.usage_tx {
        let _ = usage_tx.try_send(UsageRecord {
            name: name.to_owned(),
            outcome,
        });
    }
    let text = crate::query::within_budget(text);
    if failed {
        json!({"isError": true, "content": [{"type": "text", "text": text}]})
    } else {
        json!({"content": [{"type": "text", "text": text}]})
    }
}

impl Connection {
    fn catch_up(&self) {
        match &self.backend {
            Backend::Local { tools, endpoint } => {
                let mut tools = tools.lock().unwrap_or_else(|error| error.into_inner());
                if let Ok(tools) = ensure_tools(self, &mut tools, endpoint.clone()) {
                    let _ = refresh(tools, self);
                }
            }
            Backend::Shared(project) => {
                let _ = self.request(project, Action::Refresh);
            }
        }
    }

    fn call(
        &self,
        name: &str,
        arguments: &Value,
    ) -> Result<(String, crate::usage::ToolOutcome, CallTiming), CallError> {
        let started = Instant::now();
        if let Some(text) = self.call_workspace_search(name, arguments)? {
            return Ok((
                text,
                crate::usage::ToolOutcome::Other,
                CallTiming {
                    connect_ms: 0,
                    work_ms: duration_ms(started.elapsed()),
                },
            ));
        }
        match &self.backend {
            Backend::Local { tools, endpoint } => {
                self.call_local(tools, endpoint.clone(), name, arguments, started)
            }
            Backend::Shared(project) => self.call_shared(project, name, arguments),
        }
    }

    fn call_workspace_search(
        &self,
        name: &str,
        arguments: &Value,
    ) -> Result<Option<String>, CallError> {
        match name {
            "graph_grep" => Ok(Some(crate::workspace_search::grep(&self.root, arguments)?)),
            "graph_glob" => Ok(Some(crate::workspace_search::glob_files(
                &self.root, arguments,
            )?)),
            _ => Ok(None),
        }
    }

    fn call_local(
        &self,
        tools: &Mutex<Option<Tools>>,
        endpoint: Option<Endpoint>,
        name: &str,
        arguments: &Value,
        started: Instant,
    ) -> Result<(String, crate::usage::ToolOutcome, CallTiming), CallError> {
        crate::rosen::reset_connect_duration();
        let mut tools = tools.lock().unwrap_or_else(|error| error.into_inner());
        let tools = ensure_tools(self, &mut tools, endpoint)?;
        refresh(tools, self)?;
        let (text, outcome) = tools.measured_call(name, arguments)?;
        let connect_ms = duration_ms(crate::rosen::take_connect_duration());
        let total_ms = duration_ms(started.elapsed());
        Ok((
            text,
            outcome,
            CallTiming {
                connect_ms,
                work_ms: total_ms.saturating_sub(connect_ms),
            },
        ))
    }

    fn call_shared(
        &self,
        project: &Project,
        name: &str,
        arguments: &Value,
    ) -> Result<(String, crate::usage::ToolOutcome, CallTiming), CallError> {
        let action = Action::Tool {
            name: name.into(),
            arguments: arguments.clone(),
        };
        let (reply, pipe_connect) = self.request(project, action)?;
        let connect_ms = duration_ms(pipe_connect).saturating_add(reply.rosen_connect_ms);
        if reply.failed {
            return Err(CallError {
                message: reply.text,
                outcome: reply.outcome,
            });
        }
        Ok((
            reply.text,
            reply.outcome,
            CallTiming {
                connect_ms,
                work_ms: reply.work_ms,
            },
        ))
    }

    fn request(&self, project: &Project, action: Action) -> Result<(Reply, Duration), CallError> {
        let _request = self
            .request
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let changed: Vec<_> = self
            .dirty
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .drain()
            .collect();
        let result = project.request_timed(&self.root, changed.clone(), action, QUERY_TIMEOUT);
        let mut dirty = self.dirty.lock().unwrap_or_else(|error| error.into_inner());
        if result.is_err() {
            dirty.extend(changed);
        }
        self.status.mark(result.is_ok() && dirty.is_empty());
        result.map_err(Into::into)
    }
}

fn refresh(tools: &mut Tools, connection: &Connection) -> Result<(), CallError> {
    let changed: Vec<String> = connection
        .dirty
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .drain()
        .collect();
    if changed.is_empty() {
        return Ok(());
    }
    if let Err(error) = tools.refresh(&changed) {
        connection
            .dirty
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .extend(changed);
        return Err(error.to_string().into());
    }
    let dirty = connection
        .dirty
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    connection.status.mark(dirty.is_empty());
    Ok(())
}

fn failure(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn tool_list() -> Vec<Value> {
    vec![
        tool(
            "graph_overlaps",
            "Live Rosen path overlaps, edited symbols in each change, and stale-reference warnings when an altered public signature, IPC command, or event type is referenced by another open or recently landed change. Empty paths checks this workspace's claims and edits.",
            json!({"paths": {"type": "array", "items": {"type": "string"}}}),
            &[],
        ),
        tool(
            "graph_find",
            "Rank symbols, files and subsystems for a name or a plain-language description. Each result: address, kind, name, file:lines, signature. [exact] marks the only record with exactly that name.",
            json!({
                "query": {"type": "string", "description": "A name (Channel::open, plan_deletion) or words (\"approval record merge gate\")"},
                // A range, not an integer enum: Gemini accepts `enum` only on strings and
                // rejects the agent's whole request when a tool has one (Antigravity).
                "plane": {"type": "integer", "minimum": 1, "maximum": 3, "description": "1 symbols, 2 files, 3 subsystems"},
                "kind": {"type": "string", "description": "fn, method, type, trait, alias, mod, macro, const, section, item, file or subsystem"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 30},
            }),
            &["query"],
        ),
        tool(
            "graph_read",
            "A file path or file address gives its symbol outline with line ranges. A symbol name, spec alias (§7, §3.1, F16) or address gives its signature, docs, location and links with provenance. Use source: true for source lines. Reports ambiguous, missing and stale lookups.",
            json!({
                "target": {"type": "string", "description": "An address like 1.k3f9q, a name like Channel::open, a spec alias like §7, §3.1 or F16, or a path"},
                "source": {"type": "boolean", "description": "Add the record's source lines, so no separate file read is needed"},
            }),
            &["target"],
        ),
        tool(
            "graph_spec",
            "Return the split spec's heading tree with addresses, section aliases and §7 item status in one compact call. Pass section (e.g. §7, §3.1, F5) or depth to expand a subtree.",
            json!({
                "section": {"type": "string", "description": "Optional section alias, heading name, address or path to expand (e.g. §7, §3.1, F5)"},
                "depth": {"type": "integer", "minimum": 1, "maximum": 6, "description": "Maximum heading depth to show (default 3 when large)"},
                "open_only": {"type": "boolean", "description": "When expanding §7, list only open items"},
            }),
            &[],
        ),
        tool(
            "graph_grep",
            "Search workspace files with ripgrep, respecting gitignore and capping output.",
            json!({
                "pattern": {"type": "string", "description": "Regular expression, or literal text when fixed is true"},
                "path": {"type": "string", "description": "Optional repository-relative file or folder to narrow the search"},
                "glob": {"type": "string", "description": "Optional ripgrep file glob such as *.rs or !tests/**"},
                "case_insensitive": {"type": "boolean", "description": "Ignore letter case"},
                "fixed": {"type": "boolean", "description": "Match pattern as a literal string rather than a regex"},
                "files_only": {"type": "boolean", "description": "List only matching file paths"},
                "context": {"type": "integer", "minimum": 0, "maximum": 5, "description": "Lines of context around each match"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 200, "description": "Maximum output lines (default 50)"},
            }),
            &["pattern"],
        ),
        tool(
            "graph_glob",
            "List workspace files matching a glob pattern, respecting gitignore and capping output.",
            json!({
                "pattern": {"type": "string", "description": "Glob pattern such as **/*.rs, crates/velocity-graph/** or *.{ts,svelte}"},
                "path": {"type": "string", "description": "Optional repository-relative folder to search under"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 200, "description": "Maximum file paths to return (default 100)"},
            }),
            &["pattern"],
        ),
        tool(
            "graph_impact",
            "What depends on symbols or files, followed transitively through calls, implementations, type uses, imports and doc mentions, grouped by subsystem with depth. A type brings its methods, and a trait method implementation brings the callers of the trait method. Links found in the source are listed apart from leads to check. No dependents does not prove code unused: dynamic calls and copies are invisible to the graph, so grep a distinctive literal before deleting.",
            json!({
                "targets": {"oneOf": [{"type": "string"}, {"type": "array", "items": {"type": "string"}}], "description": "Addresses, names or paths"},
                "depth": {"type": "integer", "minimum": 1, "maximum": 6, "description": "Hops to follow (default 2)"},
            }),
            &["targets"],
        ),
        tool(
            "graph_run",
            "Several hops in one call: up to 16 steps separated by `;`, over one register, with a one-line audit per step. Steps: FIND \"text\" [limit]; AT <address|name|path>; UP; DOWN; FOLLOW <calls|implements|references|mentions|imports|depends|*> [in|out] [all]; FILTER kind=<k> | plane=<n> | path=<glob> | name=<text> (!= negates); PATH <target>; COUNT; SOURCE. FOLLOW skips ambiguous links unless `all`. Example: AT Channel::open; FOLLOW calls in; FILTER path=src-tauri/**; COUNT",
            json!({"program": {"type": "string"}}),
            &["program"],
        ),
    ]
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str]) -> Value {
    let mut tool = json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": properties, "required": required},
    });
    // Claude Code keeps these discovery tools in context instead of deferring them to search.
    if matches!(
        name,
        "graph_find"
            | "graph_read"
            | "graph_spec"
            | "graph_impact"
            | "graph_run"
            | "graph_grep"
            | "graph_glob"
    ) {
        tool["_meta"] = json!({"anthropic/alwaysLoad": true});
    }
    tool
}
