//! Pure synchronous thread-view rules (`SPEC.md §4 F4`, `F5`, `F6`, `F15`, `§5`, `§7 M3`).
//!
//! Every function here is in the pure subset translated by Charon and Aeneas
//! (`proofs/ThreadView/Generated/`) and proved to refine `proofs/ThreadView/Design.lean`
//! in `proofs/ThreadView/Confidence.lean`.

/// Lifecycle of a thread for the header worktree chip (`SPEC.md §4 F4`, `F5`).
#[derive(Clone, Copy)]
pub enum ThreadLifecycle {
    Draft,
    Working,
    Review,
    Error,
    Closed,
    Merged,
}

/// Visual state of the header worktree chip (`SPEC.md §4 F4`, `F5`).
#[derive(Clone, Copy)]
pub enum WorktreeChipState {
    Hidden,
    Live,
    Deleted,
}

/// Worktree chip projection from `(has_branch, lifecycle)`.
pub fn worktree_chip_state(has_branch: bool, lifecycle: ThreadLifecycle) -> WorktreeChipState {
    if !has_branch {
        WorktreeChipState::Hidden
    } else {
        match lifecycle {
            ThreadLifecycle::Merged => WorktreeChipState::Deleted,
            ThreadLifecycle::Draft
            | ThreadLifecycle::Working
            | ThreadLifecycle::Review
            | ThreadLifecycle::Error
            | ThreadLifecycle::Closed => WorktreeChipState::Live,
        }
    }
}

/// Header action visibility (`SPEC.md §4 F4`).
#[derive(Clone, Copy)]
pub struct HeaderActions {
    pub has_diff_action: bool,
    pub has_editor_action: bool,
    pub pr_number: Option<usize>,
    pub has_ship_pr_button: bool,
}

/// Header action projection for a thread with optional pull request number.
pub fn header_actions(pr_number: Option<usize>) -> HeaderActions {
    HeaderActions {
        has_diff_action: true,
        has_editor_action: true,
        pr_number,
        has_ship_pr_button: false,
    }
}

/// Thread-level keyboard shortcut keys (`SPEC.md §5`).
#[derive(Clone, Copy)]
pub enum ThreadShortcutKey {
    DiffKey,
    EditorKey,
    OtherKey,
}

/// Thread-level keyboard shortcut actions (`SPEC.md §5`).
#[derive(Clone, Copy)]
pub enum ThreadShortcutAction {
    None,
    OpenDiff,
    OpenEditor,
}

/// Thread-level keyboard shortcut dispatch (`SPEC.md §5`).
pub fn dispatch_thread_shortcut(
    key: ThreadShortcutKey,
    typing: bool,
    has_modifier: bool,
) -> ThreadShortcutAction {
    if typing || has_modifier {
        ThreadShortcutAction::None
    } else {
        match key {
            ThreadShortcutKey::DiffKey => ThreadShortcutAction::OpenDiff,
            ThreadShortcutKey::EditorKey => ThreadShortcutAction::OpenEditor,
            ThreadShortcutKey::OtherKey => ThreadShortcutAction::None,
        }
    }
}

/// The five composer slash commands (`SPEC.md §4 F4`).
/// There is no `/ship` command.
#[derive(Clone, Copy)]
pub enum SlashCommand {
    Task,
    Fanout3,
    Review,
    Compact,
    Model,
}

/// Parsed slash-prefix class from the composer input (`SPEC.md §4 F4`).
#[derive(Clone, Copy)]
pub enum SlashPrefix {
    All,
    Task,
    Fanout,
    Review,
    Compact,
    Model,
    None,
}

/// Whether a slash command matches a classified prefix.
pub fn matches_slash_prefix(prefix_kind: SlashPrefix, command: SlashCommand) -> bool {
    match prefix_kind {
        SlashPrefix::All => true,
        SlashPrefix::Task => matches!(command, SlashCommand::Task),
        SlashPrefix::Fanout => matches!(command, SlashCommand::Fanout3),
        SlashPrefix::Review => matches!(command, SlashCommand::Review),
        SlashPrefix::Compact => matches!(command, SlashCommand::Compact),
        SlashPrefix::Model => matches!(command, SlashCommand::Model),
        SlashPrefix::None => false,
    }
}

/// Filtered slash commands in menu order (`/task`, `/fanout 3`, `/review`, `/compact`, `/model`).
pub fn filter_slash_commands(prefix_kind: SlashPrefix) -> Vec<SlashCommand> {
    let mut commands = Vec::new();
    match prefix_kind {
        SlashPrefix::All => {
            commands.push(SlashCommand::Task);
            commands.push(SlashCommand::Fanout3);
            commands.push(SlashCommand::Review);
            commands.push(SlashCommand::Compact);
            commands.push(SlashCommand::Model);
        }
        SlashPrefix::Task => {
            commands.push(SlashCommand::Task);
        }
        SlashPrefix::Fanout => {
            commands.push(SlashCommand::Fanout3);
        }
        SlashPrefix::Review => {
            commands.push(SlashCommand::Review);
        }
        SlashPrefix::Compact => {
            commands.push(SlashCommand::Compact);
        }
        SlashPrefix::Model => {
            commands.push(SlashCommand::Model);
        }
        SlashPrefix::None => {}
    }
    commands
}

/// Clamps the current slash-menu selection to `0..count` when `count > 0`.
pub fn clamp_slash_selection(selected: usize, count: usize) -> usize {
    if selected < count {
        selected
    } else {
        count - 1
    }
}

/// Cyclic arrow-key navigation in the slash menu (`SPEC.md §4 F4`).
pub fn move_slash_selection(selected: usize, count: usize, down: bool) -> usize {
    if count == 0 {
        0
    } else {
        let clamped = clamp_slash_selection(selected, count);
        if down {
            if clamped + 1 < count { clamped + 1 } else { 0 }
        } else if clamped == 0 {
            count - 1
        } else {
            clamped - 1
        }
    }
}

/// Keys handled specially by the composer (`SPEC.md §4 F4`).
#[derive(Clone, Copy)]
pub enum ComposerKey {
    Enter,
    ShiftEnter,
    Tab,
    ArrowDown,
    ArrowUp,
    Escape,
}

/// Actions taken by the composer on keydown (`SPEC.md §4 F4`).
#[derive(Clone, Copy)]
pub enum ComposerKeyAction {
    Submit,
    InsertNewline,
    CompleteSlash,
    MoveSlashDown,
    MoveSlashUp,
    LeaveComposer,
    Ignore,
}

/// Composer keybinding rule (`SPEC.md §4 F4`).
pub fn handle_composer_key(key: ComposerKey, slash_menu_open: bool) -> ComposerKeyAction {
    match key {
        ComposerKey::ArrowDown => {
            if slash_menu_open {
                ComposerKeyAction::MoveSlashDown
            } else {
                ComposerKeyAction::Ignore
            }
        }
        ComposerKey::ArrowUp => {
            if slash_menu_open {
                ComposerKeyAction::MoveSlashUp
            } else {
                ComposerKeyAction::Ignore
            }
        }
        ComposerKey::Tab => {
            if slash_menu_open {
                ComposerKeyAction::CompleteSlash
            } else {
                ComposerKeyAction::Ignore
            }
        }
        ComposerKey::Enter => {
            if slash_menu_open {
                ComposerKeyAction::CompleteSlash
            } else {
                ComposerKeyAction::Submit
            }
        }
        ComposerKey::ShiftEnter => ComposerKeyAction::InsertNewline,
        ComposerKey::Escape => ComposerKeyAction::LeaveComposer,
    }
}

/// Classification of composer input on submission (`SPEC.md §4 F4`).
#[derive(Clone, Copy)]
pub enum ComposerSubmissionKind {
    Empty,
    SlashTask,
    Message,
}

/// Effect of submitting the composer (`SPEC.md §1 Rule 1`, `§4 F4`).
#[derive(Clone, Copy)]
pub enum ComposerSubmitEffect {
    Nothing,
    AddTask,
    StartThreadFromDraft,
    SendPromptNow,
    QueueForNextStep,
}

/// Composer submission rule (`SPEC.md §1 Rule 1`, `§4 F4`).
pub fn submit_composer(
    kind: ComposerSubmissionKind,
    is_draft: bool,
    agent_busy: bool,
) -> ComposerSubmitEffect {
    match kind {
        ComposerSubmissionKind::Empty => ComposerSubmitEffect::Nothing,
        ComposerSubmissionKind::SlashTask => ComposerSubmitEffect::AddTask,
        ComposerSubmissionKind::Message => {
            if is_draft {
                ComposerSubmitEffect::StartThreadFromDraft
            } else if agent_busy {
                ComposerSubmitEffect::QueueForNextStep
            } else {
                ComposerSubmitEffect::SendPromptNow
            }
        }
    }
}

/// Whether the queued-message notice ("queued · sends when the current step finishes") is shown.
pub fn queued_notice_visible(effect: ComposerSubmitEffect) -> bool {
    matches!(effect, ComposerSubmitEffect::QueueForNextStep)
}

/// Model pill state in the composer (`SPEC.md §4 F4`, `F15`).
#[derive(Clone, Copy)]
pub struct ModelPillState {
    pub show_fast_badge: bool,
    pub speed_control_enabled: bool,
    pub preserve_requested_fast: bool,
}

/// Projects the composer model pill from `(requested_fast, model_has_fast_mode)`.
pub fn project_model_pill(requested_fast: bool, model_has_fast_mode: bool) -> ModelPillState {
    ModelPillState {
        show_fast_badge: requested_fast && model_has_fast_mode,
        speed_control_enabled: model_has_fast_mode,
        preserve_requested_fast: requested_fast,
    }
}

/// Status of a transcript tool step (`SPEC.md §4 F4`).
#[derive(Clone, Copy)]
pub enum ToolStepStatus {
    Running,
    Completed,
}

/// Transcript tool step categories (`SPEC.md §4 F4`, `F5`).
#[derive(Clone, Copy)]
pub enum ToolStepKind {
    WorktreeCreated,
    SkillLoaded,
    ToolCall,
}

/// Initial transcript steps when a thread starts (`SPEC.md §4 F5`).
pub fn initial_transcript_steps(has_worktree: bool) -> Vec<ToolStepKind> {
    let mut steps = Vec::new();
    if has_worktree {
        steps.push(ToolStepKind::WorktreeCreated);
    }
    steps
}

/// Classification of a single unified diff line (`SPEC.md §4 F4`, `§7 M3`).
#[derive(Clone, Copy)]
pub enum DiffLineKind {
    Added,
    Deleted,
    Context,
    HunkHeader,
}

/// Added and deleted line counts for a diff block (`SPEC.md §4 F4`, `§7 M3`).
#[derive(Clone, Copy)]
pub struct DiffCounts {
    pub added: usize,
    pub deleted: usize,
}

/// Updates diff line counts with one classified diff line.
pub fn step_diff_counts(counts: DiffCounts, line: DiffLineKind) -> DiffCounts {
    match line {
        DiffLineKind::Added => DiffCounts {
            added: counts.added + 1,
            deleted: counts.deleted,
        },
        DiffLineKind::Deleted => DiffCounts {
            added: counts.added,
            deleted: counts.deleted + 1,
        },
        DiffLineKind::Context | DiffLineKind::HunkHeader => counts,
    }
}

/// Single-pass computation of `+added` and `-deleted` counts across a diff (`SPEC.md §4 F4`).
pub fn summarize_diff(lines: &[DiffLineKind]) -> DiffCounts {
    let mut counts = DiffCounts {
        added: 0,
        deleted: 0,
    };
    let mut index = 0;
    while index < lines.len() {
        counts = step_diff_counts(counts, lines[index]);
        index += 1;
    }
    counts
}

/// Transcript rendering mode (`SPEC.md §4 F4`, `F6`).
#[derive(Clone, Copy)]
pub enum TranscriptMode {
    FullTranscript,
    CollapsedWithPrHistory,
    ExpandedWithPrHistory,
}

/// Projects the transcript rendering mode from `(has_pr, show_work)`.
pub fn transcript_mode(has_pr: bool, show_work: bool) -> TranscriptMode {
    if !has_pr {
        TranscriptMode::FullTranscript
    } else if show_work {
        TranscriptMode::ExpandedWithPrHistory
    } else {
        TranscriptMode::CollapsedWithPrHistory
    }
}
