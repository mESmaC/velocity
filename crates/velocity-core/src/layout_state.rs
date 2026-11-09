//! Backend-owned main-window state. No native effects or persistence live here.

use crate::build_gate::{BuildKind, LaunchKind, launch_block_reason};
use crate::layout::{self, ThreadGroup};
use crate::layout_catalog::{self, GROUPS, PULSE};
use crate::layout_view::*;
use crate::navigation::{
    self, KeyStroke, NavigationIntent, NavigationView, ShortcutContext, ShortcutKey,
};
use crate::thread_view::{self, ComposerSubmitEffect};
use crate::thread_view_model::{
    self, AgentCommandView, ModelPillView, SlashCommandItem, ThreadDetailSnapshot, TranscriptBlock,
};

#[derive(Clone)]
pub struct LayoutState {
    data: LayoutData,
    revision: u32,
    view: NavigationView,
    selected_thread: Option<usize>,
    search: String,
    show_numbers: bool,
    palette_open: bool,
    palette_query: String,
    palette_selection: usize,
    slash_selection: usize,
    new_project_open: bool,
    form_open: bool,
    overlay_open: bool,
    composer_text: String,
    task_text: String,
    project_spec_locked: Option<bool>,
    launch_notice: Option<String>,
}

impl Default for LayoutState {
    fn default() -> Self {
        Self {
            data: LayoutData::default(),
            revision: 0,
            view: NavigationView::Home,
            selected_thread: None,
            search: String::new(),
            show_numbers: false,
            palette_open: false,
            palette_query: String::new(),
            palette_selection: 0,
            slash_selection: 0,
            new_project_open: false,
            form_open: false,
            overlay_open: false,
            composer_text: String::new(),
            task_text: String::new(),
            project_spec_locked: None,
            launch_notice: None,
        }
    }
}

impl LayoutState {
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
        self.search = search;
        self.palette_open = false;
        self.palette_query.clear();
        self.palette_selection = 0;
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
        self.palette_selection =
            layout::bounded_selection(self.palette_results().len(), self.palette_selection);
        self.changed();
    }

    /// Create closes the New project dialog once the project is set up (F2).
    pub fn close_new_project(&mut self) {
        self.new_project_open = false;
        self.changed();
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
            project_name: self.data.project_name.clone(),
            view: self.view,
            selected_thread: self.selected_thread,
            active_thread: self.active_thread_snapshot(),
            default_model_pill: self.default_model_pill(),
            search: self.search.clone(),
            groups: self.sidebar(),
            pulse: self.pulse(),
            show_numbers: self.show_numbers,
            palette_open: self.palette_open,
            new_project_open: self.new_project_open,
            palette_query: self.palette_query.clone(),
            palette_results: self.palette_results(),
            palette_selection: self.palette_selection,
            slash_items,
            slash_selection,
            tasks: self.tasks(),
            pull_requests: self.data.pull_requests.clone(),
            issues: self.data.issues.clone(),
            worktree_label: self.worktree_label(),
            providers: self.data.providers.clone(),
            airc_unread: self.data.airc_unread,
            overlap_unread: self.data.overlap_unread,
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

    fn tasks(&self) -> Vec<RailItem> {
        let mut tasks = self.data.tasks.clone();
        if self.project_spec_locked == Some(false) {
            for task in &mut tasks {
                task.metadata = "lock the spec first".into();
            }
        }
        tasks
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
        Some(thread_view_model::project_thread_detail(detail))
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
        Some(intent)
    }

    fn navigate(&mut self, intent: NavigationIntent) -> Option<LayoutIntent> {
        match intent {
            NavigationIntent::Palette => {
                self.palette_open = !self.palette_open;
                self.palette_query.clear();
                self.palette_selection = 0;
            }
            NavigationIntent::Dismiss => {
                self.palette_open = false;
                self.new_project_open = false;
            }
            NavigationIntent::NewProject => {
                self.palette_open = false;
                self.new_project_open = true;
            }
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
            overlay_open: self.overlay_open || self.palette_open || self.new_project_open,
            // The New project dialog is a form: Ctrl+Enter submits it (F2).
            form_open: self.form_open || self.new_project_open,
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
            titling: entry.detail.as_ref().is_some_and(|detail| detail.titling),
            metadata: entry.metadata.clone(),
            group: entry.policy.group,
            member,
            expanded,
            active: self.selected_thread == Some(entry.policy.identifier),
            paused: entry.policy.paused,
            agents: entry.policy.agent_count,
            number: if *number <= 9 { Some(*number) } else { None },
        }
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
        items.extend(self.data.issues.iter().map(|issue| PaletteItem {
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
