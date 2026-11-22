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
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TaskView {
    pub identifier: String,
    pub title: String,
    pub state: String,
    pub metadata: String,
    pub thread_identifier: Option<String>,
    pub attribution: Option<String>,
    pub attribution_thread: Option<String>,
    pub completed_at: Option<u32>,
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
            completed_at: None,
        }
    }
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosenWorkView {
    pub items: Vec<crate::work_pool::WorkItem>,
    pub expanded_issues: Vec<crate::work_pool::WorkKey>,
}

/// A project's Rosen server that still runs another build than this Velocity's, because it
/// could not be replaced at start (#515). The next start of Velocity tries again.
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RosenServerNotice {
    /// `running Rosen cd537b7; restart to update`.
    pub headline: String,
    /// Why the server was not replaced.
    pub reason: String,
}

impl RosenServerNotice {
    pub fn new(headline: String, reason: String) -> Self {
        Self { headline, reason }
    }
}

/// Feature owners publish their real data here; the shell invents no activity.
#[derive(Clone, Default)]
pub struct LayoutData {
    /// The persisted project these rows belong to; keys the live thread overlay.
    pub project_identifier: Option<u32>,
    pub project_name: String,
    pub rosen_mirror: Option<crate::rosen_mirror::MirrorState>,
    /// The base and mirror branch diverged when the project last caught up (#501).
    pub rosen_mirror_divergence: Option<crate::rosen_mirror::MirrorDivergence>,
    /// The project's Collapse mirror as `owner/repository`; `None` hides Open on Collapse.
    pub collapse_repository: Option<String>,
    /// The landed `rosen.toml` names the Collapse root, so Rosen sends Collapse its live
    /// state; `false` with a repository offers Send Collapse its live state (#517).
    pub collapse_live_state: bool,
    /// Collapse's last land and downstream forwarder for that repository (#427).
    pub collapse_mirror_health: Option<crate::collapse::CollapseMirrorHealth>,
    /// GitHub CI on the project's mirrored base, as last read (#507).
    pub base_ci: Option<crate::base_ci::BaseCi>,
    pub rosen_work: Option<RosenWorkView>,
    pub rosen_migration_notice: Option<String>,
    pub settled_expanded: bool,
    pub threads: Vec<ThreadEntry>,
    pub projects: Vec<PaletteItem>,
    pub tasks: Vec<TaskView>,
    pub completed_task_count: usize,
    pub changes: Vec<RailItem>,
    pub issues: Vec<RailItem>,
    pub merged_today: usize,
    pub providers: Vec<ProviderUsage>,
    pub default_model_pill: Option<ModelPillView>,
    pub composer_model_pill: Option<ModelPillView>,
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
    pub account_key: String,
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
            account_key: String::new(),
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarRow {
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub identifier: usize,
    pub title: String,
    /// The title model hasn't answered yet: the title is the message's first words, pulsing
    /// (F4).
    pub titling: bool,
    pub metadata: String,
    pub change_stage: Option<crate::thread_view_model::RosenChangeStage>,
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
    /// How many subagents this thread currently has running (F4).
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub running_subagents: usize,
}

#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LayoutIntent {
    Navigation(NavigationIntent),
    SwitchProject(String),
    DispatchIssue(
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        usize,
    ),
    SwarmIssues,
    SwarmWork,
    OpenTask(
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        usize,
    ),
    OpenIssue(
        #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
        #[cfg_attr(feature = "bindings", specta(type = String))]
        usize,
    ),
    OpenWorkspaceInExplorer(String),
    /// The project's Collapse repository page, or a landed commit's page (#429).
    OpenOnCollapse(Option<String>),
    SettingsTab(String),
    Usage,
    Updates,
    StartPrompt(String),
    /// Start a read-only question thread with no workspace, change or claim (#533).
    StartQuestion(String),
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq, Eq)]
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
    ToggleDraftReadOnly,
    ClearDraftContext,
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
    /// The selected project's Rosen event stream is reconnecting.
    pub rosen_reconnecting: bool,
    /// The selected project's Rosen server could not be replaced with this build (#515).
    pub rosen_server_notice: Option<RosenServerNotice>,
    pub project_identifier: Option<u32>,
    pub project_name: String,
    pub rosen_mirror: Option<crate::rosen_mirror::MirrorState>,
    /// The project's Rosen base trails its mirror while a catch-up is pending (#520).
    pub rosen_mirror_behind: Option<crate::rosen_mirror::MirrorBehind>,
    /// The base and mirror branch diverged when the project last caught up (#501).
    pub rosen_mirror_divergence: Option<crate::rosen_mirror::MirrorDivergence>,
    /// The project's Collapse mirror as `owner/repository`; `None` hides Open on Collapse.
    pub collapse_repository: Option<String>,
    /// The landed `rosen.toml` names the Collapse root, so Rosen sends Collapse its live
    /// state; `false` with a repository offers Send Collapse its live state (#517).
    pub collapse_live_state: bool,
    /// Collapse's last land and downstream forwarder for that repository (#427).
    pub collapse_mirror_health: Option<crate::collapse::CollapseMirrorHealth>,
    /// GitHub CI on the project's mirrored base, as last read (#507).
    pub base_ci: Option<crate::base_ci::BaseCi>,
    pub rosen_work: Option<RosenWorkView>,
    pub rosen_migration_notice: Option<String>,
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
    pub composer_model_pill: ModelPillView,
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
    pub changes: Vec<RailItem>,
    pub issues: Vec<RailItem>,
    pub workspace_label: String,
    pub providers: Vec<ProviderUsage>,
    pub airc_open: bool,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub airc_unread: usize,
    pub overlap_unread: bool,
    pub composer_text: String,
    /// Optional validated context attached to the unsent home draft (e.g. `"Land abc1234"`, #479).
    pub draft_context: Option<String>,
    /// True when the home composer is toggled to read-only question mode (#533).
    pub draft_read_only: bool,
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

/// Non-list shell state sent in a [`LayoutDelta`] only when one of its fields changed (#553).
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LayoutChrome {
    pub rosen_reconnecting: bool,
    pub rosen_server_notice: Option<RosenServerNotice>,
    pub project_identifier: Option<u32>,
    pub project_name: String,
    pub rosen_mirror: Option<crate::rosen_mirror::MirrorState>,
    pub rosen_mirror_behind: Option<crate::rosen_mirror::MirrorBehind>,
    pub rosen_mirror_divergence: Option<crate::rosen_mirror::MirrorDivergence>,
    pub collapse_repository: Option<String>,
    pub collapse_live_state: bool,
    pub collapse_mirror_health: Option<crate::collapse::CollapseMirrorHealth>,
    pub base_ci: Option<crate::base_ci::BaseCi>,
    pub rosen_migration_notice: Option<String>,
    pub view: NavigationView,
    pub settings_tab: String,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number::optional"))]
    #[cfg_attr(feature = "bindings", specta(type = Option<String>))]
    pub selected_thread: Option<usize>,
    pub default_model_pill: ModelPillView,
    pub composer_model_pill: ModelPillView,
    pub search: String,
    pub show_numbers: bool,
    pub new_project_open: bool,
    pub duty_dialog_open: bool,
    pub duty_dialog_role: DutyRole,
    pub swarm_dialog: Option<SwarmDialogRequest>,
    pub goal_dialog: Option<crate::goal_swarm::GoalDialogRequest>,
    pub slash_items: Vec<SlashCommandItem>,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub slash_selection: usize,
    pub workspace_label: String,
    pub providers: Vec<ProviderUsage>,
    pub airc_open: bool,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub airc_unread: usize,
    pub overlap_unread: bool,
    pub composer_text: String,
    pub draft_context: Option<String>,
    pub draft_read_only: bool,
    pub task_text: String,
    pub build_allowed: bool,
    pub build_block_reason: Option<String>,
    pub composer_allowed: bool,
    pub composer_block_reason: Option<String>,
    pub blocked_navigation: Vec<NavigationIntent>,
    pub launch_notice: Option<String>,
    pub toast: Option<ToastView>,
}

impl LayoutChrome {
    pub fn from_snapshot(snapshot: &LayoutSnapshot) -> Self {
        Self {
            rosen_reconnecting: snapshot.rosen_reconnecting,
            rosen_server_notice: snapshot.rosen_server_notice.clone(),
            project_identifier: snapshot.project_identifier,
            project_name: snapshot.project_name.clone(),
            rosen_mirror: snapshot.rosen_mirror.clone(),
            rosen_mirror_behind: snapshot.rosen_mirror_behind.clone(),
            rosen_mirror_divergence: snapshot.rosen_mirror_divergence.clone(),
            collapse_repository: snapshot.collapse_repository.clone(),
            collapse_live_state: snapshot.collapse_live_state,
            collapse_mirror_health: snapshot.collapse_mirror_health.clone(),
            base_ci: snapshot.base_ci.clone(),
            rosen_migration_notice: snapshot.rosen_migration_notice.clone(),
            view: snapshot.view,
            settings_tab: snapshot.settings_tab.clone(),
            selected_thread: snapshot.selected_thread,
            default_model_pill: snapshot.default_model_pill.clone(),
            composer_model_pill: snapshot.composer_model_pill.clone(),
            search: snapshot.search.clone(),
            show_numbers: snapshot.show_numbers,
            new_project_open: snapshot.new_project_open,
            duty_dialog_open: snapshot.duty_dialog_open,
            duty_dialog_role: snapshot.duty_dialog_role,
            swarm_dialog: snapshot.swarm_dialog,
            goal_dialog: snapshot.goal_dialog.clone(),
            slash_items: snapshot.slash_items.clone(),
            slash_selection: snapshot.slash_selection,
            workspace_label: snapshot.workspace_label.clone(),
            providers: snapshot.providers.clone(),
            airc_open: snapshot.airc_open,
            airc_unread: snapshot.airc_unread,
            overlap_unread: snapshot.overlap_unread,
            composer_text: snapshot.composer_text.clone(),
            draft_context: snapshot.draft_context.clone(),
            draft_read_only: snapshot.draft_read_only,
            task_text: snapshot.task_text.clone(),
            build_allowed: snapshot.build_allowed,
            build_block_reason: snapshot.build_block_reason.clone(),
            composer_allowed: snapshot.composer_allowed,
            composer_block_reason: snapshot.composer_block_reason.clone(),
            blocked_navigation: snapshot.blocked_navigation.clone(),
            launch_notice: snapshot.launch_notice.clone(),
            toast: snapshot.toast.clone(),
        }
    }
}

/// Sidebar thread groups and title-bar pulse counts, sent only when rows change (#553).
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SidebarSlice {
    pub groups: Vec<SidebarGroup>,
    pub pulse: Vec<PulseCount>,
}

/// Right-rail tasks, issues, changes and Rosen work pool, sent only when changed (#553).
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RailSlice {
    pub tasks: Vec<TaskView>,
    pub changes: Vec<RailItem>,
    pub issues: Vec<RailItem>,
    pub tasks_draft: bool,
    pub task_count_label: String,
    pub rosen_work: Option<RosenWorkView>,
}

impl RailSlice {
    pub fn from_snapshot(snapshot: &LayoutSnapshot) -> Self {
        Self {
            tasks: snapshot.tasks.clone(),
            changes: snapshot.changes.clone(),
            issues: snapshot.issues.clone(),
            tasks_draft: snapshot.tasks_draft,
            task_count_label: snapshot.task_count_label.clone(),
            rosen_work: snapshot.rosen_work.clone(),
        }
    }
}

/// Command palette state; `palette_results` is populated only while `palette_open` is true (#553).
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaletteSlice {
    pub palette_open: bool,
    pub palette_query: String,
    pub palette_results: Vec<PaletteItem>,
    #[cfg_attr(feature = "bindings", serde(with = "crate::ipc_number"))]
    #[cfg_attr(feature = "bindings", specta(type = String))]
    pub palette_selection: usize,
}

impl PaletteSlice {
    pub fn from_snapshot(snapshot: &LayoutSnapshot) -> Self {
        Self {
            palette_open: snapshot.palette_open,
            palette_query: snapshot.palette_query.clone(),
            palette_results: snapshot.palette_results.clone(),
            palette_selection: snapshot.palette_selection,
        }
    }
}

/// Incremental update to the main window's [`LayoutSnapshot`] (#553).
///
/// Only slices that actually changed since `base_revision` are present; untouched slices
/// are `None` so the frontend keeps their existing object references without deep-comparing.
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug)]
pub struct LayoutDelta {
    pub base_revision: u32,
    pub revision: u32,
    #[cfg_attr(
        feature = "bindings",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub chrome: Option<LayoutChrome>,
    #[cfg_attr(
        feature = "bindings",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub sidebar: Option<SidebarSlice>,
    #[cfg_attr(
        feature = "bindings",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub board: Option<Vec<crate::board::BoardLane>>,
    #[cfg_attr(
        feature = "bindings",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub rail: Option<RailSlice>,
    #[cfg_attr(
        feature = "bindings",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub palette: Option<PaletteSlice>,
    #[cfg_attr(
        feature = "bindings",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub active_thread_meta: Option<crate::thread_view_model::ThreadDetailMeta>,
    #[cfg_attr(
        feature = "bindings",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub active_thread_blocks: Option<crate::thread_view_model::TranscriptUpdate>,
    #[cfg_attr(feature = "bindings", serde(default))]
    pub active_thread_cleared: bool,
    #[cfg_attr(
        feature = "bindings",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub active_duty: Option<DutyViewSnapshot>,
    #[cfg_attr(feature = "bindings", serde(default))]
    pub active_duty_cleared: bool,
    #[cfg_attr(
        feature = "bindings",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub active_swarm: Option<SwarmViewSnapshot>,
    #[cfg_attr(feature = "bindings", serde(default))]
    pub active_swarm_cleared: bool,
    #[cfg_attr(
        feature = "bindings",
        serde(default, skip_serializing_if = "Option::is_none")
    )]
    pub active_goal_swarm: Option<crate::goal_swarm_view::GoalSwarmViewSnapshot>,
    #[cfg_attr(feature = "bindings", serde(default))]
    pub active_goal_swarm_cleared: bool,
}

impl LayoutDelta {
    pub fn is_empty(&self) -> bool {
        self.chrome.is_none()
            && self.sidebar.is_none()
            && self.board.is_none()
            && self.rail.is_none()
            && self.palette.is_none()
            && self.active_thread_meta.is_none()
            && self.active_thread_blocks.is_none()
            && !self.active_thread_cleared
            && self.active_duty.is_none()
            && !self.active_duty_cleared
            && self.active_swarm.is_none()
            && !self.active_swarm_cleared
            && self.active_goal_swarm.is_none()
            && !self.active_goal_swarm_cleared
    }
}
