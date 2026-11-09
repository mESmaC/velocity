//! Read models and actions for the main-window shell (SPEC.md F1, F4).

use crate::interview_view::InterviewEntry;
use crate::layout::{LayoutRow, PulseKind, ThreadGroup};
use crate::navigation::{NavigationIntent, NavigationView, NumberHintEvent};
use crate::thread_view_model::{
    AgentCommandView, ModelPillView, SlashCommandItem, ThreadDetailEntry, ThreadDetailSnapshot,
};

#[derive(Clone)]
pub struct ThreadEntry {
    pub policy: LayoutRow,
    pub title: String,
    pub metadata: String,
    pub view: NavigationView,
    pub detail: Option<ThreadDetailEntry>,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct RailItem {
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub identifier: usize,
    pub title: String,
    pub metadata: String,
}

/// Feature owners publish their real data here; the shell invents no activity.
#[derive(Clone, Default)]
pub struct LayoutData {
    /// The persisted project these rows belong to; keys the live thread overlay.
    pub project_identifier: Option<u32>,
    pub project_name: String,
    pub threads: Vec<ThreadEntry>,
    pub projects: Vec<PaletteItem>,
    pub tasks: Vec<RailItem>,
    pub completed_task_count: usize,
    pub pull_requests: Vec<RailItem>,
    pub issues: Vec<RailItem>,
    pub merged_today: usize,
    pub worktree_count: usize,
    pub worktree_drive: String,
    pub providers: Vec<ProviderUsage>,
    pub airc_unread: usize,
    pub overlap_unread: bool,
    pub default_model_pill: Option<ModelPillView>,
    pub agent_commands: Vec<AgentCommandView>,
    /// Spec interview read models, keyed by thread (F3).
    pub interviews: Vec<InterviewEntry>,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct ProviderUsage {
    pub provider: ProviderColor,
    pub label: String,
    pub percent_used: u8,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug)]
pub enum ProviderColor {
    Claude,
    Codex,
    Antigravity,
    Kimi,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct SidebarRow {
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub identifier: usize,
    pub title: String,
    pub metadata: String,
    pub group: ThreadGroup,
    pub member: bool,
    pub expanded: bool,
    pub active: bool,
    pub paused: bool,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub agents: usize,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number::optional"))]
    #[cfg_attr(feature = "bindings", specta(type = Option<String>))]
    pub number: Option<usize>,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct SidebarGroup {
    pub group: ThreadGroup,
    pub title: String,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub count: usize,
    pub rows: Vec<SidebarRow>,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct PulseCount {
    pub kind: PulseKind,
    pub label: String,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub count: usize,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub enum LayoutIntent {
    Navigation(NavigationIntent),
    SwitchProject(String),
    DispatchIssue(
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        usize,
    ),
    SwarmIssues,
    OpenTask(
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        usize,
    ),
    OpenPullRequest(
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        usize,
    ),
    OpenIssue(
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        usize,
    ),
    OpenWorktreeInExplorer(String),
    SettingsTab(String),
    Usage,
    Updates,
    StartPrompt(String),
    SteerThread {
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        identifier: usize,
        prompt: String,
        queued: bool,
    },
    AddTask(String),
    /// Ready card: include or leave out one proposed proof-list entry (F3, F17).
    ToggleProofListEntry(
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        usize,
    ),
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub enum PaletteTarget {
    Intent(LayoutIntent),
    Thread(
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        usize,
    ),
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct PaletteItem {
    pub identifier: String,
    pub label: String,
    pub group: String,
    pub shortcut: String,
    pub target: PaletteTarget,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct KeyboardInput {
    pub key: String,
    pub code: String,
    pub control: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta_held: bool,
    pub typing: bool,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub enum LayoutAction {
    Request(LayoutIntent),
    SidebarSearch(String),
    PaletteSearch(String),
    PaletteSelect(
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        usize,
    ),
    PaletteNext,
    PalettePrevious,
    PaletteAccept,
    OpenThread(
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        usize,
    ),
    ToggleSwarm(
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        usize,
    ),
    Keyboard(KeyboardInput),
    NumberHints(NumberHintEvent),
    ComposerText(String),
    SlashSelect(
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        usize,
    ),
    SlashNext,
    SlashPrevious,
    SlashComplete,
    ToggleShowWork,
    TaskText(String),
    SubmitComposer,
    SubmitTask,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct LayoutSnapshot {
    pub revision: u32,
    pub project_name: String,
    pub view: NavigationView,
    pub settings_tab: String,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number::optional"))]
    #[cfg_attr(feature = "bindings", specta(type = Option<String>))]
    pub selected_thread: Option<usize>,
    pub active_thread: Option<ThreadDetailSnapshot>,
    pub default_model_pill: ModelPillView,
    pub search: String,
    pub groups: Vec<SidebarGroup>,
    pub pulse: Vec<PulseCount>,
    pub show_numbers: bool,
    pub palette_open: bool,
    pub new_project_open: bool,
    pub palette_query: String,
    pub palette_results: Vec<PaletteItem>,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub palette_selection: usize,
    pub slash_items: Vec<SlashCommandItem>,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub slash_selection: usize,
    pub tasks: Vec<RailItem>,
    pub pull_requests: Vec<RailItem>,
    pub issues: Vec<RailItem>,
    pub worktree_label: String,
    pub providers: Vec<ProviderUsage>,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub airc_unread: usize,
    pub overlap_unread: bool,
    pub composer_text: String,
    pub task_text: String,
    pub build_allowed: bool,
    pub build_block_reason: Option<String>,
    pub composer_allowed: bool,
    pub composer_block_reason: Option<String>,
    pub blocked_navigation: Vec<NavigationIntent>,
    pub tasks_draft: bool,
    pub task_count_label: String,
    pub launch_notice: Option<String>,
}
