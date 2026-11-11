//! A running thread's transcript as the thread view shows it (SPEC.md §3.2, F4, F5).
//!
//! The ACP client files every `session/update` under a transcript entry and reports where
//! ([`Placement`]). This mirrors those entries and folds them into the thread view's blocks:
//! messages, tool steps (consecutive calls share one group, as in the reference) and the
//! diffs an edit carries. The worktree step comes first (F5), and messages sent while the
//! agent is busy show as queued until a prompt carries them (Rule 1).

use std::path::Path;

use agent_client_protocol::v1::schema::{
    ContentBlock, ContentChunk, Diff, SessionUpdate, ToolCall, ToolCallContent, ToolCallStatus,
    ToolCallUpdateFields, ToolKind,
};
use similar::TextDiff;
use velocity_acp::Placement;
use velocity_core::skill_menu;
use velocity_core::thread_view_model::{
    DiffBlockView, ErrorCardView, ToolStepRow, TranscriptBlock, build_diff_block,
};

/// Lines of unchanged context around each change in a diff, as `git diff` shows.
const DIFF_CONTEXT_LINES: usize = 3;

#[derive(Clone, Debug)]
struct ToolView {
    title: String,
    name: Option<String>,
    kind: ToolKind,
    status: ToolCallStatus,
    location: Option<String>,
    diffs: Vec<DiffBlockView>,
    /// The skill this step loads, shown as "Skill · <name>" (F4, F18).
    skill: Option<String>,
}

#[derive(Clone, Debug)]
enum EntryView {
    Setup(ToolStepRow),
    User(String),
    Agent(String),
    Tool(Box<ToolView>),
    Error(ErrorCardView),
    /// Thoughts and plans: kept so ACP's indices still line up, but not shown.
    Hidden,
}

#[derive(Clone, Debug)]
pub struct LiveTranscript {
    worktree: String,
    entries: Vec<EntryView>,
    /// `acp_entries[i]` is where ACP transcript entry `i` sits in `entries`.
    acp_entries: Vec<usize>,
    queued: Vec<String>,
}

impl LiveTranscript {
    /// A thread whose worktree is being created and set up (F5).
    pub fn new(worktree: &str) -> Self {
        LiveTranscript {
            worktree: worktree.to_owned(),
            entries: Vec::from([EntryView::Setup(setup_step(worktree, true))]),
            acp_entries: Vec::new(),
            queued: Vec::new(),
        }
    }

    /// The worktree took the title's name while setup was still running (F4).
    pub fn rename_worktree(&mut self, worktree: &str) {
        worktree.clone_into(&mut self.worktree);
        if let Some(EntryView::Setup(step)) = self.entries.first_mut() {
            *step = setup_step(worktree, true);
        }
    }

    /// The worktree is ready and the agent may start (F5).
    pub fn worktree_ready(&mut self) {
        let worktree = self.worktree.clone();
        if let Some(EntryView::Setup(step)) = self.entries.first_mut() {
            *step = setup_step(&worktree, false);
        }
    }

    pub fn record_error(&mut self, headline: &str, detail: &str) {
        self.entries.push(EntryView::Error(ErrorCardView {
            headline: headline.to_owned(),
            detail: detail.to_owned(),
        }));
    }

    /// A parked thread's next session (§2.2): its ACP entries are numbered from zero again,
    /// while everything shown so far stays.
    pub fn new_session(&mut self) {
        self.acp_entries.clear();
        self.worktree_ready();
    }

    /// Messages still waiting for the agent, oldest first.
    pub fn queued(&self) -> &[String] {
        &self.queued
    }

    /// A message waiting for the agent's current turn to end.
    pub fn queue(&mut self, message: &str) {
        self.queued.push(message.to_owned());
    }

    /// Files one update where the ACP client placed it.
    pub fn record(&mut self, placement: Placement, update: &SessionUpdate) {
        match placement {
            Placement::Added(_) => {
                // The client adds entries in order, so the next ACP index is the next one here.
                let entry = new_entry(update, &self.worktree);
                self.acp_entries.push(self.entries.len());
                self.entries.push(entry);
            }
            Placement::Appended(index) => {
                if let Some(entry) = self
                    .acp_entries
                    .get(index)
                    .and_then(|position| self.entries.get_mut(*position))
                {
                    merge(entry, update, &self.worktree);
                }
            }
        }
    }

    /// The oldest `count` queued messages went out as the agent's next prompt. Only those
    /// leave the queue: a message queued after them still waits for the following turn.
    pub fn sent_queued(&mut self, count: usize) {
        self.queued.drain(..count.min(self.queued.len()));
    }

    /// Whether a tool call is pending or running: activity `executing` (§2.2).
    pub fn has_running_tool(&self) -> bool {
        self.entries.iter().any(|entry| match entry {
            EntryView::Tool(tool) => is_running(tool.status),
            _ => false,
        })
    }

    /// `+added −deleted` across every edit the agent made, for the thread header (F4).
    pub fn line_counts(&self) -> (usize, usize) {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                EntryView::Tool(tool) => Some(&tool.diffs),
                _ => None,
            })
            .flatten()
            .fold((0, 0), |(added, deleted), diff| {
                (added + diff.added, deleted + diff.deleted)
            })
    }

    pub fn blocks(&self) -> Vec<TranscriptBlock> {
        let mut blocks = Vec::new();
        let mut steps = Vec::new();
        for entry in &self.entries {
            match entry {
                EntryView::Setup(step) => steps.push(step.clone()),
                EntryView::Tool(tool) => {
                    steps.push(tool_step(tool));
                    if !tool.diffs.is_empty() {
                        flush_steps(&mut blocks, &mut steps);
                        blocks.extend(tool.diffs.iter().map(|diff| TranscriptBlock::Diff {
                            block: diff.clone(),
                        }));
                    }
                }
                EntryView::Hidden => {}
                other => {
                    flush_steps(&mut blocks, &mut steps);
                    blocks.extend(message_block(other));
                }
            }
        }
        flush_steps(&mut blocks, &mut steps);
        blocks.extend(self.queued.iter().map(|text| TranscriptBlock::UserMessage {
            text: text.clone(),
            queued: true,
        }));
        blocks
    }

    /// Agent calls only: the worktree setup row is not a provider tool call.
    pub fn tool_steps(&self) -> Vec<ToolStepRow> {
        self.entries
            .iter()
            .filter_map(|entry| match entry {
                EntryView::Tool(tool) => Some(tool_step(tool)),
                _ => None,
            })
            .collect()
    }
}

fn setup_step(worktree: &str, running: bool) -> ToolStepRow {
    ToolStepRow {
        running,
        verb: "Worktree".into(),
        argument: worktree.to_owned(),
        result_text: if running {
            "creating from master".into()
        } else {
            "created from master".into()
        },
    }
}

fn flush_steps(blocks: &mut Vec<TranscriptBlock>, steps: &mut Vec<ToolStepRow>) {
    if !steps.is_empty() {
        blocks.push(TranscriptBlock::ToolGroup {
            steps: std::mem::take(steps),
        });
    }
}

fn message_block(entry: &EntryView) -> Option<TranscriptBlock> {
    match entry {
        EntryView::User(text) => Some(TranscriptBlock::UserMessage {
            text: text.clone(),
            queued: false,
        }),
        EntryView::Agent(text) => Some(TranscriptBlock::AgentMessage {
            paragraphs: paragraphs(text),
        }),
        EntryView::Error(card) => Some(TranscriptBlock::ErrorCard { card: card.clone() }),
        EntryView::Setup(_) | EntryView::Tool(_) | EntryView::Hidden => None,
    }
}

/// Preserve markdown verbatim: trimming split paragraphs corrupts indented code
/// after blank lines inside fences and nested lists.
fn paragraphs(text: &str) -> Vec<String> {
    if text.trim().is_empty() {
        Vec::new()
    } else {
        vec![text.to_owned()]
    }
}

fn new_entry(update: &SessionUpdate, worktree: &str) -> EntryView {
    match update {
        SessionUpdate::UserMessageChunk(chunk) => EntryView::User(chunk_text(chunk)),
        SessionUpdate::AgentMessageChunk(chunk) => EntryView::Agent(chunk_text(chunk)),
        SessionUpdate::ToolCall(call) => EntryView::Tool(Box::new(tool_view(call, worktree))),
        SessionUpdate::ToolCallUpdate(update) => {
            let mut tool = ToolView {
                title: String::new(),
                name: None,
                kind: ToolKind::Other,
                status: ToolCallStatus::Pending,
                location: None,
                diffs: Vec::new(),
                skill: None,
            };
            apply_fields(&mut tool, &update.fields, worktree);
            EntryView::Tool(Box::new(tool))
        }
        _ => EntryView::Hidden,
    }
}

fn merge(entry: &mut EntryView, update: &SessionUpdate, worktree: &str) {
    match (entry, update) {
        (EntryView::User(text), SessionUpdate::UserMessageChunk(chunk)) => {
            // Each queued message is its own chunk of one prompt.
            text.push_str("\n\n");
            text.push_str(&chunk_text(chunk));
        }
        (EntryView::Agent(text), SessionUpdate::AgentMessageChunk(chunk)) => {
            text.push_str(&chunk_text(chunk));
        }
        (EntryView::Tool(tool), SessionUpdate::ToolCall(call)) => {
            **tool = tool_view(call, worktree);
        }
        (EntryView::Tool(tool), SessionUpdate::ToolCallUpdate(update)) => {
            apply_fields(tool, &update.fields, worktree);
        }
        _ => {}
    }
}

fn chunk_text(chunk: &ContentChunk) -> String {
    match &chunk.content {
        ContentBlock::Text(text) => text.text.clone(),
        ContentBlock::Image(_) => "[image]".into(),
        ContentBlock::ResourceLink(link) => link.uri.clone(),
        _ => String::new(),
    }
}

fn tool_view(call: &ToolCall, worktree: &str) -> ToolView {
    let first_path = call.locations.first().map(|location| &location.path);
    ToolView {
        skill: loaded_skill(&call.title, call.raw_input.as_ref(), first_path),
        title: call.title.clone(),
        name: call.name.clone(),
        kind: call.kind,
        status: call.status,
        location: call
            .locations
            .first()
            .map(|location| display_path(&location.path, worktree)),
        diffs: diffs(&call.content, worktree),
    }
}

fn apply_fields(tool: &mut ToolView, fields: &ToolCallUpdateFields, worktree: &str) {
    if let Some(title) = &fields.title {
        tool.title.clone_from(title);
    }
    if let Some(name) = &fields.name {
        tool.name = Some(name.clone());
    }
    if let Some(kind) = fields.kind {
        tool.kind = kind;
    }
    if let Some(status) = fields.status {
        tool.status = status;
    }
    if let Some(locations) = &fields.locations {
        tool.location = locations
            .first()
            .map(|location| display_path(&location.path, worktree));
    }
    if let Some(content) = &fields.content {
        tool.diffs = diffs(content, worktree);
    }
    let first_path = fields
        .locations
        .as_ref()
        .and_then(|locations| locations.first())
        .map(|location| &location.path);
    if let Some(skill) = loaded_skill(&tool.title, fields.raw_input.as_ref(), first_path) {
        tool.skill = Some(skill);
    }
}

/// The skill a tool call loads, from its title, its raw `skill` input or the `SKILL.md` it
/// reads (F4, F18).
fn loaded_skill(
    title: &str,
    raw_input: Option<&serde_json::Value>,
    path: Option<&std::path::PathBuf>,
) -> Option<String> {
    let raw_skill = raw_input
        .and_then(|input| input.get("skill"))
        .and_then(serde_json::Value::as_str);
    let path = path.map(|path| path.to_string_lossy());
    skill_menu::loaded_skill(title, raw_skill, path.as_deref())
}

fn diffs(content: &[ToolCallContent], worktree: &str) -> Vec<DiffBlockView> {
    content
        .iter()
        .filter_map(|item| match item {
            ToolCallContent::Diff(diff) => Some(diff_block(diff, worktree)),
            _ => None,
        })
        .collect()
}

fn diff_block(diff: &Diff, worktree: &str) -> DiffBlockView {
    let old_text = diff.old_text.as_deref().unwrap_or_default();
    let unified = TextDiff::from_lines(old_text, &diff.new_text)
        .unified_diff()
        .context_radius(DIFF_CONTEXT_LINES)
        .to_string();
    let lines = unified
        .lines()
        .map(|line| line.trim_end_matches('\r').to_owned())
        .collect();
    build_diff_block(display_path(&diff.path, worktree), lines)
}

/// Paths inside the worktree show relative to it, as the reference does.
fn display_path(path: &Path, worktree: &str) -> String {
    path.strip_prefix(worktree)
        .map(|relative| relative.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

fn is_running(status: ToolCallStatus) -> bool {
    matches!(status, ToolCallStatus::Pending | ToolCallStatus::InProgress)
}

fn tool_step(tool: &ToolView) -> ToolStepRow {
    let (added, deleted) = tool.diffs.iter().fold((0, 0), |(added, deleted), diff| {
        (added + diff.added, deleted + diff.deleted)
    });
    let result_text = if matches!(tool.status, ToolCallStatus::Failed) {
        "failed".into()
    } else if added + deleted > 0 {
        format!("+{added} −{deleted}")
    } else {
        String::new()
    };
    if let Some(skill) = &tool.skill {
        return ToolStepRow {
            running: is_running(tool.status),
            verb: "Skill".into(),
            argument: skill.clone(),
            result_text,
        };
    }
    ToolStepRow {
        running: is_running(tool.status),
        verb: tool_verb(tool),
        argument: tool.location.clone().unwrap_or_else(|| tool.title.clone()),
        result_text,
    }
}

fn tool_verb(tool: &ToolView) -> String {
    let verb = match tool.kind {
        ToolKind::Read => "Read",
        ToolKind::Edit => "Edit",
        ToolKind::Delete => "Delete",
        ToolKind::Move => "Move",
        ToolKind::Search => "Search",
        ToolKind::Execute => "Bash",
        ToolKind::Think => "Think",
        ToolKind::Fetch => "Fetch",
        ToolKind::SwitchMode => "Mode",
        _ => return tool.name.clone().unwrap_or_else(|| "Tool".into()),
    };
    verb.into()
}
