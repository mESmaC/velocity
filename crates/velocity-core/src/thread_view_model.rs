//! Read models, slash-menu state, and reducer helpers for the thread view (`SPEC.md §4 F4`, `F5`, `F6`, `F15`, `§7 M3`).
//!
//! All domain decisions delegate to the Aeneas-verified functions in `crate::thread_view`.

use crate::interview_view::InterviewView;
use crate::layout_view::ProviderColor;
use crate::thread_view::{
    self, ComposerKey, ComposerKeyAction, ComposerSubmissionKind, ComposerSubmitEffect, DiffCounts,
    DiffLineKind, SlashCommand, SlashPrefix, ThreadLifecycle, ToolStepKind, ToolStepStatus,
    TranscriptMode, WorktreeChipState,
};

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkspaceChipView {
    Hidden,
    Live,
    Deleted,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SlashCommandItem {
    pub command: String,
    pub description: String,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AgentCommandView {
    pub name: String,
    pub description: String,
}

impl AgentCommandView {
    pub fn new(name: String, description: String) -> Self {
        Self { name, description }
    }
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelPillView {
    pub usage_account: Option<String>,
    pub provider: ProviderColor,
    pub model_name: String,
    pub effort_label: String,
    pub show_fast_badge: bool,
    pub speed_control_enabled: bool,
    pub preserve_requested_fast: bool,
}

impl ModelPillView {
    /// A pill for `model_name`, with its fast-mode state from `thread_view::project_model_pill`.
    pub fn new(
        provider: ProviderColor,
        model_name: String,
        effort_label: String,
        state: crate::thread_view::ModelPillState,
    ) -> Self {
        Self {
            provider,
            usage_account: None,
            model_name,
            effort_label,
            show_fast_badge: state.show_fast_badge,
            speed_control_enabled: state.speed_control_enabled,
            preserve_requested_fast: state.preserve_requested_fast,
        }
    }
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolStepRow {
    pub running: bool,
    pub verb: String,
    pub argument: String,
    pub result_text: String,
    /// The ACP tool call that produced this step, so a spawning step links to its subagent card (F4).
    #[cfg_attr(
        feature = "bindings",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub tool_call_id: Option<String>,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLineRole {
    Added,
    Deleted,
    Context,
    HunkHeader,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLineView {
    pub role: DiffLineRole,
    pub text: String,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffBlockView {
    pub file_path: String,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub added: usize,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub deleted: usize,
    pub lines: Vec<DiffLineView>,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrorCardView {
    pub headline: String,
    pub detail: String,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptBlock {
    UserMessage {
        text: String,
        queued: bool,
    },
    AgentMessage {
        paragraphs: Vec<String>,
    },
    ToolGroup {
        steps: Vec<ToolStepRow>,
    },
    Diff {
        block: DiffBlockView,
    },
    StatusLine {
        running: bool,
        text: String,
    },
    ErrorCard {
        card: ErrorCardView,
    },
    /// A spec interview answer recorded as a decision (F3).
    Decision {
        question: String,
        key: String,
        value: String,
    },
    /// A line Velocity delivered to the agent (an F14 "via AIRC" message or event, or a
    /// change follow-through notice), shown once the agent read it. It is never a user
    /// message and never sits in the composer queue (#231).
    Notice {
        text: String,
    },
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptModeView {
    FullTranscript,
    CollapsedWithLandHistory,
    ExpandedWithLandHistory,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub enum RosenChangeStage {
    Editing,
    Named,
    Landing,
    Landed,
    Green,
    Reopened,
    Conflict,
    Abandoned,
}

impl RosenChangeStage {
    pub fn label(self) -> &'static str {
        match self {
            Self::Editing => "Editing",
            Self::Named => "Named",
            Self::Landing => "Landing",
            Self::Landed => "Landed",
            Self::Green => "Green",
            Self::Reopened => "Reopened",
            Self::Conflict => "Conflict",
            Self::Abandoned => "Abandoned",
        }
    }

    pub fn finished(self) -> bool {
        matches!(self, Self::Landed | Self::Green | Self::Abandoned)
    }
}

/// Status of a conflict resolution handoff shown on the Stored conflicts card (F6, #522).
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct RosenResolutionView {
    pub pending: bool,
    pub status: String,
    #[serde(default)]
    pub offer_abandon: bool,
}

/// The change's server state and retained evidence for each of its lands (F6).
#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct RosenChangeView {
    pub identifier: String,
    pub stage: RosenChangeStage,
    pub name: Option<String>,
    pub conflicts: Vec<String>,
    pub lands: Vec<RosenLandView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution: Option<RosenResolutionView>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up_error: Option<String>,
}

impl RosenChangeView {
    pub fn new(identifier: String, stage: RosenChangeStage) -> Self {
        Self {
            identifier,
            stage,
            name: None,
            conflicts: Vec::new(),
            lands: Vec::new(),
            resolution: None,
            follow_up_error: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct RosenLandView {
    pub commit: String,
    pub summary: String,
    pub change: String,
    pub receipt: Option<String>,
    pub approvers: Vec<String>,
    pub build_passed: bool,
    pub checks_passed: bool,
    pub revert: Option<String>,
    pub failures: Vec<RosenLandFailureView>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct RosenLandFailureView {
    pub label: String,
    pub output: String,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkedSummaryView {
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub tool_calls: usize,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub files_touched: usize,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub added: usize,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub deleted: usize,
    pub proved: bool,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct ThreadDetailSnapshot {
    pub account_label: Option<String>,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub identifier: usize,
    pub title: String,
    pub titling: bool,
    pub title_model_name: String,
    pub change_label: String,
    pub workspace_path: String,
    pub workspace_short_path: String,
    pub workspace_chip: WorkspaceChipView,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub added: usize,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub deleted: usize,
    pub elapsed_label: String,
    pub has_diff_action: bool,
    pub has_editor_action: bool,
    pub agent_busy: bool,
    /// Read-only question thread: no workspace, change, claim or landable status (#533).
    pub read_only: bool,
    pub no_change_reason: Option<String>,
    pub show_work: bool,
    pub transcript_mode: TranscriptModeView,
    pub worked_summary: Option<WorkedSummaryView>,
    pub rosen_change: Option<RosenChangeView>,
    /// Monotonic revision of `blocks` for delta synchronization (#553).
    #[cfg_attr(feature = "bindings", serde(default))]
    pub transcript_revision: u32,
    pub blocks: SharedBlocks,
    pub agent_commands: Vec<AgentCommandView>,
    pub composer_placeholder: String,
    pub model_pill: ModelPillView,
    /// Present on spec interview threads (F3).
    pub interview: Option<InterviewView>,
    /// The live session's subagents, oldest first (§3.2 Subagents, F4).
    pub subagents: Vec<SubagentView>,
}

/// Where a subagent is (§3.2 Subagents).
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubagentStatus {
    Running,
    Done,
    Failed,
}

/// One subagent a thread's agent started, with its own transcript (§3.2 Subagents, F4).
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubagentView {
    /// The parent's tool call that spawned it.
    pub identifier: String,
    /// The task description the parent gave it.
    pub title: String,
    /// The model the parent asked for; `None` runs the thread's own model.
    pub model: Option<String>,
    pub status: SubagentStatus,
    /// Unix seconds when Velocity saw it start.
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub started_at: usize,
    /// Unix seconds when Velocity saw it finish (`Done` or `Failed`); `None` while running.
    #[cfg_attr(
        feature = "bindings",
        serde(default, with = "crate::ipc_number::optional")
    )]
    #[cfg_attr(feature = "bindings", specta(type = Option<String>))]
    pub ended_at: Option<usize>,
    /// Total tokens, when the agent reports them.
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number::optional"))]
    #[cfg_attr(feature = "bindings", specta(type = Option<String>))]
    pub tokens: Option<usize>,
    /// How many tool steps it made.
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub step_count: usize,
    /// Its newest tool step, as "Verb · argument".
    pub current_step: Option<String>,
    /// Its messages, tool steps and diffs, in order.
    pub blocks: Vec<TranscriptBlock>,
}

/// A thread's transcript, shared rather than copied: the saved record, the layout's thread
/// entry, the live view and each snapshot all hold the same blocks, and a long transcript
/// was copied three or four times on every agent burst (#319).
pub type SharedBlocks = std::sync::Arc<Vec<TranscriptBlock>>;

/// One incremental update to a thread's transcript blocks (#553).
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptPatch {
    /// Appends streamed text to the last paragraph of an `AgentMessage` at `index`.
    AppendAgentText {
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        index: usize,
        delta: String,
    },
    /// Replaces one existing block in place without changing transcript length.
    UpdateBlock {
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        index: usize,
        block: TranscriptBlock,
    },
    /// Replaces `blocks[start..]` with `blocks` (appends, updates tail, or truncates).
    ReplaceTail {
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        start: usize,
        blocks: Vec<TranscriptBlock>,
    },
}

impl TranscriptPatch {
    /// Shifts indices forward by `offset` (for prepended earlier work-session history).
    pub fn shifted(&self, offset: usize) -> Self {
        if offset == 0 {
            return self.clone();
        }
        match self {
            Self::AppendAgentText { index, delta } => Self::AppendAgentText {
                index: index.saturating_add(offset),
                delta: delta.clone(),
            },
            Self::UpdateBlock { index, block } => Self::UpdateBlock {
                index: index.saturating_add(offset),
                block: block.clone(),
            },
            Self::ReplaceTail { start, blocks } => Self::ReplaceTail {
                start: start.saturating_add(offset),
                blocks: blocks.clone(),
            },
        }
    }
}

/// Applies ordered transcript patches to `blocks`. Returns `false` if any patch index does
/// not match the current block list (indicating the caller should resync from a full copy).
pub fn apply_transcript_patches(
    blocks: &mut Vec<TranscriptBlock>,
    patches: &[TranscriptPatch],
) -> bool {
    for patch in patches {
        match patch {
            TranscriptPatch::AppendAgentText { index, delta } => {
                let Some(TranscriptBlock::AgentMessage { paragraphs }) = blocks.get_mut(*index)
                else {
                    return false;
                };
                if let Some(last) = paragraphs.last_mut() {
                    last.push_str(delta);
                } else {
                    paragraphs.push(delta.clone());
                }
            }
            TranscriptPatch::UpdateBlock { index, block } => {
                let Some(slot) = blocks.get_mut(*index) else {
                    return false;
                };
                *slot = block.clone();
            }
            TranscriptPatch::ReplaceTail {
                start,
                blocks: tail,
            } => {
                if *start > blocks.len() {
                    return false;
                }
                blocks.truncate(*start);
                blocks.extend(tail.iter().cloned());
            }
        }
    }
    true
}

/// Transcript payload inside a layout delta (#553): either a full replacement (on thread
/// switch or resync) or incremental patches from `base_revision` to `revision`.
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TranscriptUpdate {
    Full {
        revision: u32,
        blocks: SharedBlocks,
    },
    Patches {
        base_revision: u32,
        revision: u32,
        patches: Vec<TranscriptPatch>,
    },
}

/// Every field of [`ThreadDetailSnapshot`] except `blocks` and `transcript_revision`, so
/// header/status changes cross IPC without re-serializing a multi-megabyte transcript (#553).
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreadDetailMeta {
    pub account_label: Option<String>,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub identifier: usize,
    pub title: String,
    pub titling: bool,
    pub title_model_name: String,
    pub change_label: String,
    pub workspace_path: String,
    pub workspace_short_path: String,
    pub workspace_chip: WorkspaceChipView,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub added: usize,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub deleted: usize,
    pub elapsed_label: String,
    pub has_diff_action: bool,
    pub has_editor_action: bool,
    pub agent_busy: bool,
    pub read_only: bool,
    pub no_change_reason: Option<String>,
    pub show_work: bool,
    pub transcript_mode: TranscriptModeView,
    pub worked_summary: Option<WorkedSummaryView>,
    pub rosen_change: Option<RosenChangeView>,
    pub agent_commands: Vec<AgentCommandView>,
    pub composer_placeholder: String,
    pub model_pill: ModelPillView,
    pub interview: Option<InterviewView>,
    pub subagents: Vec<SubagentView>,
}

impl ThreadDetailMeta {
    pub fn from_snapshot(snapshot: &ThreadDetailSnapshot) -> Self {
        Self {
            account_label: snapshot.account_label.clone(),
            identifier: snapshot.identifier,
            title: snapshot.title.clone(),
            titling: snapshot.titling,
            title_model_name: snapshot.title_model_name.clone(),
            change_label: snapshot.change_label.clone(),
            workspace_path: snapshot.workspace_path.clone(),
            workspace_short_path: snapshot.workspace_short_path.clone(),
            workspace_chip: snapshot.workspace_chip,
            added: snapshot.added,
            deleted: snapshot.deleted,
            elapsed_label: snapshot.elapsed_label.clone(),
            has_diff_action: snapshot.has_diff_action,
            has_editor_action: snapshot.has_editor_action,
            agent_busy: snapshot.agent_busy,
            read_only: snapshot.read_only,
            no_change_reason: snapshot.no_change_reason.clone(),
            show_work: snapshot.show_work,
            transcript_mode: snapshot.transcript_mode,
            worked_summary: snapshot.worked_summary.clone(),
            rosen_change: snapshot.rosen_change.clone(),
            agent_commands: snapshot.agent_commands.clone(),
            composer_placeholder: snapshot.composer_placeholder.clone(),
            model_pill: snapshot.model_pill.clone(),
            interview: snapshot.interview.clone(),
            subagents: snapshot.subagents.clone(),
        }
    }
}

/// What a running agent session adds to a thread on top of `state.json`: the transcript
/// (which is not persisted) and the agent's advertised slash commands (`SPEC.md §3.2`, F4).
#[derive(Clone, Debug, Default)]
pub struct LiveThreadView {
    pub blocks: SharedBlocks,
    /// `None` until the agent's first `available_commands_update`; an empty list withdraws them.
    pub agent_commands: Option<Vec<AgentCommandView>>,
    /// The title model's name while it is naming the thread (F4): the sidebar title pulses
    /// and the header shows "titling · <model>". Titling never outlives the session that
    /// started it, so it is live state, not saved.
    pub titling_model: Option<String>,
    /// The session's subagents, each with its own transcript (§3.2 Subagents).
    pub subagents: Vec<SubagentView>,
}

impl LiveThreadView {
    pub fn new(
        blocks: impl Into<SharedBlocks>,
        agent_commands: Option<Vec<AgentCommandView>>,
        titling_model: Option<String>,
    ) -> Self {
        LiveThreadView {
            blocks: blocks.into(),
            agent_commands,
            titling_model,
            subagents: Vec::new(),
        }
    }

    /// The same view with the session's subagents.
    pub fn with_subagents(mut self, subagents: Vec<SubagentView>) -> Self {
        self.subagents = subagents;
        self
    }

    /// How many of the session's subagents are currently running (F4).
    pub fn running_subagents(&self) -> usize {
        self.subagents
            .iter()
            .filter(|subagent| subagent.status == SubagentStatus::Running)
            .count()
    }
}

/// A process session's tool steps, with a stable identity across watcher ticks.
/// A resumed PR session starts at step zero without replacing an earlier fix's card.
#[derive(Clone, Debug)]
pub struct SessionWork {
    pub session_identifier: String,
    pub follows_pull_request: bool,
    pub steps: Vec<ToolStepRow>,
}

impl SessionWork {
    pub fn new(
        session_identifier: String,
        follows_pull_request: bool,
        steps: Vec<ToolStepRow>,
    ) -> Self {
        Self {
            session_identifier,
            follows_pull_request,
            steps,
        }
    }
}

#[derive(Clone)]
pub struct ThreadDetailEntry {
    pub account_label: Option<String>,
    pub usage_account: Option<String>,
    pub identifier: usize,
    pub title: String,
    pub titling: bool,
    pub title_model_name: String,
    pub branch: String,
    pub worktree_path: String,
    pub lifecycle: ThreadLifecycle,
    pub added: usize,
    pub deleted: usize,
    pub elapsed_label: String,
    pub pull_request_number: Option<usize>,
    pub agent_busy: bool,
    /// Read-only question thread: no workspace, change, claim or landable status (#533).
    pub read_only: bool,
    pub no_change_reason: Option<String>,
    pub show_work: bool,
    pub worked_summary: Option<WorkedSummaryView>,
    pub rosen_change: Option<RosenChangeView>,
    pub blocks: SharedBlocks,
    pub previous_work_thread: Option<usize>,
    pub agent_commands: Vec<AgentCommandView>,
    pub provider: ProviderColor,
    pub model_name: String,
    pub effort_label: String,
    pub requested_fast: bool,
    pub model_has_fast_mode: bool,
}

/// A work item boundary shown in the transcript using the existing status row.
pub fn work_session_boundary() -> TranscriptBlock {
    TranscriptBlock::StatusLine {
        running: false,
        text: "New agent session".into(),
    }
}

impl ThreadDetailEntry {
    /// An idle thread with an empty transcript and no agent commands.
    pub fn new(
        identifier: usize,
        title: String,
        branch: String,
        worktree_path: String,
        lifecycle: ThreadLifecycle,
        provider: ProviderColor,
    ) -> Self {
        Self {
            identifier,
            title,
            titling: false,
            title_model_name: String::new(),
            account_label: None,
            usage_account: None,
            branch,
            worktree_path,
            lifecycle,
            added: 0,
            deleted: 0,
            elapsed_label: String::new(),
            pull_request_number: None,
            agent_busy: false,
            read_only: false,
            no_change_reason: None,
            show_work: false,
            worked_summary: None,
            rosen_change: None,
            blocks: SharedBlocks::default(),
            previous_work_thread: None,
            agent_commands: Vec::new(),
            provider,
            model_name: String::new(),
            effort_label: String::new(),
            requested_fast: false,
            model_has_fast_mode: false,
        }
    }
}

/// Classifies a composer string into the verified `SlashPrefix` domain.
pub fn parse_slash_prefix(input: &str) -> SlashPrefix {
    if !input.starts_with('/') || input.contains(char::is_whitespace) {
        return SlashPrefix::None;
    }
    if input == "/" {
        SlashPrefix::All
    } else if "/task".starts_with(input) {
        SlashPrefix::Task
    } else if "/fanout".starts_with(input) {
        SlashPrefix::Fanout
    } else if "/review".starts_with(input) {
        SlashPrefix::Review
    } else if "/compact".starts_with(input) {
        SlashPrefix::Compact
    } else if "/model".starts_with(input) {
        SlashPrefix::Model
    } else {
        SlashPrefix::None
    }
}

/// Maps a verified `SlashCommand` to its `(command, description)` pair from `SPEC.md §4 F4`.
pub fn slash_command_item(command: SlashCommand) -> SlashCommandItem {
    let (cmd, description) = match command {
        SlashCommand::Task => ("/task", "add a task to the Task List"),
        SlashCommand::Fanout3 => ("/fanout 3", "run on 3 models in parallel"),
        SlashCommand::Review => ("/review", "review the diff"),
        SlashCommand::Compact => ("/compact", "summarise context"),
        SlashCommand::Model => ("/model", "change model"),
    };
    SlashCommandItem {
        command: cmd.into(),
        description: description.into(),
    }
}

/// Projects the visible slash menu items for the current composer text.
pub fn slash_menu_items(composer_text: &str) -> Vec<SlashCommandItem> {
    slash_menu_items_with_agent(composer_text, &[])
}

/// Normalizes an advertised agent command name into `/<name>`.
fn normalize_agent_command(name: &str) -> Option<String> {
    let trimmed = name.trim().trim_start_matches('/');
    if trimmed.is_empty() {
        None
    } else {
        Some(format!("/{trimmed}"))
    }
}

/// Projects the visible slash menu items for the current composer text, appending the
/// agent's advertised commands (`available_commands_update`, `SPEC.md §3.2`, `F4`) after
/// Velocity's fixed commands when filtered by the same prefix.
pub fn slash_menu_items_with_agent(
    composer_text: &str,
    agent_commands: &[AgentCommandView],
) -> Vec<SlashCommandItem> {
    if !composer_text.starts_with('/') || composer_text.contains(char::is_whitespace) {
        return Vec::new();
    }
    let mut items: Vec<SlashCommandItem> =
        thread_view::filter_slash_commands(parse_slash_prefix(composer_text))
            .into_iter()
            .map(slash_command_item)
            .collect();
    for command in agent_commands {
        let Some(formatted) = normalize_agent_command(&command.name) else {
            continue;
        };
        if !formatted.starts_with(composer_text) {
            continue;
        }
        if items.iter().any(|existing| existing.command == formatted) {
            continue;
        }
        items.push(SlashCommandItem {
            command: formatted,
            description: command.description.clone(),
        });
    }
    items
}

/// Shortens a worktree path to match `wtChip` in `design/main-window.html`.
pub fn shorten_worktree_path(path: &str) -> String {
    if path.chars().count() <= 34 {
        return path.to_owned();
    }
    let prefix: String = path.chars().take(3).collect();
    let segments: Vec<&str> = path.split(['\\', '/']).collect();
    if segments.len() >= 2 {
        let tail = segments[segments.len() - 2..].join("\\");
        format!("{prefix}…\\{tail}")
    } else {
        path.to_owned()
    }
}

/// Converts a unified diff string into a `DiffBlockView` with verified `+added` and `-deleted` counts.
pub fn build_diff_block(file_path: String, raw_lines: Vec<String>) -> DiffBlockView {
    let mut kinds = Vec::with_capacity(raw_lines.len());
    let mut lines = Vec::with_capacity(raw_lines.len());
    for text in raw_lines {
        let (kind, role) = if text.starts_with("@@") {
            (DiffLineKind::HunkHeader, DiffLineRole::HunkHeader)
        } else if text.starts_with('+') {
            (DiffLineKind::Added, DiffLineRole::Added)
        } else if text.starts_with('-') {
            (DiffLineKind::Deleted, DiffLineRole::Deleted)
        } else {
            (DiffLineKind::Context, DiffLineRole::Context)
        };
        kinds.push(kind);
        lines.push(DiffLineView { role, text });
    }
    let DiffCounts { added, deleted } = thread_view::summarize_diff(&kinds);
    DiffBlockView {
        file_path,
        added,
        deleted,
        lines,
    }
}

/// Builds the initial worktree tool step group when a thread starts (`SPEC.md §4 F5`).
pub fn initial_worktree_step_group(worktree_path: &str, base_branch: &str) -> Vec<ToolStepRow> {
    thread_view::initial_transcript_steps(!worktree_path.is_empty())
        .into_iter()
        .map(|kind| match kind {
            ToolStepKind::WorktreeCreated => ToolStepRow {
                running: matches!(ToolStepStatus::Completed, ToolStepStatus::Running),
                verb: "Workspace".into(),
                argument: worktree_path.into(),
                result_text: format!("created from {base_branch}"),
                tool_call_id: None,
            },
            ToolStepKind::SkillLoaded => ToolStepRow {
                running: false,
                verb: "Skill".into(),
                argument: String::new(),
                result_text: String::new(),
                tool_call_id: None,
            },
            ToolStepKind::ToolCall => ToolStepRow {
                running: false,
                verb: "Tool".into(),
                argument: String::new(),
                result_text: String::new(),
                tool_call_id: None,
            },
        })
        .collect()
}

/// Projects a `ThreadDetailEntry` into a serializable `ThreadDetailSnapshot`.
pub fn project_thread_detail(entry: &ThreadDetailEntry) -> ThreadDetailSnapshot {
    let (chip, mode) = if entry.read_only {
        (
            WorkspaceChipView::Hidden,
            TranscriptModeView::FullTranscript,
        )
    } else {
        thread_presentation(entry)
    };
    let actions = thread_view::header_actions(entry.pull_request_number);
    let pill = thread_view::project_model_pill(entry.requested_fast, entry.model_has_fast_mode);
    let placeholder = if entry.read_only {
        "Ask a follow-up question (read-only)".into()
    } else {
        match (entry.pull_request_number, entry.lifecycle) {
            (Some(_), ThreadLifecycle::Merged) => "Thread finished · change landed".into(),
            (Some(_), ThreadLifecycle::Closed) => {
                "change closed · tell the agent to reopen it, or carry on in this workspace".into()
            }
            (Some(pr), _) => format!("Steer while it watches change #{pr}"),
            (None, _)
                if entry
                    .rosen_change
                    .as_ref()
                    .is_some_and(|change| change.stage == RosenChangeStage::Reopened) =>
            {
                "Steer the reopened thread".into()
            }
            (None, _) => "Steer, queue a follow-up, or / for commands".into(),
        }
    };
    ThreadDetailSnapshot {
        identifier: entry.identifier,
        title: entry.title.clone(),
        titling: entry.titling,
        title_model_name: entry.title_model_name.clone(),
        account_label: entry.account_label.clone(),
        change_label: entry.branch.clone(),
        workspace_path: entry.worktree_path.clone(),
        workspace_short_path: shorten_worktree_path(&entry.worktree_path),
        workspace_chip: chip,
        added: entry.added,
        deleted: entry.deleted,
        elapsed_label: entry.elapsed_label.clone(),
        has_diff_action: !entry.read_only && actions.has_diff_action,
        has_editor_action: !entry.read_only && actions.has_editor_action,
        agent_busy: entry.agent_busy,
        read_only: entry.read_only,
        no_change_reason: entry.no_change_reason.clone(),
        show_work: entry.show_work,
        transcript_mode: mode,
        worked_summary: worked_summary(entry),
        rosen_change: entry.rosen_change.clone(),
        transcript_revision: 0,
        blocks: entry.blocks.clone(),
        agent_commands: entry.agent_commands.clone(),
        composer_placeholder: placeholder,
        model_pill: ModelPillView {
            usage_account: entry.usage_account.clone(),
            provider: entry.provider,
            model_name: entry.model_name.clone(),
            effort_label: entry.effort_label.clone(),
            show_fast_badge: pill.show_fast_badge,
            speed_control_enabled: pill.speed_control_enabled,
            preserve_requested_fast: pill.preserve_requested_fast,
        },
        interview: None,
        // Subagents are live session state; the layout adds them from the live view.
        subagents: Vec::new(),
    }
}

fn thread_presentation(entry: &ThreadDetailEntry) -> (WorkspaceChipView, TranscriptModeView) {
    if let Some(change) = &entry.rosen_change {
        let chip = if entry.worktree_path.is_empty() {
            WorkspaceChipView::Hidden
        } else if change.stage.finished() {
            WorkspaceChipView::Deleted
        } else {
            WorkspaceChipView::Live
        };
        let has_history = !change.lands.is_empty();
        let mode = match (change.stage, entry.show_work, has_history) {
            (RosenChangeStage::Landed | RosenChangeStage::Green, true, true) => {
                TranscriptModeView::ExpandedWithLandHistory
            }
            (RosenChangeStage::Landed | RosenChangeStage::Green, false, true) => {
                TranscriptModeView::CollapsedWithLandHistory
            }
            _ => TranscriptModeView::FullTranscript,
        };
        return (chip, mode);
    }
    let chip = match thread_view::worktree_chip_state(!entry.branch.is_empty(), entry.lifecycle) {
        WorktreeChipState::Hidden => WorkspaceChipView::Hidden,
        WorktreeChipState::Live => WorkspaceChipView::Live,
        WorktreeChipState::Deleted => WorkspaceChipView::Deleted,
    };
    let mode =
        match thread_view::transcript_mode(entry.pull_request_number.is_some(), entry.show_work) {
            TranscriptMode::FullTranscript => TranscriptModeView::FullTranscript,
            TranscriptMode::CollapsedWithPrHistory => TranscriptModeView::CollapsedWithLandHistory,
            TranscriptMode::ExpandedWithPrHistory => TranscriptModeView::ExpandedWithLandHistory,
        };
    (chip, mode)
}

fn worked_summary(entry: &ThreadDetailEntry) -> Option<WorkedSummaryView> {
    if entry.pull_request_number.is_none()
        && !entry
            .rosen_change
            .as_ref()
            .is_some_and(|change| !change.lands.is_empty() && change.stage.finished())
    {
        return entry.worked_summary.clone();
    }
    let tool_calls = entry
        .blocks
        .iter()
        .map(|block| match block {
            TranscriptBlock::ToolGroup { steps } => steps.len(),
            _ => 0,
        })
        .sum();
    let files_touched = entry
        .blocks
        .iter()
        .filter_map(|block| match block {
            TranscriptBlock::Diff { block } => Some(block.file_path.as_str()),
            _ => None,
        })
        .collect::<std::collections::BTreeSet<_>>()
        .len();
    Some(WorkedSummaryView {
        tool_calls,
        files_touched,
        added: entry.added,
        deleted: entry.deleted,
        proved: false,
    })
}

/// Evaluates composer submission using the verified `submit_composer` and `queued_notice_visible` rules.
pub fn evaluate_composer_submission(
    text: &str,
    is_draft: bool,
    agent_busy: bool,
) -> (ComposerSubmitEffect, Option<String>, bool) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        let effect =
            thread_view::submit_composer(ComposerSubmissionKind::Empty, is_draft, agent_busy);
        return (effect, None, thread_view::queued_notice_visible(effect));
    }
    if let Some(rest) = trimmed.strip_prefix("/task") {
        let task_title = rest.trim();
        if !task_title.is_empty() {
            let effect = thread_view::submit_composer(
                ComposerSubmissionKind::SlashTask,
                is_draft,
                agent_busy,
            );
            return (
                effect,
                Some(task_title.to_owned()),
                thread_view::queued_notice_visible(effect),
            );
        }
    }
    let effect =
        thread_view::submit_composer(ComposerSubmissionKind::Message, is_draft, agent_busy);
    (
        effect,
        Some(trimmed.to_owned()),
        thread_view::queued_notice_visible(effect),
    )
}

/// Maps a composer key press through the verified `handle_composer_key` rule.
pub fn map_composer_key(key: ComposerKey, slash_menu_open: bool) -> ComposerKeyAction {
    thread_view::handle_composer_key(key, slash_menu_open)
}
