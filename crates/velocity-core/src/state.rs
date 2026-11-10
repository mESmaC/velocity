//! Pure persisted-state domain types and transitions (`SPEC.md` §2.1, §2.2, §3, F3, F5, F8, F13, F15, §7 M1).

/// Thread lifecycle states (`SPEC.md` §2.1).
/// Rule 1: there is no "waiting for user" or approval-gate state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadState {
    Draft,
    Spec,
    Working,
    Review,
    Error,
    Closed,
    Done,
    Merged,
}

/// Agent activity states (`SPEC.md` §2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentActivity {
    Thinking,
    Executing,
    Parked,
    Stalled,
    AtLimit,
    Idle,
}

/// Task List states (`SPEC.md` F8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    Draft,
    Todo,
    Now,
    Done,
}

/// Who added a task (`SPEC.md` F8).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskAttribution {
    Spec,
    User,
    Thread(u32),
}

/// OAuth provider kinds (`SPEC.md` §3, F15).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderKind {
    Claude,
    Codex,
    Antigravity,
    Kimi,
}

/// Agent effort levels (`SPEC.md` F15).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffortLevel {
    Low,
    Medium,
    High,
    Max,
}

/// Speed modes (`SPEC.md` F15).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpeedMode {
    Regular,
    Fast,
}

/// Project source kind (`SPEC.md` F2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProjectLocationKind {
    Local,
    GitHub,
}

/// Pull request review stages (`SPEC.md` §2.1, F7).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewStage {
    ContinuousIntegration,
    Review,
    Fixing,
    Approved,
}

/// Worktree settings (`SPEC.md` F5, F13), generic over path representation `PathValue`.
pub struct WorktreeSettings<PathValue> {
    pub location_root: Option<PathValue>,
    pub delete_local_branch_on_merge: bool,
    pub share_rust_build_cache: bool,
    pub warm_pool_count: u32,
}

/// Provider settings (`SPEC.md` F13, F15), generic over model representation `ModelValue`.
/// Contains no API key fields (`SPEC.md` §1, F13, F15: OAuth only).
pub struct ProviderSettings<ModelValue> {
    pub default_provider: ProviderKind,
    pub default_model: Option<ModelValue>,
    pub default_effort: EffortLevel,
    pub default_speed: SpeedMode,
    /// The title model's provider, whose agent is asked for thread titles (F4, F15).
    pub title_provider: ProviderKind,
    pub title_model: ModelValue,
    pub title_effort: EffortLevel,
    pub title_speed: SpeedMode,
}

/// Agent runtime caps and timeouts (`SPEC.md` §2.2, F13).
pub struct AgentSettings {
    pub max_turns: Option<u32>,
    pub max_duration_minutes: Option<u32>,
    pub stall_timeout_minutes: u32,
    pub tool_timeout_minutes: u32,
    pub claim_release_grace_minutes: u32,
}

/// Proof and stress settings (`SPEC.md` F13, F17).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProofSettings {
    pub stress_diff_input_count: u32,
}

/// Complete persisted settings (`SPEC.md` F13), generic over `PathValue` and `ModelValue`.
pub struct Settings<PathValue, ModelValue> {
    pub worktrees: WorktreeSettings<PathValue>,
    pub providers: ProviderSettings<ModelValue>,
    pub agents: AgentSettings,
    pub proofs: ProofSettings,
}

/// Persisted Task List record (`SPEC.md` F8), generic over `TextValue`.
pub struct TaskRecord<TextValue> {
    pub id: u32,
    pub text: TextValue,
    pub task_state: TaskState,
    pub added_by: TaskAttribution,
    pub assigned_thread_id: Option<u32>,
}

/// Persisted Thread record (`SPEC.md` §2.1, §2.2, F4, F5, F15), generic over `PathValue` and `ModelValue`.
pub struct ThreadRecord<PathValue, ModelValue> {
    pub id: u32,
    pub thread_state: ThreadState,
    pub activity: AgentActivity,
    pub provider: ProviderKind,
    pub model: ModelValue,
    pub model_supports_fast: bool,
    pub effort: EffortLevel,
    pub preferred_speed: SpeedMode,
    pub effective_speed: SpeedMode,
    pub worktree_path: Option<PathValue>,
    pub pull_request_number: Option<u32>,
    pub review_stage: Option<ReviewStage>,
    pub additions: u32,
    pub deletions: u32,
}

/// Persisted Project summary (`SPEC.md` F1, F2, F3, F8).
pub struct ProjectSummary {
    pub id: u32,
    pub location_kind: ProjectLocationKind,
    pub spec_locked: bool,
    pub active_thread_id: Option<u32>,
    pub merged_today: u32,
}

/// Root persisted state header (`SPEC.md` §7 M1), generic over `PathValue` and `ModelValue`.
pub struct PersistedStateHeader<PathValue, ModelValue> {
    pub active_project_index: u32,
    pub project_count: u32,
    pub settings: Settings<PathValue, ModelValue>,
}

/// Before a project's spec is locked, every task is a draft (`SPEC.md` F3, F8).
pub fn normalize_task_state(spec_locked: bool, task_state: TaskState) -> TaskState {
    if spec_locked {
        task_state
    } else {
        TaskState::Draft
    }
}

/// Normalize a task record under its project's `spec_locked` status (`SPEC.md` F3, F8).
pub fn normalize_task_record<TextValue>(
    spec_locked: bool,
    mut task: TaskRecord<TextValue>,
) -> TaskRecord<TextValue> {
    task.task_state = normalize_task_state(spec_locked, task.task_state);
    task
}

/// Resolve the speed an agent actually runs at (`SPEC.md` F15):
/// Fast is active only when the model offers a fast mode.
pub fn resolve_effective_speed(model_supports_fast: bool, preferred: SpeedMode) -> SpeedMode {
    match (model_supports_fast, preferred) {
        (true, SpeedMode::Fast) => SpeedMode::Fast,
        _ => SpeedMode::Regular,
    }
}

/// Normalize a thread record (`SPEC.md` F5, F15):
/// resolves `effective_speed` and preserves `worktree_path` (`None` stays `None` for
/// spec-interview and merged threads; existing paths stay fixed when settings change).
pub fn normalize_thread_record<PathValue, ModelValue>(
    mut thread: ThreadRecord<PathValue, ModelValue>,
) -> ThreadRecord<PathValue, ModelValue> {
    thread.effective_speed =
        resolve_effective_speed(thread.model_supports_fast, thread.preferred_speed);
    thread
}

/// Switch a thread to a new model (`SPEC.md` F15), keeping `preferred_speed` and `worktree_path`.
pub fn switch_thread_model<PathValue, ModelValue>(
    mut thread: ThreadRecord<PathValue, ModelValue>,
    next_model: ModelValue,
    next_supports_fast: bool,
) -> ThreadRecord<PathValue, ModelValue> {
    thread.model = next_model;
    thread.model_supports_fast = next_supports_fast;
    thread.effective_speed = resolve_effective_speed(next_supports_fast, thread.preferred_speed);
    thread
}

/// Clamp `active_project_index` to a valid index (or `0` when `project_count == 0`).
pub fn normalize_active_project_index(index: u32, project_count: u32) -> u32 {
    if index < project_count { index } else { 0 }
}

/// Normalize the persisted state header (`SPEC.md` §7 M1).
pub fn normalize_persisted_state_header<PathValue, ModelValue>(
    mut header: PersistedStateHeader<PathValue, ModelValue>,
) -> PersistedStateHeader<PathValue, ModelValue> {
    header.active_project_index =
        normalize_active_project_index(header.active_project_index, header.project_count);
    header
}

/// Apply updated settings to the persisted state header (`SPEC.md` §3.1, F5, F13, F15).
pub fn apply_settings_update<PathValue, ModelValue>(
    mut header: PersistedStateHeader<PathValue, ModelValue>,
    new_settings: Settings<PathValue, ModelValue>,
) -> PersistedStateHeader<PathValue, ModelValue> {
    header.settings = new_settings;
    normalize_persisted_state_header(header)
}

/// Default settings (`SPEC.md` §2.2, F5, F13, F15, F17), parameterized by the default title
/// model (`GPT-6 Luna` in F15).
pub fn default_settings<PathValue, ModelValue>(
    default_title_model: ModelValue,
) -> Settings<PathValue, ModelValue> {
    Settings {
        worktrees: WorktreeSettings {
            location_root: None,
            delete_local_branch_on_merge: true,
            share_rust_build_cache: true,
            warm_pool_count: 4,
        },
        providers: ProviderSettings {
            default_provider: ProviderKind::Claude,
            default_model: None,
            default_effort: EffortLevel::High,
            default_speed: SpeedMode::Regular,
            title_provider: ProviderKind::Codex,
            title_model: default_title_model,
            title_effort: EffortLevel::Low,
            title_speed: SpeedMode::Fast,
        },
        agents: AgentSettings {
            max_turns: None,
            max_duration_minutes: None,
            stall_timeout_minutes: 5,
            tool_timeout_minutes: 30,
            claim_release_grace_minutes: 10,
        },
        proofs: ProofSettings {
            stress_diff_input_count: 10_000,
        },
    }
}

/// Dirty-checked snapshot decision (`SPEC.md` §7 M1): write iff the revision changed.
pub fn should_write_snapshot(disk_revision: u32, next_revision: u32) -> bool {
    next_revision != disk_revision
}

/// Next persisted revision after a dirty-checked step.
pub fn next_persisted_revision(
    disk_revision: u32,
    next_revision: u32,
    write_performed: bool,
) -> u32 {
    if write_performed {
        next_revision
    } else {
        disk_revision
    }
}

/// Execute one dirty-checked persistence step, returning `(write_now, next_disk_revision)`.
pub fn apply_dirty_checked_step(disk_revision: u32, target_revision: u32) -> (bool, u32) {
    let write_now = should_write_snapshot(disk_revision, target_revision);
    let next_disk = next_persisted_revision(disk_revision, target_revision, write_now);
    (write_now, next_disk)
}
