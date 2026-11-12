//! Measuring the code graph (F19 "Measuring it"): each turn's tokens and working time go into
//! its project's totals for the graph on or off. Every role's session passes through here, so
//! threads, swarms and duty agents are all counted.

use std::path::PathBuf;
use std::time::Duration;

use agent_client_protocol::v1::schema::{StopReason, Usage};
use velocity_graph::usage::{TokenCounts, TurnUsage, record};

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
            last: None,
        })
    }

    /// Adds a finished turn. A turn whose agent reported no tokens still counts its time.
    pub fn record(&mut self, report: &TurnReport) {
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
        let _ = record(&self.app_data, &self.project, self.graph_on, turn);
    }
}
