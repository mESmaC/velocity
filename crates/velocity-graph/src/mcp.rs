//! The `graph` MCP server every agent gets (F19 "Agent tools"): Velocity's own executable
//! started as `velocity --graph-mcp <app-data> <project-id> <worktree>`, serving one agent on
//! stdin/stdout.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};
use rosen_client::Endpoint;
use rosen_client::snapshot::WorkspaceMarker;
use serde_json::{Value, json};

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

/// What every agent is told at connection. Stated as facts, not orders: imperative "MUST"
/// rules read like prompt injection and get ignored (Graphify #2202).
const INSTRUCTIONS: &str = "The velocity-graph tools map the workspace's code. graph_find locates symbols by name or description. graph_read on a file gives a symbol outline with line ranges; graph_read on a symbol gives its links and, with source: true, its code. graph_impact finds dependents, and graph_run chains lookups. Inferred, ambiguous and unconfirmed links are leads to verify. Grep finds exact text such as literals and config values.";

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

/// How long `initialize` waits for the index, so the session-start map can go out with the
/// instructions. With a warm cache a build takes a fraction of this.
const MAP_WAIT: Duration = Duration::from_millis(1500);
const MAX_PENDING_PATH_BYTES: usize = 32 * 1024;

/// One agent's connection.
struct Connection {
    backend: Backend,
    root: PathBuf,
    cache: PathBuf,
    thread: Option<String>,
    dirty: Mutex<HashSet<String>>,
    request: Mutex<()>,
    status: WorkspaceStatus,
    /// Signalled once the background build finished; taken by the first `initialize`.
    built: Mutex<Option<mpsc::Receiver<()>>>,
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
    serve_connection(root, cache, backend, None, input, output)
}

fn serve_connection(
    root: PathBuf,
    cache: PathBuf,
    backend: Backend,
    thread: Option<String>,
    input: impl BufRead,
    mut output: impl Write,
) -> Result<(), Error> {
    let (signal, built) = mpsc::channel();
    let status = WorkspaceStatus::track(&cache, &root);
    let connection = Arc::new(Connection {
        backend,
        root,
        cache,
        thread,
        dirty: Mutex::default(),
        request: Mutex::default(),
        status,
        built: Mutex::new(Some(built)),
    });
    let (changes, pending) = mpsc::sync_channel(1);
    let watcher = watch(&connection, changes);
    let builder = {
        let connection = connection.clone();
        std::thread::spawn(move || {
            connection.catch_up();
            let _ = signal.send(());
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
        "initialize" => json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "velocity-graph", "version": env!("CARGO_PKG_VERSION")},
            "instructions": instructions(connection),
        }),
        "ping" => json!({}),
        "tools/list" => json!({"tools": tool_list()}),
        "tools/call" => call(&request["params"], connection),
        _ => return Some(failure(id, -32601, "Method not found")),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

/// The instructions, with the session-start map when the index is ready in time.
fn instructions(connection: &Connection) -> String {
    let built = connection
        .built
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .take();
    if let Some(built) = built {
        let _ = built.recv_timeout(MAP_WAIT);
    }
    let map = match &connection.backend {
        Backend::Local { tools, .. } => tools
            .try_lock()
            .ok()
            .and_then(|tools| tools.as_ref().map(Tools::map))
            .unwrap_or_default(),
        Backend::Shared(project) => project
            .request(&connection.root, Vec::new(), Action::Map, MAP_WAIT)
            .map(|reply| reply.text)
            .unwrap_or_default(),
    };
    if map.is_empty() {
        INSTRUCTIONS.to_owned()
    } else {
        format!("{INSTRUCTIONS}\n\n{map}")
    }
}

fn call(parameters: &Value, connection: &Connection) -> Value {
    let name = parameters["name"].as_str().unwrap_or_default();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        connection.call(name, &parameters["arguments"])
    }))
    .unwrap_or_else(|_| Err("the code graph failed on this request".into()));
    let (text, outcome, failed) = match result {
        Ok((text, outcome)) => (text, outcome, false),
        Err(CallError { message, outcome }) => (message, outcome, true),
    };
    record_tool(
        &connection.cache,
        &connection.root,
        connection.thread.as_deref(),
        name,
        outcome,
    );
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
    ) -> Result<(String, crate::usage::ToolOutcome), CallError> {
        match &self.backend {
            Backend::Local { tools, endpoint } => {
                let mut tools = tools.lock().unwrap_or_else(|error| error.into_inner());
                let tools = ensure_tools(self, &mut tools, endpoint.clone())?;
                refresh(tools, self)?;
                tools.measured_call(name, arguments)
            }
            Backend::Shared(project) => {
                let action = Action::Tool {
                    name: name.into(),
                    arguments: arguments.clone(),
                };
                let reply = self.request(project, action)?;
                if reply.failed {
                    return Err(CallError {
                        message: reply.text,
                        outcome: reply.outcome,
                    });
                }
                Ok((reply.text, reply.outcome))
            }
        }
    }

    fn request(&self, project: &Project, action: Action) -> Result<Reply, CallError> {
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
        let result = project.request(&self.root, changed.clone(), action, QUERY_TIMEOUT);
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
            "Live Rosen path overlaps, with the edited symbols in each change and any symbols edited by both. Empty paths checks this workspace's claims and edits.",
            json!({"paths": {"type": "array", "items": {"type": "string"}}}),
            &[],
        ),
        tool(
            "graph_find",
            "Rank symbols, files and subsystems for a name or a plain-language description. Each result: address, kind, name, file:lines, signature. [exact] marks the only record with exactly that name.",
            json!({
                "query": {"type": "string", "description": "A name (Channel::open, plan_deletion) or words (\"approval record merge gate\")"},
                "plane": {"type": "integer", "enum": [1, 2, 3], "description": "1 symbols, 2 files, 3 subsystems"},
                "kind": {"type": "string", "description": "fn, method, type, trait, alias, mod, macro, const, section, file or subsystem"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 30},
            }),
            &["query"],
        ),
        tool(
            "graph_read",
            "A file path or file address gives its symbol outline with line ranges. A symbol name or address gives its signature, docs, location and links with provenance. Use source: true on a symbol for its source lines. Reports ambiguous, missing and stale lookups.",
            json!({
                "target": {"type": "string", "description": "An address like 1.k3f9q, a name like Channel::open, or a path"},
                "source": {"type": "boolean", "description": "Add the record's source lines, so no separate file read is needed"},
            }),
            &["target"],
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
        "graph_find" | "graph_read" | "graph_impact" | "graph_run"
    ) {
        tool["_meta"] = json!({"anthropic/alwaysLoad": true});
    }
    tool
}
