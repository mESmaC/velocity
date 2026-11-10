//! Persisted duty roles and atomic store transitions (`SPEC.md §3.1`, `§4 F7`, `§7 M5`).

use crate::project_layout::{active_project, provider_color, read_model};
use crate::projects::ProjectError;
use crate::{
    PersistedState, ProjectItem, ProviderKind, StateStore, ThreadState, write_atomic_snapshot,
};
use serde::{Deserialize, Serialize};
use specta::Type;
use velocity_core::duty::{
    self, DutyAgentCard, DutyEventKind, DutyFeedEvent, DutyRole, DutyRoleEntry, DutyStartSpec,
    PrDutyRuleSettings, ScoutRuleSettings,
};
use velocity_core::layout_state::LayoutState;
use velocity_core::layout_view::LayoutSnapshot;
use velocity_core::scout_sweep::SweepRecord;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DutyAgentItem {
    pub nickname: String,
    pub provider: ProviderKind,
    pub model_name: String,
    pub busy: bool,
    pub doing: String,
    /// A scout's last sweep (F7), so its cadence and worktree survive a restart.
    #[serde(default)]
    #[specta(skip)]
    pub sweep: Option<SweepRecord>,
    /// A PR duty agent's kept `review-pr-<n>` worktrees (F7), listed in Settings → Worktrees.
    #[serde(default)]
    #[specta(skip)]
    pub review_worktrees: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DutyFeedItem {
    pub timestamp: String,
    pub actor: String,
    pub kind: DutyEventKind,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct DutyRoleItem {
    pub id: u32,
    pub role: DutyRole,
    pub paused: bool,
    pub issues_filed: u32,
    pub pr_rules: PrDutyRuleSettings,
    pub scout_rules: ScoutRuleSettings,
    pub agents: Vec<DutyAgentItem>,
    pub feed: Vec<DutyFeedItem>,
}

impl DutyRoleItem {
    pub fn to_core(&self) -> DutyRoleEntry {
        DutyRoleEntry {
            identifier: self.id as usize,
            role: self.role,
            paused: self.paused,
            issues_filed: self.issues_filed as usize,
            pr_rules: self.pr_rules.clone(),
            scout_rules: self.scout_rules.clone(),
            agents: self
                .agents
                .iter()
                .map(|agent| DutyAgentCard {
                    nickname: agent.nickname.clone(),
                    provider: provider_color(agent.provider),
                    model_name: agent.model_name.clone(),
                    busy: agent.busy,
                    doing: agent.doing.clone(),
                    sweep: agent.sweep.clone(),
                    review_worktrees: agent.review_worktrees.clone(),
                })
                .collect(),
            feed: self
                .feed
                .iter()
                .map(|event| DutyFeedEvent {
                    timestamp: event.timestamp.clone(),
                    actor: event.actor.clone(),
                    kind: event.kind,
                    message: event.message.clone(),
                })
                .collect(),
        }
    }

    fn from_core(entry: DutyRoleEntry, provider_lookup: ProviderKind) -> Self {
        Self {
            id: u32::try_from(entry.identifier).unwrap_or(u32::MAX),
            role: entry.role,
            paused: entry.paused,
            issues_filed: u32::try_from(entry.issues_filed).unwrap_or(u32::MAX),
            pr_rules: entry.pr_rules,
            scout_rules: entry.scout_rules,
            agents: entry
                .agents
                .into_iter()
                .map(|agent| DutyAgentItem {
                    nickname: agent.nickname,
                    provider: provider_from_color(agent.provider, provider_lookup),
                    model_name: agent.model_name,
                    busy: agent.busy,
                    doing: agent.doing,
                    sweep: agent.sweep,
                    review_worktrees: agent.review_worktrees,
                })
                .collect(),
            feed: entry
                .feed
                .into_iter()
                .map(|event| DutyFeedItem {
                    timestamp: event.timestamp,
                    actor: event.actor,
                    kind: event.kind,
                    message: event.message,
                })
                .collect(),
        }
    }
}

fn provider_from_color(
    color: velocity_core::layout_view::ProviderColor,
    fallback: ProviderKind,
) -> ProviderKind {
    use velocity_core::layout_view::ProviderColor;
    match color {
        ProviderColor::Claude => ProviderKind::Claude,
        ProviderColor::Codex => ProviderKind::Codex,
        ProviderColor::Antigravity => ProviderKind::Antigravity,
        ProviderColor::Kimi => ProviderKind::Kimi,
        // A colour doesn't say which agent you added; the record's own provider stands.
        ProviderColor::Custom => fallback,
    }
    .into_with_fallback(fallback)
}

trait IntoWithFallback {
    fn into_with_fallback(self, fallback: Self) -> Self;
}

impl IntoWithFallback for ProviderKind {
    fn into_with_fallback(self, _fallback: Self) -> Self {
        self
    }
}

/// How many activity events a duty role keeps.
const FEED_LIMIT: usize = 200;

/// A project's Scouts as the sweep runner reads them.
#[derive(Clone)]
pub struct ScoutRoster {
    /// Settings when the roster was read: each agent's ACP command comes from Providers.
    pub settings: crate::Settings,
    pub project: ProjectItem,
    pub worktrees: velocity_core::worktree_settings::WorktreePreferences,
    pub paused: bool,
    pub rules: ScoutRuleSettings,
    pub scouts: Vec<DutyAgentItem>,
}

/// A project's PR duty as the review runner reads it.
#[derive(Clone)]
pub struct PrDutyRoster {
    /// Settings when the roster was read: each agent's ACP command comes from Providers.
    pub settings: crate::Settings,
    pub project: ProjectItem,
    pub worktrees: velocity_core::worktree_settings::WorktreePreferences,
    pub paused: bool,
    pub rules: PrDutyRuleSettings,
    pub agents: Vec<DutyAgentItem>,
}

/// A project's Main watcher as the watch runner reads it.
#[derive(Clone)]
pub struct WatchRoster {
    /// Settings when the roster was read: each agent's ACP command comes from Providers.
    pub settings: crate::Settings,
    pub project: ProjectItem,
    pub worktrees: velocity_core::worktree_settings::WorktreePreferences,
    pub paused: bool,
    pub watchers: Vec<DutyAgentItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct DutyDialogDefaults {
    pub provider: ProviderKind,
    /// The provider's name, including an ACP agent you added (F15).
    #[serde(default)]
    pub provider_name: String,
    pub model_name: String,
    pub model_configured: bool,
    pub pr_duty_exists: bool,
    pub scouts_exists: bool,
    pub main_watcher_exists: bool,
    pub pr_rules: PrDutyRuleSettings,
    pub scout_rules: ScoutRuleSettings,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct StartDutyRequest {
    pub role: DutyRole,
    pub agent_count: u8,
    pub provider: Option<ProviderKind>,
    pub model_name: Option<String>,
    pub pr_rules: Option<PrDutyRuleSettings>,
    pub scout_rules: Option<ScoutRuleSettings>,
}

impl StartDutyRequest {
    /// Add `agent_count` PR duty agents on the default model, keeping the role's rules.
    pub fn pr_duty(agent_count: u8) -> Self {
        Self {
            role: DutyRole::PrDuty,
            agent_count,
            provider: None,
            model_name: None,
            pr_rules: None,
            scout_rules: None,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct PauseDutyRequest {
    pub role: Option<DutyRole>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct DutyMutationOutcome {
    pub snapshot: LayoutSnapshot,
    pub role: DutyRole,
    pub event: DutyFeedEvent,
}

impl StateStore {
    pub fn duty_dialog_defaults(&self) -> DutyDialogDefaults {
        let settings = &self.state.settings;
        // A signed-out default gives way to the first signed-in provider (F15 Sign out).
        let provider = settings.new_work_choice();
        let model_configured = provider == settings.providers.default_provider
            && settings
                .providers
                .default_model
                .as_ref()
                .is_some_and(|name| !name.trim().is_empty());
        let model_name = settings.provider_model(provider);
        let project = active_project(&self.state);
        let existing_pr = project.and_then(|proj| find_role(proj, DutyRole::PrDuty));
        let existing_scouts = project.and_then(|proj| find_role(proj, DutyRole::Scouts));
        DutyDialogDefaults {
            provider,
            provider_name: settings.provider_name(provider),
            model_name,
            model_configured,
            pr_duty_exists: existing_pr.is_some(),
            scouts_exists: existing_scouts.is_some(),
            main_watcher_exists: project
                .and_then(|proj| find_role(proj, DutyRole::MainWatcher))
                .is_some(),
            pr_rules: existing_pr
                .map(|item| item.pr_rules.clone())
                .unwrap_or_default(),
            scout_rules: existing_scouts
                .map(|item| item.scout_rules.clone())
                .unwrap_or_default(),
        }
    }

    pub fn start_or_add_duty(
        &mut self,
        layout: &mut LayoutState,
        request: StartDutyRequest,
        timestamp: String,
    ) -> Result<DutyMutationOutcome, ProjectError> {
        let mut next = self.state.clone();
        let defaults = self.duty_dialog_defaults();
        let project = active_project_mut(&mut next)?;
        let provider = request.provider.unwrap_or(defaults.provider);
        self.state.settings.ensure_new_work(provider)?;
        let model_name = request
            .model_name
            .clone()
            .filter(|name| !name.trim().is_empty())
            .unwrap_or(defaults.model_name);
        let has_open_pull_requests = project.threads.iter().any(|thread| {
            thread.state == ThreadState::Review && thread.pull_request_number.is_some()
        });
        let (identifier, event) = apply_start_or_add(
            project,
            &request,
            provider,
            model_name,
            has_open_pull_requests,
            timestamp,
        );
        project.active_thread_id = Some(identifier);
        self.commit_duty_state(next)?;
        layout.replace_data(read_model(&self.state));
        layout.close_duty_dialog_and_select(identifier as usize);
        crate::build_gate::sync_layout(&self.state, layout);
        Ok(DutyMutationOutcome {
            snapshot: layout.snapshot(),
            role: request.role,
            event,
        })
    }

    pub fn toggle_pause_duty(
        &mut self,
        layout: &mut LayoutState,
        request: PauseDutyRequest,
        timestamp: String,
    ) -> Result<DutyMutationOutcome, ProjectError> {
        let mut next = self.state.clone();
        let selected = layout.selected_thread();
        let project = active_project_mut(&mut next)?;
        let target_index = resolve_duty_index(project, request.role, selected)
            .ok_or(ProjectError::IndexOutOfRange)?;
        let item = &mut project.duty_roles[target_index];
        let mut core_entry = item.to_core();
        let event = duty::toggle_duty_pause(&mut core_entry, timestamp);
        let role = core_entry.role;
        *item = DutyRoleItem::from_core(core_entry, self.state.settings.providers.default_provider);
        self.commit_duty_state(next)?;
        layout.replace_data(read_model(&self.state));
        crate::build_gate::sync_layout(&self.state, layout);
        Ok(DutyMutationOutcome {
            snapshot: layout.snapshot(),
            role,
            event,
        })
    }

    pub fn record_duty_composer_message(
        &mut self,
        layout: &mut LayoutState,
        identifier: usize,
        prompt: String,
        timestamp: String,
    ) -> Result<Option<(DutyRole, DutyFeedEvent)>, ProjectError> {
        let Some(project) = active_project(&self.state) else {
            return Ok(None);
        };
        let target_id = u32::try_from(identifier).ok();
        if !project
            .duty_roles
            .iter()
            .any(|duty| Some(duty.id) == target_id)
        {
            return Ok(None);
        }
        let mut next = self.state.clone();
        let project = active_project_mut(&mut next)?;
        let Some(item) = project
            .duty_roles
            .iter_mut()
            .find(|duty| Some(duty.id) == target_id)
        else {
            return Ok(None);
        };
        let mut core_entry = item.to_core();
        let event = duty::record_duty_instruction(&mut core_entry, prompt, timestamp);
        let role = core_entry.role;
        *item = DutyRoleItem::from_core(core_entry, self.state.settings.providers.default_provider);
        self.commit_duty_state(next)?;
        layout.replace_data(read_model(&self.state));
        crate::build_gate::sync_layout(&self.state, layout);
        Ok(Some((role, event)))
    }

    /// Every project's Scouts role, for the sweep runner (F7), paused ones included so their
    /// cards still say so.
    pub fn scout_rosters(&self) -> Vec<ScoutRoster> {
        self.state
            .projects
            .iter()
            .filter_map(|project| {
                let role = find_role(project, DutyRole::Scouts)?;
                Some(ScoutRoster {
                    settings: self.state.settings.clone(),
                    project: project.clone(),
                    worktrees: self.worktree_preferences(project),
                    paused: role.paused,
                    rules: role.scout_rules.clone(),
                    scouts: role.agents.clone(),
                })
            })
            .collect()
    }

    /// Every project's PR duty role, for the review runner (F7), paused ones included so their
    /// cards still say so.
    pub fn pr_duty_rosters(&self) -> Vec<PrDutyRoster> {
        self.state
            .projects
            .iter()
            .filter_map(|project| {
                let role = find_role(project, DutyRole::PrDuty)?;
                Some(PrDutyRoster {
                    settings: self.state.settings.clone(),
                    project: project.clone(),
                    worktrees: self.worktree_preferences(project),
                    paused: role.paused,
                    rules: role.pr_rules.clone(),
                    agents: role.agents.clone(),
                })
            })
            .collect()
    }

    /// Every project's Main watcher, for the watch runner (F7), paused ones included so their
    /// cards still say so.
    pub fn main_watch_rosters(&self) -> Vec<WatchRoster> {
        self.state
            .projects
            .iter()
            .filter_map(|project| {
                let role = find_role(project, DutyRole::MainWatcher)?;
                Some(WatchRoster {
                    settings: self.state.settings.clone(),
                    project: project.clone(),
                    worktrees: self.worktree_preferences(project),
                    paused: role.paused,
                    watchers: role.agents.clone(),
                })
            })
            .collect()
    }

    /// Changes a project's Scouts role through `change`, which is given the feed's timestamp
    /// and returns the feed events to post, and saves it. Returns the posted events, or `None`
    /// when the project or its Scouts role is gone (deleted while a sweep ran).
    pub fn update_scouts(
        &mut self,
        project: u32,
        change: impl FnOnce(&mut DutyRoleEntry, &str) -> Vec<DutyFeedEvent>,
    ) -> Result<Option<Vec<DutyFeedEvent>>, ProjectError> {
        self.update_duty(project, DutyRole::Scouts, change)
    }

    /// [`Self::update_scouts`] for any duty role: PR duty's reviews report the same way.
    pub fn update_duty(
        &mut self,
        project: u32,
        role: DutyRole,
        change: impl FnOnce(&mut DutyRoleEntry, &str) -> Vec<DutyFeedEvent>,
    ) -> Result<Option<Vec<DutyFeedEvent>>, ProjectError> {
        let mut next = self.state.clone();
        let Some(item) = next
            .projects
            .iter_mut()
            .find(|item| item.id == project)
            .and_then(|item| item.duty_roles.iter_mut().find(|entry| entry.role == role))
        else {
            return Ok(None);
        };
        let mut entry = item.to_core();
        let events = change(&mut entry, &clock_timestamp());
        entry.feed.extend(events.iter().cloned());
        // Sweeps run for as long as the app does; the feed keeps the recent part.
        let overflow = entry.feed.len().saturating_sub(FEED_LIMIT);
        entry.feed.drain(..overflow);
        let fallback = self.state.settings.providers.default_provider;
        let updated = DutyRoleItem::from_core(entry, fallback);
        if *item != updated {
            *item = updated;
            self.commit_duty_state(next)?;
        }
        Ok(Some(events))
    }

    fn commit_duty_state(&mut self, mut next: PersistedState) -> Result<(), ProjectError> {
        next.revision = self.state.revision.wrapping_add(1);
        write_atomic_snapshot(&self.directory, &next)?;
        self.disk_revision = next.revision;
        self.state = next;
        Ok(())
    }
}

fn active_project_mut(state: &mut PersistedState) -> Result<&mut ProjectItem, ProjectError> {
    let index =
        usize::try_from(state.active_project_index).map_err(|_| ProjectError::IndexOutOfRange)?;
    state
        .projects
        .get_mut(index)
        .ok_or(ProjectError::IndexOutOfRange)
}

fn find_role(project: &ProjectItem, role: DutyRole) -> Option<&DutyRoleItem> {
    project.duty_roles.iter().find(|item| item.role == role)
}

fn resolve_duty_index(
    project: &ProjectItem,
    role: Option<DutyRole>,
    selected: Option<usize>,
) -> Option<usize> {
    if let Some(role) = role {
        return project.duty_roles.iter().position(|item| item.role == role);
    }
    let selected_id = selected.and_then(|id| u32::try_from(id).ok())?;
    project
        .duty_roles
        .iter()
        .position(|item| item.id == selected_id)
}

fn next_project_entry_id(project: &ProjectItem) -> u32 {
    crate::threads::highest_entry_identifier(project)
        .saturating_add(1)
        .max(1)
}

fn apply_start_or_add(
    project: &mut ProjectItem,
    request: &StartDutyRequest,
    provider: ProviderKind,
    model_name: String,
    has_open_pull_requests: bool,
    timestamp: String,
) -> (u32, DutyFeedEvent) {
    if let Some(existing) = project
        .duty_roles
        .iter_mut()
        .find(|item| item.role == request.role)
    {
        let mut core_entry = existing.to_core();
        let spec = DutyStartSpec {
            role: request.role,
            agent_count: usize::from(request.agent_count),
            provider: provider_color(provider),
            model_name,
            pr_rules: request
                .pr_rules
                .clone()
                .unwrap_or_else(|| core_entry.pr_rules.clone()),
            scout_rules: request
                .scout_rules
                .clone()
                .unwrap_or_else(|| core_entry.scout_rules.clone()),
            has_open_pull_requests,
            timestamp,
        };
        let event = duty::add_agents_to_duty_entry(&mut core_entry, spec);
        let id = existing.id;
        *existing = DutyRoleItem::from_core(core_entry, provider);
        (id, event)
    } else {
        let id = next_project_entry_id(project);
        let spec = DutyStartSpec {
            role: request.role,
            agent_count: usize::from(request.agent_count),
            provider: provider_color(provider),
            model_name,
            pr_rules: request.pr_rules.clone().unwrap_or_default(),
            scout_rules: request.scout_rules.clone().unwrap_or_default(),
            has_open_pull_requests,
            timestamp,
        };
        let (core_entry, event) = duty::start_duty_entry(id as usize, spec);
        project
            .duty_roles
            .push(DutyRoleItem::from_core(core_entry, provider));
        (id, event)
    }
}

pub fn clock_timestamp() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let day_seconds = seconds % 86_400;
    let hours = day_seconds / 3_600;
    let minutes = (day_seconds % 3_600) / 60;
    format!("{hours:02}:{minutes:02}")
}
