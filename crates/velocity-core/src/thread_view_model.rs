//! Read models, slash-menu state, and reducer helpers for the thread view (`SPEC.md §4 F4`, `F5`, `F6`, `F15`, `§7 M3`).
//!
//! All domain decisions delegate to the Aeneas-verified functions in `crate::thread_view`.

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
pub enum WorktreeChipView {
    Hidden,
    Live,
    Deleted,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
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

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct ModelPillView {
    pub provider: ProviderColor,
    pub model_name: String,
    pub effort_label: String,
    pub show_fast_badge: bool,
    pub speed_control_enabled: bool,
    pub preserve_requested_fast: bool,
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
#[derive(Clone, Debug)]
pub struct DiffLineView {
    pub role: DiffLineRole,
    pub text: String,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
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
#[derive(Clone, Debug)]
pub struct ErrorCardView {
    pub headline: String,
    pub detail: String,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub enum TranscriptBlock {
    UserMessage { text: String, queued: bool },
    AgentMessage { paragraphs: Vec<String> },
    ToolGroup { steps: Vec<ToolStepRow> },
    Diff { block: DiffBlockView },
    StatusLine { running: bool, text: String },
    ErrorCard { card: ErrorCardView },
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptModeView {
    FullTranscript,
    CollapsedWithPrHistory,
    ExpandedWithPrHistory,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PullRequestHistoryCard {
    pub identifier: String,
    pub title: String,
    pub body: String,
    pub path: Option<String>,
    pub line: Option<u32>,
    pub resolved: bool,
    pub tool: Option<ToolStepRow>,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct PullRequestTrackStep {
    pub label: String,
    pub state: String,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct PullRequestHistoryView {
    pub status: String,
    pub stage: String,
    pub track: Vec<PullRequestTrackStep>,
    pub cards: Vec<PullRequestHistoryCard>,
    pub cleanup_error: Option<String>,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct ThreadDetailSnapshot {
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub identifier: usize,
    pub title: String,
    pub titling: bool,
    pub title_model_name: String,
    pub branch: String,
    pub worktree_path: String,
    pub worktree_short_path: String,
    pub worktree_chip: WorktreeChipView,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub added: usize,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub deleted: usize,
    pub elapsed_label: String,
    pub has_diff_action: bool,
    pub has_editor_action: bool,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number::optional"))]
    #[cfg_attr(feature = "bindings", specta(type = Option<String>))]
    pub pull_request_number: Option<usize>,
    pub has_ship_pr_button: bool,
    pub agent_busy: bool,
    pub show_work: bool,
    pub transcript_mode: TranscriptModeView,
    pub worked_summary: Option<WorkedSummaryView>,
    pub pull_request_history: Option<PullRequestHistoryView>,
    pub blocks: Vec<TranscriptBlock>,
    pub agent_commands: Vec<AgentCommandView>,
    pub composer_placeholder: String,
    pub model_pill: ModelPillView,
}

#[derive(Clone)]
pub struct ThreadDetailEntry {
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
    pub show_work: bool,
    pub worked_summary: Option<WorkedSummaryView>,
    pub pull_request_history: Option<PullRequestHistoryView>,
    pub blocks: Vec<TranscriptBlock>,
    pub agent_commands: Vec<AgentCommandView>,
    pub provider: ProviderColor,
    pub model_name: String,
    pub effort_label: String,
    pub requested_fast: bool,
    pub model_has_fast_mode: bool,
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
                verb: "Worktree".into(),
                argument: worktree_path.into(),
                result_text: format!("created from {base_branch}"),
            },
            ToolStepKind::SkillLoaded => ToolStepRow {
                running: false,
                verb: "Skill".into(),
                argument: String::new(),
                result_text: String::new(),
            },
            ToolStepKind::ToolCall => ToolStepRow {
                running: false,
                verb: "Tool".into(),
                argument: String::new(),
                result_text: String::new(),
            },
        })
        .collect()
}

/// Projects a `ThreadDetailEntry` into a serializable `ThreadDetailSnapshot`.
pub fn project_thread_detail(entry: &ThreadDetailEntry) -> ThreadDetailSnapshot {
    let chip = match thread_view::worktree_chip_state(!entry.branch.is_empty(), entry.lifecycle) {
        WorktreeChipState::Hidden => WorktreeChipView::Hidden,
        WorktreeChipState::Live => WorktreeChipView::Live,
        WorktreeChipState::Deleted => WorktreeChipView::Deleted,
    };
    let actions = thread_view::header_actions(entry.pull_request_number);
    let mode =
        match thread_view::transcript_mode(entry.pull_request_number.is_some(), entry.show_work) {
            TranscriptMode::FullTranscript => TranscriptModeView::FullTranscript,
            TranscriptMode::CollapsedWithPrHistory => TranscriptModeView::CollapsedWithPrHistory,
            TranscriptMode::ExpandedWithPrHistory => TranscriptModeView::ExpandedWithPrHistory,
        };
    let pill = thread_view::project_model_pill(entry.requested_fast, entry.model_has_fast_mode);
    let placeholder = match (entry.pull_request_number, entry.lifecycle) {
        (Some(_), ThreadLifecycle::Merged) => "Thread finished · PR merged".into(),
        (Some(_), ThreadLifecycle::Closed) => {
            "PR closed · tell the agent to reopen it, or carry on in this worktree".into()
        }
        (Some(pr), _) => format!("Steer while it watches PR #{pr}"),
        (None, _) => "Steer, queue a follow-up, or / for commands".into(),
    };
    ThreadDetailSnapshot {
        identifier: entry.identifier,
        title: entry.title.clone(),
        titling: entry.titling,
        title_model_name: entry.title_model_name.clone(),
        branch: entry.branch.clone(),
        worktree_path: entry.worktree_path.clone(),
        worktree_short_path: shorten_worktree_path(&entry.worktree_path),
        worktree_chip: chip,
        added: entry.added,
        deleted: entry.deleted,
        elapsed_label: entry.elapsed_label.clone(),
        has_diff_action: actions.has_diff_action,
        has_editor_action: actions.has_editor_action,
        pull_request_number: actions.pr_number,
        has_ship_pr_button: actions.has_ship_pr_button,
        agent_busy: entry.agent_busy,
        show_work: entry.show_work,
        transcript_mode: mode,
        worked_summary: worked_summary(entry),
        pull_request_history: entry.pull_request_history.clone(),
        blocks: entry.blocks.clone(),
        agent_commands: entry.agent_commands.clone(),
        composer_placeholder: placeholder,
        model_pill: ModelPillView {
            provider: entry.provider,
            model_name: entry.model_name.clone(),
            effort_label: entry.effort_label.clone(),
            show_fast_badge: pill.show_fast_badge,
            speed_control_enabled: pill.speed_control_enabled,
            preserve_requested_fast: pill.preserve_requested_fast,
        },
    }
}

fn worked_summary(entry: &ThreadDetailEntry) -> Option<WorkedSummaryView> {
    if entry.pull_request_number.is_none() {
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
