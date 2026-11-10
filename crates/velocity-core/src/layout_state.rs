//! Backend-owned main-window state. No native effects or persistence live here.

use crate::airc_modal::AircBadge;
use crate::build_gate::{BuildKind, LaunchKind, launch_block_reason};
use crate::duty::{self, DutyRole, DutyViewSnapshot};
use crate::goal_swarm::GoalDialogRequest;
use crate::goal_swarm_view::{self, GoalSwarmViewSnapshot};
use crate::interview_view;
use crate::layout::{self, ThreadGroup};
use crate::layout_catalog::{self, GROUPS, PULSE};
use crate::layout_view::*;
use crate::navigation::{
    self, KeyStroke, NavigationIntent, NavigationView, ShortcutContext, ShortcutKey,
};
use crate::swarm::{self, SwarmDialogRequest, SwarmSource, SwarmViewSnapshot};
use crate::thread_view::{self, ComposerSubmitEffect};
use crate::thread_view_model::{
    self, AgentCommandView, LiveThreadView, ModelPillView, SlashCommandItem, ThreadDetailSnapshot,
    TranscriptBlock,
};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone)]
pub struct LayoutState {
    data: LayoutData,
    issue_rows: BTreeMap<u32, Vec<RailItem>>,
    /// Running sessions' transcripts by `(project, thread)`. Kept apart from `data` because the
    /// store rebuilds `data` from `state.json`, which has no transcript, after every change.
    live: BTreeMap<(u32, usize), LiveThreadView>,
    revision: u32,
    view: NavigationView,
    settings_tab: String,
    selected_thread: Option<usize>,
    search: String,
    show_numbers: bool,
    palette_open: bool,
    palette_query: String,
    palette_selection: usize,
    slash_selection: usize,
    new_project_open: bool,
    duty_dialog_open: bool,
    duty_dialog_role: DutyRole,
    swarm_dialog: Option<SwarmDialogRequest>,
    goal_dialog: Option<GoalDialogRequest>,
    /// The AIRC modal (F14). Its contents arrive on `airc://modal`, not in this snapshot.
    airc_open: bool,
    /// Footer badges by project, from the channel; kept apart from `data`, which the store
    /// rebuilds from `state.json`.
    airc_badges: BTreeMap<u32, AircBadge>,
    form_open: bool,
    overlay_open: bool,
    composer_text: String,
    task_text: String,
    project_spec_locked: Option<bool>,
    launch_notice: Option<String>,
    /// `(project, thread)` rows deleted but still inside their undo window (F12). Kept apart
    /// from `data` for the same reason as `live`: the store does not know about them yet.
    hidden_threads: BTreeSet<(u32, usize)>,
    toast: Option<ToastView>,
    toast_count: u32,
}

impl Default for LayoutState {
    fn default() -> Self {
        Self {
            data: LayoutData::default(),
            issue_rows: BTreeMap::new(),
            live: BTreeMap::new(),
            revision: 0,
            view: NavigationView::Home,
            settings_tab: "worktrees".into(),
            selected_thread: None,
            search: String::new(),
            show_numbers: false,
            palette_open: false,
            palette_query: String::new(),
            palette_selection: 0,
            slash_selection: 0,
            new_project_open: false,
            duty_dialog_open: false,
            duty_dialog_role: DutyRole::PrDuty,
            swarm_dialog: None,
            goal_dialog: None,
            airc_open: false,
            airc_badges: BTreeMap::new(),
            form_open: false,
            overlay_open: false,
            composer_text: String::new(),
            task_text: String::new(),
            project_spec_locked: None,
            launch_notice: None,
            hidden_threads: BTreeSet::new(),
            toast: None,
            toast_count: 0,
        }
    }
}

impl LayoutState {
    pub fn set_project_issues(&mut self, project: u32, issues: Vec<RailItem>) {
        self.issue_rows.insert(project, issues);
        if self.data.project_identifier == Some(project) {
            self.changed();
        }
    }

    fn project_issues(&self) -> &[RailItem] {
        self.data
            .project_identifier
            .and_then(|project| self.issue_rows.get(&project))
            .map_or(&self.data.issues, Vec::as_slice)
    }

    pub fn search_query(&self) -> &str {
        &self.search
    }

    pub fn selected_thread(&self) -> Option<usize> {
        self.selected_thread
    }

    /// Restore the selected project's retained view and discard transient input
    /// that could otherwise send a prompt or task to a different project.
    pub fn restore_project(
        &mut self,
        data: LayoutData,
        selected_thread: Option<usize>,
        search: String,
    ) {
        self.data = data;
        self.hide_pending_rows();
        self.search = search;
        self.palette_open = false;
        self.palette_query.clear();
        self.palette_selection = 0;
        self.duty_dialog_open = false;
        self.goal_dialog = None;
        self.airc_open = false;
        self.form_open = false;
        self.overlay_open = false;
        self.composer_text.clear();
        self.task_text.clear();
        self.show_numbers = false;
        self.selected_thread = None;
        self.view = NavigationView::Home;
        self.launch_notice = None;
        if let Some(identifier) = selected_thread {
            self.open_thread(identifier);
        }
        self.changed();
    }

    pub fn set_worktree_summary(
        &mut self,
        view: &crate::worktree_view::WorktreeSettingsView,
    ) -> bool {
        let count = view.rows.iter().filter(|row| !row.deleted).count();
        if self.data.worktree_count == count && self.data.worktree_drive == view.footer_drive {
            return false;
        }
        self.data.worktree_count = count;
        self.data.worktree_drive = view.footer_drive.clone();
        self.changed();
        true
    }

    /// Only the backend's persisted project projection supplies this context.
    pub fn set_project_spec_locked(&mut self, locked: Option<bool>) {
        if self.project_spec_locked != locked {
            self.project_spec_locked = locked;
            self.launch_notice = None;
            self.changed();
        }
    }

    /// Feature owners replace their read model after domain events, then emit a snapshot.
    pub fn replace_data(&mut self, data: LayoutData) {
        self.data = data;
        self.hide_pending_rows();
        self.follow_selected_view();
        self.deselect_hidden();
        self.palette_selection =
            layout::bounded_selection(self.palette_results().len(), self.palette_selection);
        self.changed();
    }

    pub fn set_task_rows(&mut self, tasks: Vec<crate::layout_view::TaskView>) {
        self.data.completed_task_count = tasks.iter().filter(|task| task.state == "done").count();
        self.data.tasks = tasks;
        self.changed();
    }

    /// Deleted threads leave the sidebar at once and come back on Undo (F12).
    pub fn set_threads_hidden(&mut self, project: u32, identifiers: &[usize], hidden: bool) {
        for identifier in identifiers {
            if hidden {
                self.hidden_threads.insert((project, *identifier));
            } else {
                self.hidden_threads.remove(&(project, *identifier));
            }
        }
        if Some(project) == self.data.project_identifier {
            for thread in &mut self.data.threads {
                if identifiers.contains(&thread.policy.identifier) {
                    thread.policy.hidden = hidden;
                }
            }
        }
        self.deselect_hidden();
        self.changed();
    }

    /// A short message, or a deletion's Undo offer, replacing any toast already showing.
    pub fn show_toast(&mut self, message: String, undo_deletion: Option<u32>) {
        self.toast_count = self.toast_count.wrapping_add(1);
        self.toast = Some(ToastView::new(self.toast_count, message, undo_deletion));
        self.changed();
    }

    /// The deletion landed or was undone, so its Undo is no longer on offer.
    pub fn clear_deletion_toast(&mut self, deletion: u32) {
        if self
            .toast
            .as_ref()
            .is_some_and(|toast| toast.undo_deletion == Some(deletion))
        {
            self.toast = None;
            self.changed();
        }
    }

    fn hide_pending_rows(&mut self) {
        let Some(project) = self.data.project_identifier else {
            return;
        };
        for thread in &mut self.data.threads {
            if self
                .hidden_threads
                .contains(&(project, thread.policy.identifier))
            {
                thread.policy.hidden = true;
            }
        }
    }

    fn deselect_hidden(&mut self) {
        if self.selected_thread.is_some_and(|identifier| {
            !self
                .data
                .threads
                .iter()
                .any(|thread| thread.policy.identifier == identifier && !thread.policy.hidden)
        }) {
            self.selected_thread = None;
            self.view = NavigationView::Home;
        }
    }

    /// A running session's transcript and commands for one thread (`SPEC.md §3.2`, F4).
    pub fn set_live_thread(&mut self, project: u32, identifier: usize, view: LiveThreadView) {
        self.live.insert((project, identifier), view);
        self.changed();
    }

    /// Opens a thread the backend just started, as if it had been clicked (F4).
    pub fn select_thread(&mut self, identifier: usize) {
        self.open_thread(identifier);
        self.changed();
    }

    fn live_thread(&self, identifier: usize) -> Option<&LiveThreadView> {
        let project = self.data.project_identifier?;
        self.live.get(&(project, identifier))
    }

    /// A spec interview moves from scan to ask to ready while it is open, and each phase
    /// takes different keys (§5), so the open thread's view follows its latest phase.
    fn follow_selected_view(&mut self) {
        let Some(identifier) = self.selected_thread else {
            return;
        };
        let follows = matches!(
            self.view,
            NavigationView::Thread
                | NavigationView::Duty
                | NavigationView::Swarm
                | NavigationView::SpecScan
                | NavigationView::SpecAsk
                | NavigationView::SpecReady
                | NavigationView::SpecLocked
        );
        if let Some(entry) = self
            .data
            .threads
            .iter()
            .find(|thread| thread.policy.identifier == identifier)
            && follows
        {
            self.view = entry.view;
        }
    }

    /// Live agent output that `state.json` does not keep: the transcript streamed so far
    /// and whether a turn is running.
    pub fn set_thread_live(
        &mut self,
        identifier: usize,
        blocks: Vec<TranscriptBlock>,
        agent_busy: bool,
    ) {
        if let Some(thread) = self
            .data
            .threads
            .iter_mut()
            .find(|thread| thread.policy.identifier == identifier)
            && let Some(detail) = thread.detail.as_mut()
        {
            detail.blocks = blocks;
            detail.agent_busy = agent_busy;
            self.changed();
        }
    }

    /// Create closes the New project dialog once the project is set up (F2).
    pub fn close_new_project(&mut self) {
        self.new_project_open = false;
        self.changed();
    }

    /// Starting or adding duty agents closes the Start a duty agent dialog and selects the role (F7).
    pub fn close_duty_dialog_and_select(&mut self, identifier: usize) {
        self.duty_dialog_open = false;
        self.open_thread(identifier);
        self.changed();
    }

    /// Launching a swarm, or adding items to one, closes the Swarm dialog and selects the
    /// swarm (F10).
    pub fn close_swarm_dialog_and_select(&mut self, identifier: usize) {
        self.swarm_dialog = None;
        self.open_thread(identifier);
        self.changed();
    }

    /// Launching a goal swarm closes the dialog, clears the Home composer that prefilled
    /// its prompt, and selects the launched member when available (F16).
    pub fn close_goal_dialog_after_launch(&mut self, select: Option<usize>) {
        self.goal_dialog = None;
        if matches!(self.view, NavigationView::Home) {
            self.composer_text.clear();
        }
        if let Some(identifier) = select {
            self.open_thread(identifier);
        }
        self.changed();
    }

    /// One overlay at a time: opening a dialog closes the palette and every other dialog.
    fn close_overlays(&mut self) {
        self.palette_open = false;
        self.new_project_open = false;
        self.duty_dialog_open = false;
        self.swarm_dialog = None;
        self.goal_dialog = None;
        self.airc_open = false;
    }

    fn open_duty_dialog(&mut self, role: DutyRole) {
        self.close_overlays();
        self.duty_dialog_role = role;
        self.duty_dialog_open = true;
    }

    fn open_swarm_dialog(&mut self, source: SwarmSource, into: Option<usize>) {
        self.close_overlays();
        self.swarm_dialog = Some(SwarmDialogRequest::new(source, into));
    }

    /// `G`, the starter chip or the palette: the Goal swarm dialog, its prompt prefilled
    /// with what was typed in the empty composer (F16).
    fn open_goal_dialog(&mut self) {
        let typed = if matches!(self.view, NavigationView::Home) {
            self.composer_text.as_str()
        } else {
            ""
        };
        let request = GoalDialogRequest::new(typed);
        self.close_overlays();
        self.goal_dialog = Some(request);
    }

    /// + Items opens the dialog on the swarm's own source, adding to it (F10).
    fn add_swarm_items(&mut self, identifier: usize) {
        if let Some(entry) = self
            .data
            .swarms
            .iter()
            .find(|entry| entry.identifier == identifier)
        {
            self.open_swarm_dialog(entry.source, Some(identifier));
        }
    }

    /// + Agents opens the Goal swarm dialog on that swarm's prompt, adding to it (F16).
    fn add_goal_agents(&mut self, identifier: usize) {
        if let Some(entry) = self
            .data
            .goal_swarms
            .iter()
            .find(|entry| entry.identifier == identifier)
        {
            let request = GoalDialogRequest::add_agents(&entry.prompt, identifier);
            self.close_overlays();
            self.goal_dialog = Some(request);
        }
    }

    /// `A` or the footer link: the modal replaces any other dialog, as in the reference.
    fn open_airc(&mut self) {
        self.close_overlays();
        self.airc_open = true;
    }

    pub fn airc_open(&self) -> bool {
        self.airc_open
    }

    /// Records a project's footer badge; returns whether the window needs a new snapshot.
    pub fn set_airc_badge(&mut self, project: u32, badge: AircBadge) -> bool {
        if self.airc_badges.get(&project) == Some(&badge) {
            return false;
        }
        self.airc_badges.insert(project, badge);
        let shown = self.data.project_identifier == Some(project);
        if shown {
            self.changed();
        }
        shown
    }

    fn airc_badge(&self) -> AircBadge {
        self.data
            .project_identifier
            .and_then(|project| self.airc_badges.get(&project).copied())
            .unwrap_or_default()
    }

    /// Views and dialogs register their context so shortcuts cannot escape an overlay.
    pub fn set_context(&mut self, view: NavigationView, overlay_open: bool, form_open: bool) {
        self.view = view;
        self.overlay_open = overlay_open;
        self.form_open = form_open;
        self.changed();
    }

    pub fn apply(&mut self, action: LayoutAction) -> Option<LayoutIntent> {
        let intent = self.reduce(action);
        let intent = intent.and_then(|intent| self.guard_intent(intent));
        self.changed();
        intent
    }

    fn reduce(&mut self, action: LayoutAction) -> Option<LayoutIntent> {
        match action {
            LayoutAction::ComposerText(_)
            | LayoutAction::TaskText(_)
            | LayoutAction::SlashSelect(_)
            | LayoutAction::SlashNext
            | LayoutAction::SlashPrevious
            | LayoutAction::SlashComplete
            | LayoutAction::ToggleShowWork => {
                self.edit_composer(action);
                None
            }
            LayoutAction::SubmitComposer => self.submit_composer(),
            LayoutAction::SubmitTask => self.submit_task(),
            LayoutAction::Request(intent) => self.request(intent),
            LayoutAction::SidebarSearch(query) => {
                self.search = query;
                None
            }
            LayoutAction::PaletteSearch(query) => {
                self.palette_query = query;
                self.palette_selection = 0;
                None
            }
            LayoutAction::PaletteSelect(selected) => {
                self.palette_selection =
                    layout::bounded_selection(self.palette_results().len(), selected);
                None
            }
            LayoutAction::PaletteNext => {
                self.move_palette(true);
                None
            }
            LayoutAction::PalettePrevious => {
                self.move_palette(false);
                None
            }
            LayoutAction::PaletteAccept => self.accept_palette(),
            LayoutAction::OpenThread(identifier) => self.open_thread(identifier),
            LayoutAction::ToggleSwarm(identifier) => {
                self.open_swarm(identifier);
                None
            }
            LayoutAction::AddSwarmItems(identifier) => {
                self.add_swarm_items(identifier);
                None
            }
            LayoutAction::AddGoalAgents(identifier) => {
                self.add_goal_agents(identifier);
                None
            }
            LayoutAction::Keyboard(input) => self.keyboard(input),
            LayoutAction::NumberHints(event) => {
                self.show_numbers = navigation::update_number_hints(self.show_numbers, event);
                None
            }
        }
    }

    fn edit_composer(&mut self, action: LayoutAction) {
        match action {
            LayoutAction::ComposerText(text) => {
                self.composer_text = text;
                self.slash_selection = 0;
            }
            LayoutAction::TaskText(text) => self.task_text = text,
            LayoutAction::SlashSelect(selected) => self.select_slash(selected),
            LayoutAction::SlashNext => self.move_slash(true),
            LayoutAction::SlashPrevious => self.move_slash(false),
            LayoutAction::SlashComplete => self.complete_slash(),
            LayoutAction::ToggleShowWork => self.toggle_show_work(),
            _ => {}
        }
    }

    pub fn snapshot(&self) -> LayoutSnapshot {
        let slash_items = self.current_slash_items();
        let slash_selection = if slash_items.is_empty() {
            0
        } else {
            thread_view::clamp_slash_selection(self.slash_selection, slash_items.len())
        };
        LayoutSnapshot {
            revision: self.revision,
            project_identifier: self.data.project_identifier,
            project_name: self.data.project_name.clone(),
            view: self.view,
            settings_tab: self.settings_tab.clone(),
            selected_thread: self.selected_thread,
            active_thread: self.active_thread_snapshot(),
            active_duty: self.active_duty_snapshot(),
            active_swarm: self.active_swarm_snapshot(),
            active_goal_swarm: self.active_goal_swarm_snapshot(),
            default_model_pill: self.default_model_pill(),
            search: self.search.clone(),
            groups: self.sidebar(),
            pulse: self.pulse(),
            show_numbers: self.show_numbers,
            palette_open: self.palette_open,
            new_project_open: self.new_project_open,
            duty_dialog_open: self.duty_dialog_open,
            duty_dialog_role: self.duty_dialog_role,
            swarm_dialog: self.swarm_dialog,
            goal_dialog: self.goal_dialog.clone(),
            palette_query: self.palette_query.clone(),
            palette_results: self.palette_results(),
            palette_selection: self.palette_selection,
            slash_items,
            slash_selection,
            tasks: self.tasks(),
            pull_requests: self.data.pull_requests.clone(),
            issues: self.project_issues().to_vec(),
            worktree_label: self.worktree_label(),
            providers: self.data.providers.clone(),
            airc_open: self.airc_open,
            airc_unread: self.airc_badge().unread,
            overlap_unread: self.airc_badge().alert,
            composer_text: self.composer_text.clone(),
            task_text: self.task_text.clone(),
            build_allowed: self.build_block_reason().is_none(),
            build_block_reason: self.build_block_reason().map(str::to_owned),
            composer_allowed: self.composer_block_reason().is_none(),
            composer_block_reason: self.composer_block_reason().map(str::to_owned),
            blocked_navigation: self.blocked_navigation(),
            tasks_draft: self.project_spec_locked == Some(false),
            task_count_label: self.task_count_label(),
            launch_notice: self.launch_notice.clone(),
            toast: self.toast.clone(),
        }
    }

    fn guard_intent(&mut self, intent: LayoutIntent) -> Option<LayoutIntent> {
        let interview = match &intent {
            LayoutIntent::SteerThread { identifier, .. } => self.is_interview_thread(*identifier),
            _ => false,
        };
        match crate::layout_guard::guard_intent(intent, self.project_spec_locked, interview) {
            Ok(intent) => {
                self.launch_notice = None;
                Some(intent)
            }
            Err(reason) => {
                self.launch_notice = Some(reason.into());
                None
            }
        }
    }

    fn build_block_reason(&self) -> Option<&'static str> {
        launch_block_reason(
            LaunchKind::Build(BuildKind::Thread),
            self.project_spec_locked,
        )
    }

    fn is_interview_thread(&self, identifier: usize) -> bool {
        self.data.threads.iter().any(|thread| {
            thread.policy.identifier == identifier
                && matches!(
                    thread.view,
                    NavigationView::SpecScan
                        | NavigationView::SpecAsk
                        | NavigationView::SpecReady
                        | NavigationView::SpecLocked
                )
        })
    }

    fn composer_block_reason(&self) -> Option<&'static str> {
        let is_draft = self.selected_thread.is_none() || matches!(self.view, NavigationView::Home);
        let (effect, _, _) =
            thread_view_model::evaluate_composer_submission(&self.composer_text, is_draft, false);
        if matches!(effect, ComposerSubmitEffect::AddTask) {
            return None;
        }
        let kind = if self
            .selected_thread
            .is_some_and(|identifier| self.is_interview_thread(identifier))
        {
            LaunchKind::Interview
        } else {
            LaunchKind::Build(BuildKind::Thread)
        };
        launch_block_reason(kind, self.project_spec_locked)
    }

    fn blocked_navigation(&self) -> Vec<NavigationIntent> {
        [NavigationIntent::Fanout]
            .into_iter()
            .filter(|intent| {
                crate::layout_guard::guard_intent(
                    LayoutIntent::Navigation(*intent),
                    self.project_spec_locked,
                    false,
                )
                .is_err()
            })
            .collect()
    }

    fn tasks(&self) -> Vec<crate::layout_view::TaskView> {
        self.data.tasks.clone()
    }

    fn task_count_label(&self) -> String {
        if self.data.tasks.is_empty() {
            String::new()
        } else if self.project_spec_locked == Some(false) {
            format!("draft · {}", self.data.tasks.len())
        } else {
            format!(
                "{}/{}",
                self.data.completed_task_count,
                self.data.tasks.len()
            )
        }
    }

    fn changed(&mut self) {
        self.revision = self.revision.saturating_add(1);
    }

    fn active_thread_snapshot(&self) -> Option<ThreadDetailSnapshot> {
        let identifier = self.selected_thread?;
        let entry = self
            .data
            .threads
            .iter()
            .find(|thread| thread.policy.identifier == identifier && !thread.policy.hidden)?;
        let detail = entry.detail.as_ref()?;
        let mut snapshot = thread_view_model::project_thread_detail(detail);
        if let Some(live) = self.live_thread(identifier) {
            snapshot.blocks = live.blocks.clone();
            if let Some(commands) = &live.agent_commands {
                snapshot.agent_commands = commands.clone();
            }
            if let Some(model) = &live.titling_model {
                snapshot.titling = true;
                snapshot.title_model_name = model.clone();
            }
        }

        if let Some(interview) = self
            .data
            .interviews
            .iter()
            .find(|interview| interview.thread_identifier == identifier)
        {
            snapshot.composer_placeholder =
                interview_view::interview_placeholder(interview.view.phase).into();
            snapshot.interview = Some(interview.view.clone());
        }
        Some(snapshot)
    }

    fn active_duty_snapshot(&self) -> Option<DutyViewSnapshot> {
        let identifier = self.selected_thread?;
        let entry = self
            .data
            .threads
            .iter()
            .find(|thread| thread.policy.identifier == identifier && !thread.policy.hidden)?;
        let duty = entry.duty.as_ref()?;
        let default_pill = self.default_model_pill();
        Some(duty::project_duty_view(
            duty,
            self.data.pull_requests.len(),
            self.data.merged_today,
            &default_pill,
        ))
    }

    fn active_swarm_snapshot(&self) -> Option<SwarmViewSnapshot> {
        let identifier = self.selected_thread?;
        let entry = self
            .data
            .swarms
            .iter()
            .find(|entry| entry.identifier == identifier)?;
        Some(swarm::project_swarm_view(entry))
    }

    fn active_goal_swarm_snapshot(&self) -> Option<GoalSwarmViewSnapshot> {
        let identifier = self.selected_thread?;
        let entry = self
            .data
            .goal_swarms
            .iter()
            .find(|entry| entry.identifier == identifier)?;
        Some(goal_swarm_view::project_goal_swarm_view(entry))
    }

    fn default_model_pill(&self) -> ModelPillView {
        if let Some(pill) = &self.data.default_model_pill {
            return pill.clone();
        }
        let state = thread_view::project_model_pill(false, true);
        ModelPillView {
            provider: ProviderColor::Claude,
            model_name: "Opus 5.5".into(),
            effort_label: "High".into(),
            show_fast_badge: state.show_fast_badge,
            speed_control_enabled: state.speed_control_enabled,
            preserve_requested_fast: state.preserve_requested_fast,
        }
    }

    fn active_agent_commands(&self) -> &[AgentCommandView] {
        if let Some(identifier) = self.selected_thread
            && let Some(commands) = self
                .live_thread(identifier)
                .and_then(|live| live.agent_commands.as_ref())
        {
            return commands;
        }
        if let Some(identifier) = self.selected_thread
            && let Some(entry) = self
                .data
                .threads
                .iter()
                .find(|thread| thread.policy.identifier == identifier && !thread.policy.hidden)
            && let Some(detail) = entry.detail.as_ref()
        {
            return &detail.agent_commands;
        }
        &self.data.agent_commands
    }

    fn current_slash_items(&self) -> Vec<SlashCommandItem> {
        thread_view_model::slash_menu_items_with_agent(
            &self.composer_text,
            self.active_agent_commands(),
        )
    }

    /// Updates the advertised slash commands (`available_commands_update`, `SPEC.md §3.2`) for a thread.
    pub fn set_thread_agent_commands(
        &mut self,
        identifier: usize,
        commands: Vec<AgentCommandView>,
    ) {
        if let Some(thread) = self
            .data
            .threads
            .iter_mut()
            .find(|thread| thread.policy.identifier == identifier)
            && let Some(detail) = thread.detail.as_mut()
        {
            detail.agent_commands = commands;
            self.changed();
        }
    }

    fn select_slash(&mut self, selected: usize) {
        let count = self.current_slash_items().len();
        self.slash_selection = if count == 0 {
            0
        } else {
            thread_view::clamp_slash_selection(selected, count)
        };
    }

    fn move_slash(&mut self, down: bool) {
        let count = self.current_slash_items().len();
        self.slash_selection = thread_view::move_slash_selection(self.slash_selection, count, down);
    }

    fn complete_slash(&mut self) {
        let items = self.current_slash_items();
        if items.is_empty() {
            return;
        }
        let index = thread_view::clamp_slash_selection(self.slash_selection, items.len());
        if let Some(item) = items.get(index) {
            self.composer_text = format!("{} ", item.command);
            self.slash_selection = 0;
        }
    }

    fn toggle_show_work(&mut self) {
        let Some(identifier) = self.selected_thread else {
            return;
        };
        if let Some(thread) = self
            .data
            .threads
            .iter_mut()
            .find(|thread| thread.policy.identifier == identifier)
            && let Some(detail) = thread.detail.as_mut()
        {
            detail.show_work = !detail.show_work;
        }
    }

    fn submit_task(&mut self) -> Option<LayoutIntent> {
        let intent = nonempty_request(&self.task_text, LayoutIntent::AddTask)?;
        self.task_text.clear();
        Some(intent)
    }

    fn submit_composer(&mut self) -> Option<LayoutIntent> {
        let is_draft = self.selected_thread.is_none() || matches!(self.view, NavigationView::Home);
        let agent_busy = self
            .selected_thread
            .and_then(|identifier| {
                self.data
                    .threads
                    .iter()
                    .find(|thread| thread.policy.identifier == identifier)
            })
            .and_then(|thread| thread.detail.as_ref())
            .is_some_and(|detail| detail.agent_busy);
        let (effect, payload, queued) = thread_view_model::evaluate_composer_submission(
            &self.composer_text,
            is_draft,
            agent_busy,
        );
        let text = payload?;
        let intent = match effect {
            ComposerSubmitEffect::Nothing => return None,
            ComposerSubmitEffect::AddTask => LayoutIntent::AddTask(text),
            ComposerSubmitEffect::StartThreadFromDraft => LayoutIntent::StartPrompt(text),
            ComposerSubmitEffect::SendPromptNow | ComposerSubmitEffect::QueueForNextStep => {
                LayoutIntent::SteerThread {
                    identifier: self.selected_thread?,
                    prompt: text,
                    queued,
                }
            }
        };
        let intent = self.guard_intent(intent)?;
        self.composer_text.clear();
        self.slash_selection = 0;
        self.record_steering(&intent);
        Some(intent)
    }

    fn record_steering(&mut self, intent: &LayoutIntent) {
        if let LayoutIntent::SteerThread {
            identifier,
            prompt,
            queued,
        } = intent
            && let Some(thread) = self
                .data
                .threads
                .iter_mut()
                .find(|thread| thread.policy.identifier == *identifier)
            && let Some(detail) = thread.detail.as_mut()
        {
            detail.blocks.push(TranscriptBlock::UserMessage {
                text: prompt.clone(),
                queued: *queued,
            });
        }
    }

    fn request(&mut self, intent: LayoutIntent) -> Option<LayoutIntent> {
        if let LayoutIntent::Navigation(navigation) = intent {
            return self.navigate(navigation);
        }
        if let LayoutIntent::SwarmIssues = intent {
            self.open_swarm_dialog(SwarmSource::Issues, None);
            return None;
        }
        if let LayoutIntent::SettingsTab(tab) = &intent
            && tab == "agents"
        {
            self.view = NavigationView::Agents;
        }
        if let LayoutIntent::SettingsTab(tab) = &intent
            && tab == "worktrees"
        {
            self.view = NavigationView::Settings;
            self.settings_tab = tab.clone();
        }
        Some(intent)
    }

    fn navigate(&mut self, intent: NavigationIntent) -> Option<LayoutIntent> {
        match intent {
            NavigationIntent::Palette => {
                self.palette_open = !self.palette_open;
                self.palette_query.clear();
                self.palette_selection = 0;
            }
            NavigationIntent::Dispatch => {
                self.palette_open = true;
                self.palette_query = "#".into();
                self.palette_selection = 0;
                return None;
            }
            NavigationIntent::Dismiss => self.close_overlays(),
            NavigationIntent::NewProject => {
                self.close_overlays();
                self.new_project_open = true;
            }
            NavigationIntent::Swarm => self.open_swarm_dialog(SwarmSource::Tasks, None),
            NavigationIntent::Goal => self.open_goal_dialog(),
            NavigationIntent::Airc => self.open_airc(),
            NavigationIntent::Settings => self.view = NavigationView::Settings,
            NavigationIntent::PrDuty => self.open_duty_dialog(DutyRole::PrDuty),
            NavigationIntent::Scouts => self.open_duty_dialog(DutyRole::Scouts),
            NavigationIntent::ShowNumbers => self.show_numbers = true,
            NavigationIntent::Jump(number) => {
                let visible: Vec<_> = self
                    .sidebar()
                    .into_iter()
                    .flat_map(|group| group.rows)
                    .map(|row| row.identifier)
                    .collect();
                return layout::alt_target(number, &visible)
                    .and_then(|identifier| self.open_thread(identifier));
            }
            NavigationIntent::NewThread => {
                self.selected_thread = None;
                self.view = NavigationView::Home;
            }
            _ => {}
        }
        Some(LayoutIntent::Navigation(intent))
    }

    fn keyboard(&mut self, input: KeyboardInput) -> Option<LayoutIntent> {
        if self.palette_open && !input.control && !input.alt && !input.meta_held {
            match input.key.as_str() {
                "ArrowDown" => {
                    self.move_palette(true);
                    return None;
                }
                "ArrowUp" => {
                    self.move_palette(false);
                    return None;
                }
                "Enter" => return self.accept_palette(),
                _ => {}
            }
        }
        let context = ShortcutContext {
            view: self.view,
            typing: input.typing,
            overlay_open: self.overlay_open
                || self.palette_open
                || self.new_project_open
                || self.duty_dialog_open
                || self.swarm_dialog.is_some()
                || self.goal_dialog.is_some()
                || self.airc_open,
            // Dialogs with a primary action accept Ctrl+Enter (F2, F7, F10, F16).
            form_open: self.form_open
                || self.new_project_open
                || self.duty_dialog_open
                || self.swarm_dialog.is_some()
                || self.goal_dialog.is_some(),
        };
        let stroke = KeyStroke {
            key: shortcut_key(&input),
            control: input.control,
            shift: input.shift,
            alt: input.alt,
            meta_held: input.meta_held,
        };
        navigation::shortcut_intent(context, stroke).and_then(|intent| self.navigate(intent))
    }

    fn move_palette(&mut self, next: bool) {
        let count = self.palette_results().len();
        self.palette_selection = if next {
            layout::next_selection(count, self.palette_selection)
        } else {
            layout::previous_selection(count, self.palette_selection)
        };
    }

    fn accept_palette(&mut self) -> Option<LayoutIntent> {
        let entry = self
            .palette_results()
            .get(self.palette_selection)
            .cloned()?;
        self.palette_open = false;
        match entry.target {
            PaletteTarget::Intent(intent) => self.request(intent),
            PaletteTarget::Thread(identifier) => self.open_thread(identifier),
        }
    }

    fn open_thread(&mut self, identifier: usize) -> Option<LayoutIntent> {
        let entry = self
            .data
            .threads
            .iter()
            .find(|thread| thread.policy.identifier == identifier)?;
        if entry.policy.hidden {
            return None;
        }
        self.selected_thread = Some(identifier);
        self.view = entry.view;
        // Selection is published in layout://state; feature views consume it there.
        None
    }

    fn open_swarm(&mut self, identifier: usize) {
        // Retain the action for existing callers; expansion is owned by selection.
        if self.data.threads.iter().any(|thread| {
            thread.policy.identifier == identifier
                && matches!(thread.policy.group, ThreadGroup::Swarm)
        }) {
            self.open_thread(identifier);
        }
    }

    fn selected_row(&self) -> Option<layout::LayoutRow> {
        self.selected_thread.and_then(|identifier| {
            self.data
                .threads
                .iter()
                .find(|thread| thread.policy.identifier == identifier)
                .map(|thread| thread.policy)
        })
    }

    fn sidebar(&self) -> Vec<SidebarGroup> {
        // The reference trims the sidebar query, so a query of only spaces shows every thread.
        let query = normalized(self.search.trim());
        let selected = self.selected_row();
        let mut number = 0;
        GROUPS
            .into_iter()
            .filter_map(|group| {
                let roots: Vec<_> = self
                    .data
                    .threads
                    .iter()
                    .filter(|thread| {
                        let mut row = thread.policy;
                        row.matches_search =
                            layout::substring_match(&query, &normalized(&thread.title));
                        layout::is_root_visible(group, row)
                    })
                    .collect();
                if !layout::show_group(group, !roots.is_empty(), query.is_empty()) {
                    return None;
                }
                let count = roots.len();
                let rows = self.sidebar_rows(&roots, selected, &mut number);
                Some(SidebarGroup {
                    group,
                    title: layout_catalog::group_title(group).into(),
                    count,
                    rows,
                })
            })
            .collect()
    }

    fn sidebar_rows(
        &self,
        roots: &[&ThreadEntry],
        selected: Option<layout::LayoutRow>,
        number: &mut usize,
    ) -> Vec<SidebarRow> {
        let mut rows = Vec::new();
        for root in roots {
            let expanded = layout::expand_members(root.policy, selected);
            rows.push(self.sidebar_row(root, false, expanded, number));
            if expanded {
                for member in self.data.threads.iter().filter(|thread| {
                    layout::is_member_visible(root.policy.identifier, thread.policy)
                }) {
                    rows.push(self.sidebar_row(member, true, false, number));
                }
            }
        }
        rows
    }

    fn sidebar_row(
        &self,
        entry: &ThreadEntry,
        member: bool,
        expanded: bool,
        number: &mut usize,
    ) -> SidebarRow {
        *number = number.saturating_add(1);
        SidebarRow {
            identifier: entry.policy.identifier,
            title: entry.title.clone(),
            titling: entry.detail.as_ref().is_some_and(|detail| detail.titling)
                || self
                    .live_thread(entry.policy.identifier)
                    .is_some_and(|live| live.titling_model.is_some()),
            metadata: entry.metadata.clone(),
            group: entry.policy.group,
            duty_role: entry.duty.as_ref().map(|duty| duty.role),
            member,
            expanded,
            active: self.selected_thread == Some(entry.policy.identifier),
            paused: entry.policy.paused,
            agents: entry.policy.agent_count,
            number: if *number <= 9 { Some(*number) } else { None },
            segments: self.row_segments(entry.policy.identifier),
            goal: self
                .data
                .goal_swarms
                .iter()
                .any(|goal| goal.identifier == entry.policy.identifier),
        }
    }

    /// A swarm or goal swarm row's segment bar, one segment per member or agent.
    fn row_segments(&self, identifier: usize) -> Vec<swarm::SwarmStage> {
        if let Some(goal) = self
            .data
            .goal_swarms
            .iter()
            .find(|goal| goal.identifier == identifier)
        {
            return goal_swarm_view::goal_segments(&goal.agents);
        }
        self.data
            .swarms
            .iter()
            .find(|swarm| swarm.identifier == identifier)
            .map(|swarm| swarm::segments(&swarm.members))
            .unwrap_or_default()
    }

    fn pulse(&self) -> Vec<PulseCount> {
        PULSE
            .into_iter()
            .filter_map(|(kind, label)| {
                let count = if matches!(kind, layout::PulseKind::MergedToday) {
                    self.data.merged_today
                } else {
                    self.data
                        .threads
                        .iter()
                        .map(|thread| layout::pulse_contribution(kind, thread.policy))
                        .fold(0_usize, usize::saturating_add)
                };
                if count == 0 {
                    None
                } else {
                    Some(PulseCount {
                        kind,
                        label: label.into(),
                        count,
                    })
                }
            })
            .collect()
    }

    fn palette_results(&self) -> Vec<PaletteItem> {
        let query = normalized(&self.palette_query);
        let mut items = layout_catalog::commands();
        items.extend(self.data.projects.iter().cloned());
        items.extend(
            self.data
                .threads
                .iter()
                .filter(|thread| {
                    !thread.policy.hidden && !matches!(thread.policy.group, ThreadGroup::Draft)
                })
                .map(|thread| PaletteItem {
                    identifier: format!("thread:{}", thread.policy.identifier),
                    label: thread.title.clone(),
                    group: "thread".into(),
                    shortcut: String::new(),
                    target: PaletteTarget::Thread(thread.policy.identifier),
                }),
        );
        items.extend(self.project_issues().iter().map(|issue| PaletteItem {
            identifier: format!("issue:{}", issue.identifier),
            label: format!("#{} {}", issue.identifier, issue.title),
            group: "issue".into(),
            shortcut: String::new(),
            target: PaletteTarget::Intent(LayoutIntent::DispatchIssue(issue.identifier)),
        }));
        items
            .into_iter()
            .filter(|item| layout::fuzzy_match(&query, &normalized(&item.label)))
            .collect()
    }

    fn worktree_label(&self) -> String {
        if self.data.worktree_drive.is_empty() {
            format!("{} worktrees", self.data.worktree_count)
        } else {
            format!(
                "{} on {}",
                self.data.worktree_count, self.data.worktree_drive
            )
        }
    }
}

fn normalized(text: &str) -> Vec<char> {
    text.to_lowercase().chars().collect()
}

fn nonempty_request(text: &str, request: fn(String) -> LayoutIntent) -> Option<LayoutIntent> {
    if text.trim().is_empty() {
        None
    } else {
        Some(request(text.to_owned()))
    }
}

fn shortcut_key(input: &KeyboardInput) -> ShortcutKey {
    use ShortcutKey::*;
    if let Some(digit) = input
        .code
        .strip_prefix("Digit")
        .and_then(|digit| digit.parse::<usize>().ok())
    {
        return Digit(digit);
    }
    match input.key.to_lowercase().as_str() {
        "k" => K,
        "n" => N,
        "," => Comma,
        "alt" => Alt,
        "/" => Slash,
        "t" => T,
        "w" => W,
        "g" => G,
        "a" => A,
        "escape" => Escape,
        "enter" => Enter,
        "i" => I,
        "r" => R,
        "f" => F,
        "v" => V,
        "s" => S,
        "l" => L,
        "d" => D,
        "e" => E,
        _ => Other,
    }
}
