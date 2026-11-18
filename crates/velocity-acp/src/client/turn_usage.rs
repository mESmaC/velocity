//! Measuring the code graph (F19 "Measuring it"): each turn's tokens and working time go into
//! its project's totals for the graph on or off. Every role's session passes through here, so
//! threads, swarms and duty agents are all counted.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use agent_client_protocol::v1::schema::{
    SessionUpdate, StopReason, ToolCallLocation, ToolKind, Usage,
};
use velocity_graph::usage::{SourceReads, TokenCounts, TurnUsage, record_turn};

use super::airc::airc_project;
use super::session::SessionSetup;

/// How a `session/prompt` ended, with what it reported and how long it took.
pub(super) struct TurnReport {
    pub outcome: Result<StopReason, String>,
    /// The agent's running totals for the session (ACP reports them cumulatively).
    pub usage: Option<TokenCounts>,
    pub elapsed: Duration,
}

pub(super) fn counts(usage: &Usage) -> TokenCounts {
    TokenCounts {
        input: usage.input_tokens,
        output: usage.output_tokens,
        cached_read: usage.cached_read_tokens.unwrap_or_default(),
        cached_write: usage.cached_write_tokens.unwrap_or_default(),
        total: usage.total_tokens,
    }
}

/// Where a session's turns are counted: its project, and whether it got the graph.
pub(super) struct UsageMeter {
    app_data: PathBuf,
    project: String,
    graph_on: bool,
    worktree: PathBuf,
    thread: Option<String>,
    /// The agent's running totals after the previous turn.
    last: Option<TokenCounts>,
}

impl UsageMeter {
    /// The meter for a session started from `setup`; `None` for a session with no AIRC (a
    /// headless probe), which belongs to no project.
    pub fn for_setup(setup: &SessionSetup, graph_on: bool) -> Option<Self> {
        let (airc, app_data, project) = airc_project(setup.airc.as_ref()?)?;
        Some(Self {
            app_data: PathBuf::from(app_data),
            project: project.to_owned(),
            graph_on,
            worktree: setup.worktree.clone(),
            thread: super::airc::usage_thread(airc).map(str::to_owned),
            last: None,
        })
    }

    /// Adds a finished turn. A turn whose agent reported no tokens still counts its time.
    pub fn record(&mut self, report: &TurnReport, reads: SourceReads) {
        let tokens = report.usage.map(|running| {
            let turn = running.since(self.last.unwrap_or_default());
            self.last = Some(running);
            turn
        });
        let turn = TurnUsage {
            tokens,
            working_milliseconds: report.elapsed.as_millis() as u64,
        };
        // Measurement never gets in the way of the agent: a failed write loses one turn.
        let _ = record_turn(
            &self.app_data,
            &self.project,
            self.graph_on,
            turn,
            Some((&self.worktree, self.thread.as_deref(), reads)),
        );
    }
}

/// ACP providers differ in what their tool events expose. Count source reads when a
/// source path is present, and keep one count per tool ID across partial updates.
#[derive(Default)]
pub(super) struct CodeReads {
    started: bool,
    observed: HashSet<Arc<str>>,
    pending: HashMap<Arc<str>, ToolObservation>,
}

#[derive(Default)]
struct ToolObservation {
    kind: Option<ToolKind>,
    name: Option<String>,
    locations: Vec<ToolCallLocation>,
    input: Option<serde_json::Value>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ReadScope {
    Raw,
    WholeFile,
}

impl CodeReads {
    pub(super) fn start(&mut self) {
        self.started = true;
    }

    pub(super) fn take_count(&mut self) -> SourceReads {
        let mut reads = SourceReads::default();
        for (identifier, tool) in self.pending.drain() {
            self.observed.insert(identifier);
            // Streaming inputs may expose the path before an offset or limit.
            if let Some(scope) = code_read(
                tool.kind,
                tool.name.as_deref(),
                Some(&tool.locations),
                tool.input.as_ref(),
            ) {
                reads.calls += 1;
                reads.whole_file_calls += u64::from(scope == ReadScope::WholeFile);
            }
        }
        reads
    }

    pub(super) fn observe(&mut self, update: &SessionUpdate) {
        if !self.started {
            return;
        }
        let (identifier, kind, name, paths, input) = match update {
            SessionUpdate::ToolCall(call) => (
                &call.tool_call_id.0,
                Some(call.kind),
                call.name.as_deref(),
                Some(call.locations.as_slice()),
                call.raw_input.as_ref(),
            ),
            SessionUpdate::ToolCallUpdate(call) => (
                &call.tool_call_id.0,
                call.fields.kind,
                call.fields.name.as_deref(),
                call.fields.locations.as_deref(),
                call.fields.raw_input.as_ref(),
            ),
            _ => return,
        };
        if self.observed.contains(identifier) {
            return;
        }
        let tool = self.pending.entry(identifier.clone()).or_default();
        if let Some(kind) = kind {
            tool.kind = Some(kind);
        }
        if let Some(name) = name {
            tool.name = Some(name.to_owned());
        }
        if let Some(paths) = paths {
            tool.locations = paths.to_vec();
        }
        if let Some(input) = input
            && (matches!(tool.kind, Some(ToolKind::Read | ToolKind::Search))
                || tool.name.as_deref().is_some_and(named_read)
                || input["command"].is_string())
        {
            tool.input = Some(input.clone());
        }
    }
}

fn code_read(
    kind: Option<ToolKind>,
    name: Option<&str>,
    paths: Option<&[ToolCallLocation]>,
    input: Option<&serde_json::Value>,
) -> Option<ReadScope> {
    let name = name.map(str::to_ascii_lowercase);
    if name.as_deref().is_some_and(|name| {
        name.starts_with("mcp__velocity-graph__")
            || matches!(
                name,
                "graph_find" | "graph_read" | "graph_impact" | "graph_run" | "graph_overlaps"
            )
    }) {
        return None;
    }
    let source_location = paths.is_some_and(|paths| {
        paths
            .iter()
            .any(|location| source_path(&location.path.to_string_lossy()))
    });
    if let Some(command) = input.and_then(|input| input["command"].as_str()) {
        return shell_read(command, source_location);
    }
    let source_argument = input.is_some_and(|input| {
        [
            "path",
            "file_path",
            "filePath",
            "glob",
            "include",
            "filePattern",
        ]
        .iter()
        .filter_map(|key| input[*key].as_str())
        .any(source_path)
            || input["type"].as_str().is_some_and(source_type)
    });
    if !source_location && !source_argument {
        return None;
    }
    if matches!(kind, Some(ToolKind::Read)) || name.as_deref() == Some("read") {
        return Some(if whole_file_read(input, paths) {
            ReadScope::WholeFile
        } else {
            ReadScope::Raw
        });
    }
    (matches!(kind, Some(ToolKind::Search)) || matches!(name.as_deref(), Some("grep" | "rg")))
        .then_some(ReadScope::Raw)
}

fn named_read(name: &str) -> bool {
    matches!(name.to_ascii_lowercase().as_str(), "read" | "grep" | "rg")
}

fn whole_file_read(input: Option<&serde_json::Value>, paths: Option<&[ToolCallLocation]>) -> bool {
    input.is_some_and(|input| {
        ["limit", "end_line", "endLine"]
            .iter()
            .all(|key| input[*key].is_null())
            && ["offset", "start_line", "startLine"]
                .iter()
                .all(|key| input[*key].as_u64().is_none_or(|line| line <= 1))
    }) && paths.is_none_or(|paths| {
        paths
            .iter()
            .all(|location| location.line.is_none_or(|line| line <= 1))
    })
}

fn source_type(name: &str) -> bool {
    matches!(
        name,
        "rust"
            | "ts"
            | "js"
            | "svelte"
            | "py"
            | "go"
            | "java"
            | "cs"
            | "c"
            | "cpp"
            | "lean"
            | "sh"
            | "ps"
            | "sql"
            | "css"
            | "html"
    )
}

fn source_path(path: &str) -> bool {
    let path = path.trim_matches(['"', '\'', ')', '}', ',']);
    matches!(velocity_graph::Language::of(path), Some(language) if language != velocity_graph::Language::Markdown)
        || path.rsplit_once('.').is_some_and(|(_, extension)| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "sh" | "bash" | "ps1" | "psm1" | "sql" | "css" | "html"
            )
        })
}

fn shell_read(command: &str, source_location: bool) -> Option<ReadScope> {
    let mut quote = None;
    let parts = command.split_inclusive(|character: char| {
        if quote == Some(character) {
            quote = None;
            false
        } else if quote.is_none() && matches!(character, '"' | '\'') {
            quote = Some(character);
            false
        } else {
            quote.is_none() && matches!(character, '|' | ';' | '&' | '\n')
        }
    });
    let mut scope = None;
    for part in parts {
        let piped = part.ends_with('|');
        let mut words = part
            .trim_end_matches(['|', ';', '&', '\n'])
            .split_whitespace();
        let Some(program) = words.next() else {
            continue;
        };
        let program = program.trim_matches(['"', '\'', '(']).to_ascii_lowercase();
        let program = program.trim_end_matches(".exe");
        if !matches!(program, "cat" | "sed" | "get-content" | "grep" | "rg") {
            continue;
        }
        let mut has_source = source_location;
        let search = matches!(program, "grep" | "rg");
        let mut limited = piped;
        while let Some(word) = words.next() {
            has_source |= source_path(word);
            if search {
                if matches!(word, "-t" | "--type") {
                    has_source |= words.next().is_some_and(source_type);
                } else if let Some(name) = word.strip_prefix("--type=") {
                    has_source |= source_type(name);
                }
            }
            let flag = word.to_ascii_lowercase();
            limited |= matches!(
                flag.split(':').next(),
                Some("-totalcount" | "-head" | "-tail" | "-first" | "-last")
            );
        }
        if has_source {
            if matches!(program, "cat" | "get-content") && !limited {
                return Some(ReadScope::WholeFile);
            }
            scope = Some(ReadScope::Raw);
        }
    }
    scope
}
