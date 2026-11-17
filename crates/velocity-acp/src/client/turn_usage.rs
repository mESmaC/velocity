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
use velocity_graph::usage::{TokenCounts, TurnUsage, record_turn};

use super::airc::airc_project;
use super::session::{SessionSetup, server_name, session_servers};

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
    /// The agent's running totals after the previous turn.
    last: Option<TokenCounts>,
}

impl UsageMeter {
    /// The meter for a session started from `setup`; `None` for a session with no AIRC (a
    /// headless probe), which belongs to no project.
    pub fn for_setup(setup: &SessionSetup) -> Option<Self> {
        let (_, app_data, project) = airc_project(setup.airc.as_ref()?)?;
        let graph_on = session_servers(setup)
            .iter()
            .any(|server| server_name(server) == Some(velocity_graph::SERVER_NAME));
        Some(Self {
            app_data: PathBuf::from(app_data),
            project: project.to_owned(),
            graph_on,
            worktree: setup.worktree.clone(),
            last: None,
        })
    }

    /// Adds a finished turn. A turn whose agent reported no tokens still counts its time.
    pub fn record(&mut self, report: &TurnReport, reads: u64) {
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
            Some((&self.worktree, reads)),
        );
    }
}

/// ACP providers differ in what their tool events expose. Count source reads when a
/// source path is present, and keep one count per tool ID across partial updates.
#[derive(Default)]
pub(super) struct CodeReads {
    started: bool,
    observed: HashSet<Arc<str>>,
    kinds: HashMap<Arc<str>, ToolKind>,
    pending: u64,
}

impl CodeReads {
    pub(super) fn start(&mut self) {
        self.started = true;
    }

    pub(super) fn take_count(&mut self) -> u64 {
        std::mem::take(&mut self.pending)
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
        if let Some(kind) = kind {
            self.kinds.insert(identifier.clone(), kind);
        }
        if is_code_read(self.kinds.get(identifier).copied(), name, paths, input) {
            self.kinds.remove(identifier);
            self.observed.insert(identifier.clone());
            self.pending += 1;
        }
    }
}

fn is_code_read(
    kind: Option<ToolKind>,
    name: Option<&str>,
    paths: Option<&[ToolCallLocation]>,
    input: Option<&serde_json::Value>,
) -> bool {
    if name.is_some_and(|name| {
        ["graph_find", "graph_read", "graph_impact", "graph_run"]
            .iter()
            .any(|tool| name.ends_with(tool))
    }) {
        return false;
    }
    let source_path = |path: &str| velocity_graph::Language::of(path).is_some();
    let source_location = paths.is_some_and(|paths| {
        paths
            .iter()
            .any(|location| source_path(&location.path.to_string_lossy()))
    });
    let source_argument = input.is_some_and(|input| {
        ["path", "file_path", "filePath"]
            .iter()
            .filter_map(|key| input[*key].as_str())
            .any(source_path)
    });
    let read_or_search = matches!(kind, Some(ToolKind::Read | ToolKind::Search))
        || name.is_some_and(|name| {
            matches!(name.to_ascii_lowercase().as_str(), "read" | "grep" | "rg")
        });
    if read_or_search && (source_location || source_argument) {
        return true;
    }
    let command = input.and_then(|input| input["command"].as_str());
    command.is_some_and(|command| {
        command.split(['|', ';', '&', '\n']).any(|part| {
            let mut words = part.split_whitespace();
            let read = words.next().is_some_and(|word| {
                matches!(
                    word.to_ascii_lowercase().as_str(),
                    "cat" | "sed" | "get-content" | "grep" | "rg"
                )
            });
            read && (source_location
                || words.any(|word| source_path(word.trim_matches(['"', '\'']))))
        })
    })
}
