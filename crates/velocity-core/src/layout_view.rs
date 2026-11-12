//! Read models and actions for the main-window shell (SPEC.md F1, F4, F7).

use crate::duty::{DutyRole, DutyRoleEntry, DutyViewSnapshot};
use crate::interview_view::InterviewEntry;
use crate::layout::{LayoutRow, PulseKind, ThreadGroup};
use crate::navigation::{NavigationIntent, NavigationView, NumberHintEvent};
use crate::swarm::{SwarmDialogRequest, SwarmEntry, SwarmStage, SwarmViewSnapshot};
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
    pub duty: Option<DutyRoleEntry>,
}

impl ThreadEntry {
    /// A sidebar row with no thread detail and no duty role.
    pub fn new(policy: LayoutRow, title: String, metadata: String, view: NavigationView) -> Self {
        Self {
            policy,
            title,
            metadata,
            view,
            detail: None,
            duty: None,
        }
    }

    /// A row under Swarms that opens the Swarm view and nests its members (F10, F16).
    pub fn swarm_row(identifier: usize, title: String, metadata: String, agents: usize) -> Self {
        Self {
            policy: LayoutRow {
                identifier,
                group: crate::layout::ThreadGroup::Swarm,
                hidden: false,
                matches_search: true,
                member_of: None,
                expanded: false,
                agent_count: agents,
                paused: false,
            },
            title,
            metadata,
            view: NavigationView::Swarm,
            detail: None,
            duty: None,
        }
    }
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

impl RailItem {
    pub fn new(identifier: usize, title: String, metadata: String) -> Self {
        Self {
            identifier,
            title,
            metadata,
        }
    }
}

/// F8 task rows carry independent links for ownership and attribution.
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct TaskView {
    pub identifier: String,
    pub title: String,
    pub state: String,
    pub metadata: String,
    pub thread_identifier: Option<String>,
    pub attribution: Option<String>,
    pub attribution_thread: Option<String>,
}

impl TaskView {
    pub fn new(identifier: String, title: String, state: String) -> Self {
        Self {
            identifier,
            title,
            state,
            metadata: String::new(),
            thread_identifier: None,
            attribution: None,
            attribution_thread: None,
        }
    }
}

/// Feature owners publish their real data here; the shell invents no activity.
#[derive(Clone, Default)]
pub struct LayoutData {
    /// The persisted project these rows belong to; keys the live thread overlay.
    pub project_identifier: Option<u32>,
    pub project_name: String,
    pub settled_expanded: bool,
    pub threads: Vec<ThreadEntry>,
    pub projects: Vec<PaletteItem>,
    pub tasks: Vec<TaskView>,
    pub completed_task_count: usize,
    pub pull_requests: Vec<RailItem>,
    pub issues: Vec<RailItem>,
    pub merged_today: usize,
    pub worktree_count: usize,
    pub worktree_drive: String,
    pub providers: Vec<ProviderUsage>,
    pub default_model_pill: Option<ModelPillView>,
    pub agent_commands: Vec<AgentCommandView>,
    /// Spec interview read models, keyed by thread (F3).
    pub interviews: Vec<InterviewEntry>,
    /// Swarm rows and views, keyed by their sidebar identifier (F10).
    pub swarms: Vec<SwarmEntry>,
    /// Goal swarm rows and views, keyed by their sidebar identifier (F16).
    pub goal_swarms: Vec<crate::goal_swarm_view::GoalSwarmEntry>,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderUsage {
    pub provider: ProviderColor,
    pub label: String,
    pub percent_used: u8,
    /// The usage panel's heading: `Claude`.
    pub name: String,
    /// The subscription plan the provider named (`Max`, `Pro`).
    pub plan: Option<String>,
    /// The usage panel's rows, one per subscription window, shortest first.
    pub windows: Vec<UsageWindowView>,
    /// What the panel shows instead of rows: `No limits reported.`
    pub note: Option<String>,
}

impl ProviderUsage {
    pub fn new(provider: ProviderColor, label: String, percent_used: u8) -> Self {
        Self {
            provider,
            label,
            percent_used,
            name: String::new(),
            plan: None,
            windows: Vec::new(),
            note: None,
        }
    }

    /// The footer usage panel's section for this provider (F15).
    pub fn with_panel(
        mut self,
        name: String,
        plan: Option<String>,
        windows: Vec<UsageWindowView>,
        note: Option<String>,
    ) -> Self {
        self.name = name;
        self.plan = plan;
        self.windows = windows;
        self.note = note;
        self
    }
}

/// One subscription window in the footer usage panel, as T3 Code draws it: the share left,
/// a bar of what is left and when it resets (F15).
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageWindowView {
    /// `5-hour`, `Weekly`.
    pub label: String,
    pub percent_left: u8,
    /// `resets in 2h 14m`, or empty when the provider gave no reset time.
    pub resets_in: String,
    /// The bar's tooltip and accessible name, with the pace: the share of the window's time
    /// left, where even spending would put the bar.
    pub summary: String,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderColor {
    Claude,
    Codex,
    Antigravity,
    Kimi,
    /// An ACP agent you added (F15 Custom ACP agents).
    Custom,
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
    /// The title model hasn't answered yet: the title is the message's first words, pulsing
    /// (F4).
    pub titling: bool,
    pub metadata: String,
    pub group: ThreadGroup,
    pub duty_role: Option<DutyRole>,
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
    /// A swarm row's segmented progress bar, one segment per member (F10).
    pub segments: Vec<SwarmStage>,
    /// A goal swarm row: the target icon instead of the swarm icon (F16).
    pub goal: bool,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct SidebarGroup {
    pub collapsed: bool,
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
    /// Withdraws a message you queued that the agent has not read yet (F4). `index` counts
    /// the thread's queued rows, oldest first.
    RemoveQueuedMessage {
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        identifier: usize,
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        index: usize,
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
    ToggleSettled,
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
    /// The swarm view's + Items: the Swarm dialog adding open items to that swarm (F10).
    AddSwarmItems(
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        usize,
    ),
    /// The goal swarm view's + Agents: the Goal swarm dialog adding agents on its prompt (F16).
    AddGoalAgents(
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
    pub project_identifier: Option<u32>,
    pub project_name: String,
    pub view: NavigationView,
    pub settings_tab: String,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number::optional"))]
    #[cfg_attr(feature = "bindings", specta(type = Option<String>))]
    pub selected_thread: Option<usize>,
    pub active_thread: Option<ThreadDetailSnapshot>,
    pub active_duty: Option<DutyViewSnapshot>,
    pub active_swarm: Option<SwarmViewSnapshot>,
    /// The selected goal swarm's view (F16); its rows share the Swarm view.
    pub active_goal_swarm: Option<crate::goal_swarm_view::GoalSwarmViewSnapshot>,
    pub default_model_pill: ModelPillView,
    pub search: String,
    pub groups: Vec<SidebarGroup>,
    /// The Board view's lanes (F1). They mirror `groups`, `tasks` and `issues`, so they are
    /// always current, whichever view is showing.
    pub board: Vec<crate::board::BoardLane>,
    pub pulse: Vec<PulseCount>,
    pub show_numbers: bool,
    pub palette_open: bool,
    pub new_project_open: bool,
    pub duty_dialog_open: bool,
    pub duty_dialog_role: DutyRole,
    /// The open Swarm dialog: its starting source, and the swarm + Items adds to (F10).
    pub swarm_dialog: Option<SwarmDialogRequest>,
    /// The open Goal swarm dialog and the prompt it starts with (F16).
    pub goal_dialog: Option<crate::goal_swarm::GoalDialogRequest>,
    pub palette_query: String,
    pub palette_results: Vec<PaletteItem>,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub palette_selection: usize,
    pub slash_items: Vec<SlashCommandItem>,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub slash_selection: usize,
    pub tasks: Vec<TaskView>,
    pub pull_requests: Vec<RailItem>,
    pub issues: Vec<RailItem>,
    pub worktree_label: String,
    pub providers: Vec<ProviderUsage>,
    pub airc_open: bool,
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
    pub toast: Option<ToastView>,
}

/// The bottom-centre toast. `identifier` changes with every new toast so the window can
/// time it out; one with `undo_deletion` offers Undo until that deletion lands (F12).
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToastView {
    pub identifier: u32,
    pub message: String,
    pub undo_deletion: Option<u32>,
}

impl ToastView {
    pub fn new(identifier: u32, message: String, undo_deletion: Option<u32>) -> Self {
        ToastView {
            identifier,
            message,
            undo_deletion,
        }
    }
}
