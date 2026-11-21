//! A thread's subagents (SPEC.md §3.2 Subagents, #436): one node per spawning tool call,
//! or per subagent session when the agent gives each subagent its own, with its own
//! transcript, status, model, start time and token use.
//!
//! The ACP client files a subagent's updates under its spawning call or session
//! ([`velocity_acp::client::AgentEvent::Subagent`]). The facts about each subagent come
//! from the spawning call's own updates ([`velocity_acp::subagent_report`]), or from what
//! the subagent's session and its parent report ([`velocity_acp::session_report`]).

use std::collections::BTreeSet;

use agent_client_protocol::v1::schema::{SessionUpdate, ToolCallStatus};
use velocity_acp::{
    Placement, SubagentActivity, SubagentReport, agent_thread_states, session_report,
    subagent_report,
};
use velocity_core::thread_view_model::{SubagentStatus, SubagentView};

use crate::transcript_view::LiveTranscript;

/// The title a subagent shows until its spawning call names its task.
const UNNAMED: &str = "Subagent";

#[derive(Clone, Debug)]
struct SubagentNode {
    spawn: String,
    title: String,
    model: Option<String>,
    status: SubagentStatus,
    /// Its spawning call returns before it finishes (§3.2 Subagents).
    background: bool,
    started_at: u64,
    ended_at: Option<u64>,
    tokens: Option<u64>,
    /// Codex child threads, whose `agentsStates` report this subagent's status.
    threads: Vec<String>,
    transcript: LiveTranscript,
}

/// Every subagent of one thread's live session, oldest first.
#[derive(Clone, Debug, Default)]
pub struct Subagents {
    worktree: String,
    nodes: Vec<SubagentNode>,
    /// Subagents changed since the last [`Subagents::take_changed`].
    changed: BTreeSet<String>,
}

impl Subagents {
    pub fn new(worktree: &str) -> Self {
        Subagents {
            worktree: worktree.to_owned(),
            ..Self::default()
        }
    }

    /// Subagents started from now on show paths under the thread's actual workspace (F4).
    pub fn rename_worktree(&mut self, worktree: &str) {
        worktree.clone_into(&mut self.worktree);
    }

    /// Reads one of the parent's updates for the subagents it starts or reports on.
    pub fn observe(&mut self, update: &SessionUpdate, now: u64) {
        let known = |call: &str| self.nodes.iter().any(|node| node.spawn == call);
        if let Some(report) = subagent_report(update, known) {
            self.apply_report(report, now);
        }
        for (thread, status) in agent_thread_states(update) {
            if let Some(node) = self
                .nodes
                .iter_mut()
                .find(|node| node.threads.contains(&thread))
                && set_status(node, thread_status(status), now)
            {
                self.changed.insert(node.spawn.clone());
            }
        }
    }

    /// Reads what the session of the subagent `spawn`, or its parent, reports about it: its
    /// announcement and state, its model, its token use.
    pub fn observe_session(&mut self, spawn: &str, update: &SessionUpdate, now: u64) {
        if let Some(report) = session_report(spawn, update) {
            self.apply_report(report, now);
        }
    }

    /// Files an update a subagent made in that subagent's transcript. A spawn it makes
    /// itself becomes a subagent of its own.
    pub fn record(&mut self, spawn: &str, placement: Placement, update: &SessionUpdate, now: u64) {
        self.node_mut(spawn, now)
            .transcript
            .record(placement, update);
        self.changed.insert(spawn.to_owned());
        self.observe(update, now);
    }

    /// The session ended: nothing it started still runs. A subagent cut off mid-call
    /// failed; a background one is reported done, as its parent saw it to the end.
    pub fn session_ended(&mut self) {
        let now = unix_now();
        for node in &mut self.nodes {
            if node.status == SubagentStatus::Running {
                let status = if node.background {
                    SubagentStatus::Done
                } else {
                    SubagentStatus::Failed
                };
                set_status(node, status, now);
                self.changed.insert(node.spawn.clone());
            }
        }
    }

    /// Whether a subagent still works: its parent is waiting, not stalled (§2.2).
    pub fn running(&self) -> bool {
        self.running_transcripts().next().is_some()
    }

    /// How many subagents currently run, without cloning their transcripts (#553).
    pub fn running_count(&self) -> usize {
        self.running_transcripts().count()
    }

    /// Whether a running subagent waits in, or runs at the head of, the build queue (§2.2).
    pub fn in_build_queue(&self) -> bool {
        self.running_transcripts()
            .any(|transcript| crate::agent_activity::in_build_queue(&transcript.blocks()))
    }

    /// `+added −deleted` across every subagent's edits.
    pub fn line_counts(&self) -> (usize, usize) {
        self.nodes
            .iter()
            .map(|node| node.transcript.line_counts())
            .fold((0, 0), |(added, deleted), (more_added, more_deleted)| {
                (added + more_added, deleted + more_deleted)
            })
    }

    pub fn views(&self) -> Vec<SubagentView> {
        self.nodes.iter().map(view).collect()
    }

    /// The subagents changed since the last call, for the `thread://subagent` event.
    pub fn take_changed(&mut self) -> Vec<SubagentView> {
        let changed = std::mem::take(&mut self.changed);
        self.nodes
            .iter()
            .filter(|node| changed.contains(&node.spawn))
            .map(view)
            .collect()
    }

    fn running_transcripts(&self) -> impl Iterator<Item = &LiveTranscript> {
        self.nodes
            .iter()
            .filter(|node| node.status == SubagentStatus::Running)
            .map(|node| &node.transcript)
    }

    fn apply_report(&mut self, report: SubagentReport, now: u64) {
        self.changed.insert(report.spawn.clone());
        let node = self.node_mut(&report.spawn, now);
        if let Some(description) = report.description {
            node.title = description;
        } else if node.title == UNNAMED
            && let Some(title) = report.title.filter(|title| !title.trim().is_empty())
        {
            node.title = title;
        }
        if report.model.is_some() {
            node.model = report.model;
        }
        if report.tokens.is_some() {
            node.tokens = report.tokens;
        }
        node.background |= report.background;
        for thread in report.threads {
            if !node.threads.contains(&thread) {
                node.threads.push(thread);
            }
        }
        if let Some(status) = report.status {
            let next = call_status(node, status);
            set_status(node, next, now);
        }
        if let Some(activity) = report.activity {
            let next = activity_status(node.status, activity);
            set_status(node, next, now);
        }
    }

    fn node_mut(&mut self, spawn: &str, now: u64) -> &mut SubagentNode {
        let index = match self.nodes.iter().position(|node| node.spawn == spawn) {
            Some(index) => index,
            None => {
                self.nodes.push(SubagentNode {
                    spawn: spawn.to_owned(),
                    title: UNNAMED.to_owned(),
                    model: None,
                    status: SubagentStatus::Running,
                    background: false,
                    started_at: now,
                    ended_at: None,
                    tokens: None,
                    threads: Vec::new(),
                    transcript: LiveTranscript::subagent(&self.worktree),
                });
                self.nodes.len() - 1
            }
        };
        &mut self.nodes[index]
    }
}

/// Updates a subagent's status, recording its finish time once it leaves `Running`.
fn set_status(node: &mut SubagentNode, status: SubagentStatus, now: u64) -> bool {
    let changed = node.status != status;
    node.status = status;
    match status {
        SubagentStatus::Running => node.ended_at = None,
        SubagentStatus::Done | SubagentStatus::Failed if node.ended_at.is_none() => {
            node.ended_at = Some(now);
        }
        SubagentStatus::Done | SubagentStatus::Failed => {}
    }
    changed
}

/// A subagent's status after its spawning call reports `status`. A background subagent's
/// call completes while it still works (a Codex spawn completes once the child thread
/// exists), so only a failure ends it.
fn call_status(node: &SubagentNode, status: ToolCallStatus) -> SubagentStatus {
    match status {
        ToolCallStatus::Failed => SubagentStatus::Failed,
        ToolCallStatus::Completed if node.background => node.status,
        ToolCallStatus::Completed => SubagentStatus::Done,
        _ => SubagentStatus::Running,
    }
}

/// A subagent's status after its own session reports `activity`. A session the agent lost
/// sight of was cut off if it was working; one that had finished keeps its outcome.
fn activity_status(current: SubagentStatus, activity: SubagentActivity) -> SubagentStatus {
    match activity {
        SubagentActivity::Running => SubagentStatus::Running,
        SubagentActivity::Done => SubagentStatus::Done,
        SubagentActivity::Failed => SubagentStatus::Failed,
        SubagentActivity::Unknown if current == SubagentStatus::Running => SubagentStatus::Failed,
        SubagentActivity::Unknown => current,
    }
}

/// A Codex subagent's status from its child thread's state.
fn thread_status(status: ToolCallStatus) -> SubagentStatus {
    match status {
        ToolCallStatus::Failed => SubagentStatus::Failed,
        ToolCallStatus::Completed => SubagentStatus::Done,
        _ => SubagentStatus::Running,
    }
}

fn view(node: &SubagentNode) -> SubagentView {
    let steps = node.transcript.tool_steps();
    SubagentView {
        identifier: node.spawn.clone(),
        title: node.title.clone(),
        model: node.model.clone(),
        status: node.status,
        started_at: usize::try_from(node.started_at).unwrap_or(usize::MAX),
        ended_at: node
            .ended_at
            .map(|ended| usize::try_from(ended).unwrap_or(usize::MAX)),
        tokens: node
            .tokens
            .map(|tokens| usize::try_from(tokens).unwrap_or(usize::MAX)),
        step_count: steps.len(),
        current_step: steps
            .last()
            .map(|step| format!("{} · {}", step.verb, step.argument)),
        blocks: node.transcript.blocks(),
    }
}

/// Unix seconds now, for a subagent's start time.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
