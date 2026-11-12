//! The `graph` MCP server every agent gets (F19 "Agent tools"): Velocity's own executable
//! started as `velocity --graph-mcp <app-data> <project-id> <worktree>`, serving one agent on
//! stdin/stdout.

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};
use serde_json::{Value, json};

use crate::Error;
use crate::index::Index;
use crate::tools::Tools;

/// The switch that makes Velocity's executable serve the graph.
pub const SERVER_SWITCH: &str = "--graph-mcp";
/// The MCP server name agents see.
pub const SERVER_NAME: &str = "velocity-graph";

const USAGE: &str = "Usage: <app-data> <project-id> <worktree>";

/// Folders whose changes never matter to the graph: build output and dependencies, which git
/// ignores anyway. Skipping them here saves a listing per build.
const NOISE: [&str; 9] = [
    ".git",
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
const INSTRUCTIONS: &str = "The velocity-graph tools map this worktree's code: symbols, files and subsystems, linked by calls, imports, implementations, type uses and doc mentions. Finding where something lives, what calls it, or what a change affects is what the graph answers, so these questions start there rather than with a grep or a file read: graph_find for a name or a description, graph_read for one record and its links (source: true adds its lines), graph_impact for what depends on a change, and graph_run to chain several hops in one call. Every record gives its file and lines, so a read can be just those lines. Links marked inferred, ambiguous or unconfirmed are leads to check, not facts, and when the graph has nothing on a question it says so rather than guessing. Grep remains the tool for exact text: strings, literals, config values and logs. The graph follows edits as they happen.";

pub fn server_arguments(app_data: &Path, project: &str, worktree: &Path) -> Vec<String> {
    vec![
        app_data.display().to_string(),
        project.to_owned(),
        worktree.display().to_string(),
    ]
}

/// Serves one agent from [`server_arguments`] on stdin/stdout until input ends.
pub fn serve_stdio(arguments: &[OsString]) -> Result<(), Error> {
    let [app_data, project, worktree] = arguments else {
        return Err(Error::Invalid(USAGE.into()));
    };
    let project = project
        .to_str()
        .ok_or_else(|| Error::Invalid("Invalid project identity".into()))?;
    let cache = PathBuf::from(app_data).join("graph").join(project);
    serve(
        PathBuf::from(worktree),
        cache,
        std::io::BufReader::new(std::io::stdin()),
        std::io::stdout(),
    )
}

type Shared = Arc<OnceLock<Result<Mutex<Tools>, String>>>;

/// How long `initialize` waits for the index, so the session-start map can go out with the
/// instructions. With a warm cache a build takes a fraction of this.
const MAP_WAIT: Duration = Duration::from_millis(1500);

/// One agent's connection.
struct Connection {
    tools: Shared,
    root: PathBuf,
    cache: PathBuf,
    dirty: Arc<Mutex<HashSet<String>>>,
    /// Signalled once the background build finished; taken by the first `initialize`.
    built: Mutex<Option<mpsc::Receiver<()>>>,
}

/// Serves one connection. The index is built in the background from the start, so the first
/// tool call usually finds it ready.
pub fn serve(
    root: PathBuf,
    cache: PathBuf,
    input: impl BufRead,
    mut output: impl Write,
) -> Result<(), Error> {
    let (signal, built) = mpsc::channel();
    let connection = Connection {
        tools: Arc::new(OnceLock::new()),
        root,
        cache,
        dirty: Arc::default(),
        built: Mutex::new(Some(built)),
    };
    let builder = {
        let (tools, root, cache) = (
            connection.tools.clone(),
            connection.root.clone(),
            connection.cache.clone(),
        );
        std::thread::spawn(move || {
            tools.get_or_init(|| build(&root, &cache));
            let _ = signal.send(());
        })
    };
    // Kept alive for the connection; a watcher that can't start leaves the staleness check
    // to catch edits.
    let _watcher = watch(&connection.root, connection.dirty.clone());
    for line in input.lines() {
        let line = line?;
        let response = match serde_json::from_str::<Value>(&line) {
            Ok(request) => respond(&request, &connection),
            Err(_) => Some(failure(Value::Null, -32700, "Parse error")),
        };
        if let Some(response) = response {
            writeln!(output, "{response}")?;
            output.flush()?;
        }
    }
    // A connection that ends while the index is still building (a warming run, below) lets
    // the build finish, so its parses reach the cache.
    let _ = builder.join();
    Ok(())
}

/// Fills the project's parse cache from `worktree` in the background (F19 "Warming"): the
/// executable runs as a graph server with no input, below normal priority, and exits once the
/// index is built.
pub fn warm(
    executable: &Path,
    app_data: &Path,
    project: &str,
    worktree: &Path,
) -> std::io::Result<std::process::Child> {
    let mut command = crate::files::background_command(executable);
    command
        .arg(SERVER_SWITCH)
        .args(server_arguments(app_data, project, worktree))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
}

/// Builds the index. A panic in a parser becomes an error the tools report, rather than the
/// end of the agent's graph server.
fn build(root: &Path, cache: &Path) -> Result<Mutex<Tools>, String> {
    match std::panic::catch_unwind(|| Index::open(root, cache)) {
        Ok(Ok(index)) => Ok(Mutex::new(Tools::new(index))),
        Ok(Err(error)) => Err(error.to_string()),
        Err(_) => Err("the indexer stopped on an internal error".to_owned()),
    }
}

fn watch(root: &Path, dirty: Arc<Mutex<HashSet<String>>>) -> Option<notify::RecommendedWatcher> {
    let base = root.to_path_buf();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let Ok(event) = event else { return };
        let mut dirty = dirty.lock().unwrap_or_else(|poison| poison.into_inner());
        for path in event.paths {
            let Ok(relative) = path.strip_prefix(&base) else {
                continue;
            };
            let relative = relative.to_string_lossy().replace('\\', "/");
            if !relative.split('/').any(|part| NOISE.contains(&part)) {
                dirty.insert(relative);
            }
        }
    })
    .ok()?;
    watcher.watch(root, RecursiveMode::Recursive).ok()?;
    Some(watcher)
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
    let map = match connection.tools.get() {
        Some(Ok(tools)) => tools
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .map(),
        _ => String::new(),
    };
    if map.is_empty() {
        INSTRUCTIONS.to_owned()
    } else {
        format!("{INSTRUCTIONS}\n\n{map}")
    }
}

fn call(parameters: &Value, connection: &Connection) -> Value {
    let name = parameters["name"].as_str().unwrap_or_default();
    let tools = connection
        .tools
        .get_or_init(|| build(&connection.root, &connection.cache));
    let result = match tools {
        Ok(tools) => {
            let mut tools = tools.lock().unwrap_or_else(|poison| poison.into_inner());
            let changed: Vec<String> = connection
                .dirty
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .drain()
                .collect();
            // One request that trips a bug fails alone; the agent keeps its graph.
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                tools
                    .refresh(&changed)
                    .map_err(|error| error.to_string())
                    .and_then(|()| tools.call(name, &parameters["arguments"]))
            }))
            .unwrap_or_else(|_| Err("the code graph failed on this request".to_owned()))
        }
        Err(error) => Err(format!("the code graph could not be built: {error}")),
    };
    match result {
        Ok(text) => json!({"content": [{"type": "text", "text": text}]}),
        Err(text) => json!({"isError": true, "content": [{"type": "text", "text": text}]}),
    }
}

fn failure(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

fn tool_list() -> Vec<Value> {
    vec![
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
            "One record by address, exact name or path: signature, doc, file and lines, its chain up, its children, and its links by relation (calls, calls (in), implements, references, mentions, imports) with how each was found. Fails as `ambiguous` (with every candidate), `missing` (with the nearest names) or `stale`; it never picks between candidates.",
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
                "depth": {"type": "integer", "minimum": 1, "maximum": 6, "description": "Hops to follow (default 3)"},
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
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": properties, "required": required},
    })
}
