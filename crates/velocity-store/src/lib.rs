//! JSON persistence store (`state.json`) for projects, threads, tasks, and settings.
//!
//! Pure state invariants and dirty-check logic live in `velocity_core::state`
//! (proven in `proofs/State/Design.lean` and `proofs/State/Confidence.lean`),
//! instantiated directly with `String` for text, paths, and model names.
//! This crate owns atomic filesystem reads/writes, corrupt-snapshot quarantine,
//! and serde/specta serialization.

pub mod agent_activity;
pub mod agent_tasks;
pub mod airc_members;
pub mod build_gate;
pub mod claim_piece;
pub mod follow_through;

use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub mod interview;
pub mod issues;
mod new_project;

pub use interview::{
    InterviewDecision, InterviewOption, InterviewQuestion, InterviewRecord, InterviewStage,
    ProofListEntry,
};
pub use new_project::NewProjectRecord;
use velocity_core::state::{
    self as core_state, AgentActivity as CoreAgentActivity, AgentSettings as CoreAgentSettings,
    EffortLevel as CoreEffortLevel, PersistedStateHeader, ProofSettings as CoreProofSettings,
    ProviderKind as CoreProviderKind, ProviderSettings as CoreProviderSettings,
    ReviewStage as CoreReviewStage, Settings as CoreSettings, SpeedMode as CoreSpeedMode,
    TaskAttribution as CoreTaskAttribution, TaskRecord as CoreTaskRecord,
    TaskState as CoreTaskState, ThreadRecord as CoreThreadRecord, ThreadState as CoreThreadState,
    WorktreeSettings as CoreWorktreeSettings, apply_dirty_checked_step,
};

pub mod duty;
pub mod goal_swarm_dialog;
pub mod goal_swarm_view;
pub mod goal_swarms;
mod project_layout;
pub mod projects;
pub mod provider_views;
pub mod providers;
pub mod swarm_dialog;
pub mod swarms;
pub mod task_list;
pub mod threads;

pub use duty::DutyRoleItem;

pub const STATE_FILE_NAME: &str = "state.json";
pub mod worktree_settings;
pub const PARTIAL_STATE_FILE_NAME: &str = "state.json.partial";
pub const DEFAULT_TITLE_MODEL_NAME: &str = "GPT-6 Luna";

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("Agent timeouts and caps must be positive; use off for unlimited caps")]
    AgentSetting,
    #[error("Invalid worktree setting: {0}")]
    WorktreeSetting(String),
    #[error("Could not create state directory '{path}': {source}")]
    CreateDirectory {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Could not read state file '{path}': {source}")]
    ReadSnapshot {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Could not move corrupt state file '{destination}' aside to '{corrupt}': {source}")]
    QuarantineCorruptSnapshot {
        corrupt: String,
        destination: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Could not serialize state snapshot: {source}")]
    SerializeSnapshot {
        #[source]
        source: serde_json::Error,
    },
    #[error("Could not write temporary state file '{path}': {source}")]
    WritePartial {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Could not flush temporary state file '{path}': {source}")]
    SyncPartial {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Could not replace state file '{destination}' from '{partial}': {source}")]
    ReplaceSnapshot {
        partial: String,
        destination: String,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
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

impl From<ThreadState> for CoreThreadState {
    fn from(value: ThreadState) -> Self {
        match value {
            ThreadState::Draft => Self::Draft,
            ThreadState::Spec => Self::Spec,
            ThreadState::Working => Self::Working,
            ThreadState::Review => Self::Review,
            ThreadState::Error => Self::Error,
            ThreadState::Closed => Self::Closed,
            ThreadState::Done => Self::Done,
            ThreadState::Merged => Self::Merged,
        }
    }
}

impl From<CoreThreadState> for ThreadState {
    fn from(value: CoreThreadState) -> Self {
        match value {
            CoreThreadState::Draft => Self::Draft,
            CoreThreadState::Spec => Self::Spec,
            CoreThreadState::Working => Self::Working,
            CoreThreadState::Review => Self::Review,
            CoreThreadState::Error => Self::Error,
            CoreThreadState::Closed => Self::Closed,
            CoreThreadState::Done => Self::Done,
            CoreThreadState::Merged => Self::Merged,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum AgentActivity {
    Thinking,
    Executing,
    Parked,
    Stalled,
    AtLimit,
    Idle,
}

impl From<AgentActivity> for CoreAgentActivity {
    fn from(value: AgentActivity) -> Self {
        match value {
            AgentActivity::Thinking => Self::Thinking,
            AgentActivity::Executing => Self::Executing,
            AgentActivity::Parked => Self::Parked,
            AgentActivity::Stalled => Self::Stalled,
            AgentActivity::AtLimit => Self::AtLimit,
            AgentActivity::Idle => Self::Idle,
        }
    }
}

impl From<CoreAgentActivity> for AgentActivity {
    fn from(value: CoreAgentActivity) -> Self {
        match value {
            CoreAgentActivity::Thinking => Self::Thinking,
            CoreAgentActivity::Executing => Self::Executing,
            CoreAgentActivity::Parked => Self::Parked,
            CoreAgentActivity::Stalled => Self::Stalled,
            CoreAgentActivity::AtLimit => Self::AtLimit,
            CoreAgentActivity::Idle => Self::Idle,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    Draft,
    Todo,
    Now,
    Done,
}

impl From<TaskState> for CoreTaskState {
    fn from(value: TaskState) -> Self {
        match value {
            TaskState::Draft => Self::Draft,
            TaskState::Todo => Self::Todo,
            TaskState::Now => Self::Now,
            TaskState::Done => Self::Done,
        }
    }
}

impl From<CoreTaskState> for TaskState {
    fn from(value: CoreTaskState) -> Self {
        match value {
            CoreTaskState::Draft => Self::Draft,
            CoreTaskState::Todo => Self::Todo,
            CoreTaskState::Now => Self::Now,
            CoreTaskState::Done => Self::Done,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", content = "thread_id", rename_all = "snake_case")]
pub enum TaskAttribution {
    Spec,
    User,
    Thread(u32),
}

impl From<TaskAttribution> for CoreTaskAttribution {
    fn from(value: TaskAttribution) -> Self {
        match value {
            TaskAttribution::Spec => Self::Spec,
            TaskAttribution::User => Self::User,
            TaskAttribution::Thread(thread_id) => Self::Thread(thread_id),
        }
    }
}

impl From<CoreTaskAttribution> for TaskAttribution {
    fn from(value: CoreTaskAttribution) -> Self {
        match value {
            CoreTaskAttribution::Spec => Self::Spec,
            CoreTaskAttribution::User => Self::User,
            CoreTaskAttribution::Thread(thread_id) => Self::Thread(thread_id),
        }
    }
}

/// A provider: one of the four built in, or an ACP agent you added (F15 Custom ACP agents).
///
/// It crosses IPC and `state.json` as a string: `claude`, `codex`, `antigravity`, `kimi`, or
/// `custom-<id>`. The four keep the names saved before custom agents existed, and a string
/// lets the window compare and key providers by value.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProviderKind {
    Claude,
    Codex,
    Antigravity,
    Kimi,
    /// The custom agent with this identifier in `ProviderSettings::custom_agents`.
    Custom(u32),
}

const CUSTOM_PROVIDER_PREFIX: &str = "custom-";

impl ProviderKind {
    /// The provider's IPC and saved name.
    pub fn key(self) -> String {
        match self {
            Self::Claude => "claude".into(),
            Self::Codex => "codex".into(),
            Self::Antigravity => "antigravity".into(),
            Self::Kimi => "kimi".into(),
            Self::Custom(identifier) => format!("{CUSTOM_PROVIDER_PREFIX}{identifier}"),
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "claude" => Some(Self::Claude),
            "codex" => Some(Self::Codex),
            "antigravity" => Some(Self::Antigravity),
            "kimi" => Some(Self::Kimi),
            other => other
                .strip_prefix(CUSTOM_PROVIDER_PREFIX)
                .and_then(|identifier| identifier.parse().ok())
                .map(Self::Custom),
        }
    }
}

impl Serialize for ProviderKind {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.key())
    }
}

impl<'de> Deserialize<'de> for ProviderKind {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let key = String::deserialize(deserializer)?;
        Self::from_key(&key)
            .ok_or_else(|| serde::de::Error::custom(format!("unknown provider `{key}`")))
    }
}

/// The shape the window sees: `export type ProviderKind = string`, still named so the
/// frontend keeps one type for a provider.
mod provider_kind_wire {
    #[derive(specta::Type)]
    #[allow(dead_code)]
    pub struct ProviderKind(String);
}

impl Type for ProviderKind {
    fn definition(types: &mut specta::Types) -> specta::datatype::DataType {
        provider_kind_wire::ProviderKind::definition(types)
    }
}

impl From<ProviderKind> for CoreProviderKind {
    fn from(value: ProviderKind) -> Self {
        match value {
            ProviderKind::Claude => Self::Claude,
            ProviderKind::Codex => Self::Codex,
            ProviderKind::Antigravity => Self::Antigravity,
            ProviderKind::Kimi => Self::Kimi,
            ProviderKind::Custom(identifier) => Self::Custom(identifier),
        }
    }
}

impl From<CoreProviderKind> for ProviderKind {
    fn from(value: CoreProviderKind) -> Self {
        match value {
            CoreProviderKind::Claude => Self::Claude,
            CoreProviderKind::Codex => Self::Codex,
            CoreProviderKind::Antigravity => Self::Antigravity,
            CoreProviderKind::Kimi => Self::Kimi,
            CoreProviderKind::Custom(identifier) => Self::Custom(identifier),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum EffortLevel {
    Low,
    Medium,
    High,
    Max,
}

impl From<EffortLevel> for CoreEffortLevel {
    fn from(value: EffortLevel) -> Self {
        match value {
            EffortLevel::Low => Self::Low,
            EffortLevel::Medium => Self::Medium,
            EffortLevel::High => Self::High,
            EffortLevel::Max => Self::Max,
        }
    }
}

impl From<CoreEffortLevel> for EffortLevel {
    fn from(value: CoreEffortLevel) -> Self {
        match value {
            CoreEffortLevel::Low => Self::Low,
            CoreEffortLevel::Medium => Self::Medium,
            CoreEffortLevel::High => Self::High,
            CoreEffortLevel::Max => Self::Max,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum SpeedMode {
    Regular,
    Fast,
}

impl From<SpeedMode> for CoreSpeedMode {
    fn from(value: SpeedMode) -> Self {
        match value {
            SpeedMode::Regular => Self::Regular,
            SpeedMode::Fast => Self::Fast,
        }
    }
}

impl From<CoreSpeedMode> for SpeedMode {
    fn from(value: CoreSpeedMode) -> Self {
        match value {
            CoreSpeedMode::Regular => Self::Regular,
            CoreSpeedMode::Fast => Self::Fast,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ProjectLocationKind {
    Local,
    GitHub,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStage {
    ContinuousIntegration,
    Review,
    Fixing,
    Approved,
}

impl From<ReviewStage> for CoreReviewStage {
    fn from(value: ReviewStage) -> Self {
        match value {
            ReviewStage::ContinuousIntegration => Self::ContinuousIntegration,
            ReviewStage::Review => Self::Review,
            ReviewStage::Fixing => Self::Fixing,
            ReviewStage::Approved => Self::Approved,
        }
    }
}

impl From<CoreReviewStage> for ReviewStage {
    fn from(value: CoreReviewStage) -> Self {
        match value {
            CoreReviewStage::ContinuousIntegration => Self::ContinuousIntegration,
            CoreReviewStage::Review => Self::Review,
            CoreReviewStage::Fixing => Self::Fixing,
            CoreReviewStage::Approved => Self::Approved,
        }
    }
}

fn deserialize_optional_non_empty_string<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(raw.filter(|value| !value.is_empty()))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct WorktreeSettings {
    pub location_root: Option<String>,
    pub delete_local_branch_on_merge: bool,
    pub share_rust_build_cache: bool,
    pub warm_pool_count: u32,
    #[serde(default)]
    pub project_roots: BTreeMap<u32, String>,
    #[serde(default = "worktree_settings::default_copy_files")]
    pub copy_files: String,
    #[serde(default = "worktree_settings::default_setup_command")]
    pub setup_command: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ProviderSettings {
    pub default_provider: ProviderKind,
    pub default_model: Option<String>,
    pub default_effort: EffortLevel,
    pub default_speed: SpeedMode,
    /// Absent from state saved before thread titles; GPT-6 Luna's provider then (F15).
    #[serde(default = "default_title_provider")]
    pub title_provider: ProviderKind,
    pub title_model: String,
    pub title_effort: EffortLevel,
    pub title_speed: SpeedMode,
    /// What each provider's agent last reported (F15). Absent from state saved before
    /// Settings → Providers.
    #[serde(default)]
    pub catalog: Vec<providers::ProviderCatalog>,
    /// Providers you signed out of in Velocity: new work can't use them (F15 Sign out).
    #[serde(default)]
    pub signed_out: Vec<ProviderKind>,
    /// Model chips you switched off (F15 Models).
    #[serde(default)]
    pub hidden_models: Vec<providers::HiddenModel>,
    /// Built-in providers whose ACP agent command you edited; Reset removes the entry (F15).
    #[serde(default)]
    pub commands: Vec<providers::ProviderCommand>,
    /// ACP agents you added beside the four (F15 Custom ACP agents), in card order.
    #[serde(default)]
    pub custom_agents: Vec<providers::CustomAgent>,
    /// The identifier the next added agent gets. Identifiers are never reused.
    #[serde(default)]
    pub next_custom_agent: u32,
}

fn default_title_provider() -> ProviderKind {
    ProviderKind::Codex
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct AgentSettings {
    pub max_turns: Option<u32>,
    pub max_duration_minutes: Option<u32>,
    pub stall_timeout_minutes: u32,
    #[serde(default = "default_tool_timeout")]
    pub tool_timeout_minutes: u32,
    pub claim_release_grace_minutes: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ProofSettings {
    pub stress_diff_input_count: u32,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct Settings {
    pub worktrees: WorktreeSettings,
    pub providers: ProviderSettings,
    pub agents: AgentSettings,
    pub proofs: ProofSettings,
}

impl Settings {
    pub fn to_core(&self) -> CoreSettings<String, String> {
        CoreSettings {
            worktrees: CoreWorktreeSettings {
                location_root: self.worktrees.location_root.clone(),
                delete_local_branch_on_merge: self.worktrees.delete_local_branch_on_merge,
                share_rust_build_cache: self.worktrees.share_rust_build_cache,
                warm_pool_count: self.worktrees.warm_pool_count,
            },
            providers: CoreProviderSettings {
                default_provider: self.providers.default_provider.into(),
                default_model: self.providers.default_model.clone(),
                default_effort: self.providers.default_effort.into(),
                default_speed: self.providers.default_speed.into(),
                title_provider: self.providers.title_provider.into(),
                title_model: self.providers.title_model.clone(),
                title_effort: self.providers.title_effort.into(),
                title_speed: self.providers.title_speed.into(),
            },
            agents: CoreAgentSettings::new(
                self.agents.max_turns,
                self.agents.max_duration_minutes,
                self.agents.stall_timeout_minutes,
                self.agents.tool_timeout_minutes,
                self.agents.claim_release_grace_minutes,
            ),
            proofs: CoreProofSettings {
                stress_diff_input_count: self.proofs.stress_diff_input_count,
            },
        }
    }

    pub fn from_core(core_settings: CoreSettings<String, String>) -> Self {
        Self {
            worktrees: WorktreeSettings {
                location_root: core_settings.worktrees.location_root,
                delete_local_branch_on_merge: core_settings.worktrees.delete_local_branch_on_merge,
                share_rust_build_cache: core_settings.worktrees.share_rust_build_cache,
                warm_pool_count: core_settings.worktrees.warm_pool_count,
                project_roots: BTreeMap::new(),
                copy_files: worktree_settings::default_copy_files(),
                setup_command: worktree_settings::default_setup_command(),
            },
            providers: ProviderSettings {
                default_provider: core_settings.providers.default_provider.into(),
                default_model: core_settings.providers.default_model,
                default_effort: core_settings.providers.default_effort.into(),
                default_speed: core_settings.providers.default_speed.into(),
                title_provider: core_settings.providers.title_provider.into(),
                title_model: core_settings.providers.title_model,
                title_effort: core_settings.providers.title_effort.into(),
                title_speed: core_settings.providers.title_speed.into(),
                catalog: Vec::new(),
                signed_out: Vec::new(),
                hidden_models: Vec::new(),
                commands: Vec::new(),
                custom_agents: Vec::new(),
                next_custom_agent: 1,
            },
            agents: AgentSettings {
                max_turns: core_settings.agents.max_turns,
                max_duration_minutes: core_settings.agents.max_duration_minutes,
                stall_timeout_minutes: core_settings.agents.stall_timeout_minutes,
                tool_timeout_minutes: core_settings.agents.tool_timeout_minutes,
                claim_release_grace_minutes: core_settings.agents.claim_release_grace_minutes,
            },
            proofs: ProofSettings {
                stress_diff_input_count: core_settings.proofs.stress_diff_input_count,
            },
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self::from_core(core_state::default_settings(String::from(
            DEFAULT_TITLE_MODEL_NAME,
        )))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct TaskItem {
    /// AIRC request sequence, saved atomically with the task to prevent replay duplicates.
    #[serde(default)]
    #[specta(skip)]
    pub agent_request: Option<u64>,
    pub id: u32,
    pub text: String,
    pub state: TaskState,
    pub added_by: TaskAttribution,
    pub assigned_thread_id: Option<u32>,
}

impl TaskItem {
    pub fn new(id: u32, text: String, state: TaskState, added_by: TaskAttribution) -> Self {
        Self {
            id,
            text,
            state,
            added_by,
            assigned_thread_id: None,
            agent_request: None,
        }
    }

    pub fn normalize(mut self, spec_locked: bool) -> Self {
        let normalized = core_state::normalize_task_record(
            spec_locked,
            CoreTaskRecord {
                id: self.id,
                text: self.text,
                task_state: self.state.into(),
                added_by: self.added_by.into(),
                assigned_thread_id: self.assigned_thread_id,
            },
        );
        self.id = normalized.id;
        self.text = normalized.text;
        self.state = normalized.task_state.into();
        self.added_by = normalized.added_by.into();
        self.assigned_thread_id = normalized.assigned_thread_id;
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeRole {
    #[default]
    Thread,
    Review,
    Sweep,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ThreadItem {
    #[serde(default)]
    pub supervision: agent_activity::Supervision,
    pub id: u32,
    pub title: String,
    pub state: ThreadState,
    pub activity: AgentActivity,
    pub branch_name: String,
    #[serde(deserialize_with = "deserialize_optional_non_empty_string")]
    #[specta(type = Option<String>)]
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub worktree_role: WorktreeRole,
    #[serde(default, skip_serializing)]
    #[specta(skip)]
    pub worktree_path_id: Option<String>,
    pub pull_request_number: Option<u32>,
    #[serde(default)]
    pub follow_through: Option<velocity_github::follow_through::FollowThrough>,
    /// Last AIRC message whose receiving turn completed (F14).
    #[serde(default)]
    #[specta(skip)]
    pub airc_cursor: u64,
    #[serde(default)]
    pub dependency_wait: Option<String>,
    /// The agent's ACP session id, which a parked thread loads again on resume (§2.2).
    #[serde(default)]
    pub agent_session: Option<String>,
    pub review_stage: Option<ReviewStage>,
    pub task_ids: Vec<u32>,
    #[serde(default)]
    pub issue: Option<issues::ThreadIssue>,
    /// F16 inventory identity; merged threads record built sections without editing SPEC.md.
    #[serde(default)]
    pub spec_section: Option<String>,
    pub additions: u32,
    pub deletions: u32,
    pub elapsed_seconds: u32,
    pub provider: ProviderKind,
    pub model_name: String,
    pub effort: EffortLevel,
    pub preferred_speed: SpeedMode,
    pub effective_speed: SpeedMode,
    pub model_supports_fast: bool,
    /// Spec interview threads only (F3).
    #[serde(default)]
    pub interview: Option<InterviewRecord>,
    /// Goal swarm membership and choosing-work phase (F16).
    #[serde(default)]
    pub goal_member: Option<goal_swarms::GoalThreadMember>,
}

impl ThreadItem {
    pub fn new(id: u32, title: String) -> Self {
        Self {
            id,
            title,
            state: ThreadState::Working,
            activity: AgentActivity::Executing,
            branch_name: String::new(),
            worktree_path: None,
            worktree_role: WorktreeRole::Thread,
            worktree_path_id: None,
            pull_request_number: None,
            follow_through: None,
            airc_cursor: 0,
            dependency_wait: None,
            agent_session: None,
            supervision: Default::default(),
            review_stage: None,
            task_ids: Vec::new(),
            issue: None,
            spec_section: None,
            additions: 0,
            deletions: 0,
            elapsed_seconds: 0,
            provider: ProviderKind::Codex,
            model_name: String::new(),
            effort: EffortLevel::High,
            preferred_speed: SpeedMode::Regular,
            effective_speed: SpeedMode::Regular,
            model_supports_fast: false,
            interview: None,
            goal_member: None,
        }
    }

    pub(crate) fn for_build(thread: threads::NewThread, identifier: u32) -> Self {
        Self {
            id: identifier,
            title: thread.title,
            state: ThreadState::Working,
            activity: AgentActivity::Executing,
            branch_name: thread.branch_name,
            worktree_path: Some(thread.worktree_path),
            // A thread started from the composer builds in its own worktree (F4, F5).
            worktree_role: WorktreeRole::Thread,
            worktree_path_id: None,
            pull_request_number: None,
            follow_through: None,
            airc_cursor: 0,
            dependency_wait: None,
            agent_session: None,
            supervision: Default::default(),
            review_stage: None,
            task_ids: Vec::new(),
            issue: None,
            spec_section: None,
            additions: 0,
            deletions: 0,
            elapsed_seconds: 0,
            provider: thread.provider,
            model_name: thread.model_name,
            effort: thread.effort,
            preferred_speed: thread.preferred_speed,
            effective_speed: thread.preferred_speed,
            model_supports_fast: thread.model_supports_fast,
            interview: None,
            goal_member: None,
        }
        .normalize()
    }

    fn into_core_record(self) -> (Self, CoreThreadRecord<String, String>) {
        let core_record = CoreThreadRecord {
            id: self.id,
            thread_state: self.state.into(),
            activity: self.activity.into(),
            provider: self.provider.into(),
            model: self.model_name.clone(),
            model_supports_fast: self.model_supports_fast,
            effort: self.effort.into(),
            preferred_speed: self.preferred_speed.into(),
            effective_speed: self.effective_speed.into(),
            worktree_path: self.worktree_path.clone(),
            pull_request_number: self.pull_request_number,
            review_stage: self.review_stage.map(Into::into),
            additions: self.additions,
            deletions: self.deletions,
        };
        (self, core_record)
    }

    fn apply_core_record(mut self, record: CoreThreadRecord<String, String>) -> Self {
        self.id = record.id;
        self.state = record.thread_state.into();
        self.activity = record.activity.into();
        self.provider = record.provider.into();
        self.model_name = record.model;
        self.model_supports_fast = record.model_supports_fast;
        self.effort = record.effort.into();
        self.preferred_speed = record.preferred_speed.into();
        self.effective_speed = record.effective_speed.into();
        self.worktree_path_id = record.worktree_path.clone();
        self.worktree_path = record.worktree_path;
        self.pull_request_number = record.pull_request_number;
        self.review_stage = record.review_stage.map(Into::into);
        self.additions = record.additions;
        self.deletions = record.deletions;
        self
    }

    pub fn normalize(self) -> Self {
        let (item, core_record) = self.into_core_record();
        let normalized = core_state::normalize_thread_record(core_record);
        item.apply_core_record(normalized)
    }

    pub fn switch_model(self, next_model_name: String, next_model_supports_fast: bool) -> Self {
        let (item, core_record) = self.into_core_record();
        let switched =
            core_state::switch_thread_model(core_record, next_model_name, next_model_supports_fast);
        item.apply_core_record(switched)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ProjectItem {
    pub id: u32,
    pub name: String,
    pub location_kind: ProjectLocationKind,
    pub repository_path: String,
    pub default_branch: String,
    pub github_remote: Option<String>,
    pub spec_path: String,
    pub spec_locked: bool,
    /// Completed F16 pieces retained after their merged thread history is deleted.
    #[serde(default)]
    pub built_spec_sections: Vec<String>,
    pub active_thread_id: Option<u32>,
    pub merged_today: u32,
    pub tasks: Vec<TaskItem>,
    pub threads: Vec<ThreadItem>,
    /// The spec interview Create asked for and nothing has started yet (F2, F3).
    #[serde(default)]
    pub pending_interview: Option<PendingInterview>,
    /// Duty roles started on this project (F7).
    #[serde(default)]
    pub duty_roles: Vec<DutyRoleItem>,
    /// Swarms launched on this project, newest first (F10).
    #[serde(default)]
    pub swarms: Vec<swarms::SwarmRecord>,
    /// Goal swarms launched on this project, newest first (F16).
    #[serde(default)]
    pub goal_swarms: Vec<goal_swarms::GoalSwarmRecord>,
    /// The newest AIRC record shown while the modal was open: the footer badge counts
    /// lines after it (F14).
    #[serde(default)]
    #[specta(skip)]
    pub airc_seen: u64,
}

/// The New project dialog's interview choices, kept until the spec interview
/// (F3) starts its thread from them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PendingInterview {
    /// `None` until a provider is signed in and offers a model (F15, issue #25).
    pub model_name: Option<String>,
    pub effort: EffortLevel,
    pub preferred_speed: SpeedMode,
    pub brief: String,
}

impl ProjectItem {
    pub fn new(identifier: u32, name: String, repository_path: String) -> Self {
        Self {
            id: identifier,
            name,
            repository_path,
            location_kind: ProjectLocationKind::Local,
            default_branch: "master".into(),
            github_remote: None,
            spec_path: "SPEC.md".into(),
            spec_locked: false,
            built_spec_sections: Vec::new(),
            active_thread_id: None,
            merged_today: 0,
            tasks: Vec::new(),
            threads: Vec::new(),
            pending_interview: None,
            duty_roles: Vec::new(),
            swarms: Vec::new(),
            goal_swarms: Vec::new(),
            airc_seen: 0,
        }
    }

    pub fn normalize(mut self) -> Self {
        let spec_locked = self.spec_locked;
        self.tasks = self
            .tasks
            .into_iter()
            .map(|task| task.normalize(spec_locked))
            .collect();
        self.threads = self
            .threads
            .into_iter()
            .map(ThreadItem::normalize)
            .collect();
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PersistedState {
    pub revision: u32,
    pub active_project_index: u32,
    pub projects: Vec<ProjectItem>,
    pub settings: Settings,
    /// Deleted rows must never let delayed agent or watcher events address a new thread.
    #[serde(default)]
    pub next_thread_identifiers: BTreeMap<u32, u32>,
    #[serde(default)]
    pub thread_started_at: BTreeMap<u32, BTreeMap<u32, String>>,
}

impl Default for PersistedState {
    fn default() -> Self {
        Self {
            revision: 1,
            active_project_index: 0,
            projects: Vec::new(),
            settings: Settings::default(),
            next_thread_identifiers: BTreeMap::new(),
            thread_started_at: BTreeMap::new(),
        }
    }
}

impl PersistedState {
    pub fn normalize(mut self) -> Self {
        let project_count = u32::try_from(self.projects.len()).unwrap_or(u32::MAX);
        let normalized_header =
            core_state::normalize_persisted_state_header(PersistedStateHeader {
                active_project_index: self.active_project_index,
                project_count,
                settings: self.settings.to_core(),
            });
        self.active_project_index = normalized_header.active_project_index;
        // The pure header normalizes selection only. Retain serialized worktree
        // preferences that are intentionally outside that minimal core header.
        self.projects = self
            .projects
            .into_iter()
            .map(ProjectItem::normalize)
            .collect();
        self
    }

    pub fn apply_settings(&mut self, next_settings: Settings) {
        let project_count = u32::try_from(self.projects.len()).unwrap_or(u32::MAX);
        let updated_header = core_state::apply_settings_update(
            PersistedStateHeader {
                active_project_index: self.active_project_index,
                project_count,
                settings: self.settings.to_core(),
            },
            next_settings.to_core(),
        );
        self.active_project_index = updated_header.active_project_index;
        self.settings = next_settings;
        self.revision = self.revision.wrapping_add(1);
    }
}

/// Convert an absolute Windows path to extended-length (`\\?\`) syntax so paths
/// with spaces, drive letters, and > 260 characters work reliably.
pub fn extended_windows_path(path: &Path) -> PathBuf {
    if !cfg!(windows) || !path.is_absolute() {
        return path.to_path_buf();
    }
    let display = path.to_string_lossy();
    if display.starts_with(r"\\?\") {
        return path.to_path_buf();
    }
    if let Some(network_suffix) = display.strip_prefix(r"\\") {
        return PathBuf::from(format!(r"\\?\UNC\{network_suffix}"));
    }
    PathBuf::from(format!(r"\\?\{display}"))
}

fn write_atomic_snapshot(directory: &Path, state: &PersistedState) -> Result<(), StoreError> {
    let serialized = serde_json::to_vec_pretty(state)
        .map_err(|source| StoreError::SerializeSnapshot { source })?;
    let partial_path = extended_windows_path(&directory.join(PARTIAL_STATE_FILE_NAME));
    let destination_path = extended_windows_path(&directory.join(STATE_FILE_NAME));
    let mut partial_file =
        File::create(&partial_path).map_err(|source| StoreError::WritePartial {
            path: partial_path.display().to_string(),
            source,
        })?;
    partial_file
        .write_all(&serialized)
        .map_err(|source| StoreError::WritePartial {
            path: partial_path.display().to_string(),
            source,
        })?;
    partial_file
        .sync_all()
        .map_err(|source| StoreError::SyncPartial {
            path: partial_path.display().to_string(),
            source,
        })?;
    drop(partial_file);
    fs::rename(&partial_path, &destination_path).map_err(|source| StoreError::ReplaceSnapshot {
        partial: partial_path.display().to_string(),
        destination: destination_path.display().to_string(),
        source,
    })
}

fn recover_or_cleanup_partial(directory: &Path) -> Result<(), StoreError> {
    let partial_path = extended_windows_path(&directory.join(PARTIAL_STATE_FILE_NAME));
    let destination_path = extended_windows_path(&directory.join(STATE_FILE_NAME));
    if !partial_path.exists() {
        return Ok(());
    }
    if destination_path.exists() {
        let _ignored = fs::remove_file(&partial_path);
        return Ok(());
    }
    if let Ok(bytes) = fs::read(&partial_path)
        && serde_json::from_slice::<PersistedState>(&bytes).is_ok()
    {
        fs::rename(&partial_path, &destination_path).map_err(|source| {
            StoreError::ReplaceSnapshot {
                partial: partial_path.display().to_string(),
                destination: destination_path.display().to_string(),
                source,
            }
        })?;
        return Ok(());
    }
    let _ignored = fs::remove_file(&partial_path);
    Ok(())
}

fn move_corrupt_snapshot_aside(
    directory: &Path,
    snapshot_path: &Path,
) -> Result<PathBuf, StoreError> {
    let mut backup_index: u32 = 1;
    let corrupt_path = loop {
        let candidate = extended_windows_path(
            &directory.join(format!("{STATE_FILE_NAME}.corrupt-{backup_index}")),
        );
        if !candidate.exists() {
            break candidate;
        }
        backup_index = backup_index.saturating_add(1);
    };
    fs::rename(snapshot_path, &corrupt_path).map_err(|source| {
        StoreError::QuarantineCorruptSnapshot {
            corrupt: corrupt_path.display().to_string(),
            destination: snapshot_path.display().to_string(),
            source,
        }
    })?;
    Ok(corrupt_path)
}

pub struct StateStore {
    directory: PathBuf,
    state: PersistedState,
    disk_revision: u32,
    recovered_corrupt_snapshot: Option<PathBuf>,
}

impl StateStore {
    pub fn load_or_default(directory: impl AsRef<Path>) -> Result<Self, StoreError> {
        let resolved_directory = extended_windows_path(directory.as_ref());
        fs::create_dir_all(&resolved_directory).map_err(|source| StoreError::CreateDirectory {
            path: resolved_directory.display().to_string(),
            source,
        })?;
        recover_or_cleanup_partial(&resolved_directory)?;
        let snapshot_path = extended_windows_path(&resolved_directory.join(STATE_FILE_NAME));
        if snapshot_path.exists() {
            Self::load_existing_or_recover(resolved_directory, &snapshot_path)
        } else {
            Self::initialize_fresh_snapshot(resolved_directory, None)
        }
    }

    fn load_existing_or_recover(
        resolved_directory: PathBuf,
        snapshot_path: &Path,
    ) -> Result<Self, StoreError> {
        let bytes = fs::read(snapshot_path).map_err(|source| StoreError::ReadSnapshot {
            path: snapshot_path.display().to_string(),
            source,
        })?;
        match serde_json::from_slice::<PersistedState>(&bytes) {
            Ok(parsed) => {
                let normalized = parsed.normalize();
                let disk_revision = normalized.revision;
                Ok(Self {
                    directory: resolved_directory,
                    state: normalized,
                    disk_revision,
                    recovered_corrupt_snapshot: None,
                })
            }
            Err(parse_error) => {
                let corrupt_path = move_corrupt_snapshot_aside(&resolved_directory, snapshot_path)?;
                eprintln!(
                    "Quarantined unreadable state file '{}' to '{}' ({parse_error}); starting from defaults.",
                    snapshot_path.display(),
                    corrupt_path.display()
                );
                Self::initialize_fresh_snapshot(resolved_directory, Some(corrupt_path))
            }
        }
    }

    fn initialize_fresh_snapshot(
        resolved_directory: PathBuf,
        recovered_corrupt_snapshot: Option<PathBuf>,
    ) -> Result<Self, StoreError> {
        let initial_state = PersistedState::default().normalize();
        write_atomic_snapshot(&resolved_directory, &initial_state)?;
        let disk_revision = initial_state.revision;
        Ok(Self {
            directory: resolved_directory,
            state: initial_state,
            disk_revision,
            recovered_corrupt_snapshot,
        })
    }

    pub fn recovered_corrupt_snapshot(&self) -> Option<&Path> {
        self.recovered_corrupt_snapshot.as_deref()
    }

    pub fn state(&self) -> &PersistedState {
        &self.state
    }

    pub fn settings(&self) -> Settings {
        self.state.settings.clone()
    }

    pub fn set_settings(&mut self, next_settings: Settings) -> Result<Settings, StoreError> {
        let agents = &next_settings.agents;
        if agents.stall_timeout_minutes == 0
            || agents.tool_timeout_minutes == 0
            || agents.claim_release_grace_minutes == 0
            || agents.max_turns == Some(0)
            || agents.max_duration_minutes == Some(0)
        {
            return Err(StoreError::AgentSetting);
        }
        if self.state.settings != next_settings {
            self.state.apply_settings(next_settings);
        }
        self.save_if_changed()?;
        Ok(self.state.settings.clone())
    }

    pub fn save_if_changed(&mut self) -> Result<bool, StoreError> {
        let (write_now, next_disk_revision) =
            apply_dirty_checked_step(self.disk_revision, self.state.revision);
        if write_now {
            write_atomic_snapshot(&self.directory, &self.state)?;
        }
        self.disk_revision = next_disk_revision;
        Ok(write_now)
    }
}

fn default_tool_timeout() -> u32 {
    30
}
