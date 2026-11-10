//! The Swarm dialog's read model (SPEC.md F10): open items with their availability, the
//! model cards, the round-robin split and the PR duty capacity check. The window holds only
//! the user's choices and asks for this again whenever one changes.

use serde::{Deserialize, Serialize};
use specta::Type;
use velocity_core::duty::DutyRole;
use velocity_core::swarm::{self, SwarmSource};
use velocity_github::IssuePanelRow;

use crate::swarms::SwarmItem;
use crate::{ProjectItem, ProviderKind, StateStore, TaskState, ThreadState};

/// Providers with a confirmed ACP agent (§3.2); Antigravity waits on Q14.
pub(crate) const LAUNCHABLE: [ProviderKind; 3] = [
    ProviderKind::Claude,
    ProviderKind::Codex,
    ProviderKind::Kimi,
];

/// What the user has chosen so far. `None` items means the default: every open item.
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct SwarmDialogQuery {
    pub source: SwarmSource,
    pub items: Option<Vec<SwarmItem>>,
    pub providers: Vec<ProviderKind>,
    pub into: Option<u32>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct SwarmDialogItem {
    pub item: SwarmItem,
    pub title: String,
    pub issue: bool,
    /// `task` or `#n`, or why it can't be picked: "has an agent", "lock the spec first".
    pub note: String,
    pub selectable: bool,
    pub selected: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct SwarmModelCard {
    pub provider: ProviderKind,
    pub model_name: String,
    pub selected: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct SwarmDialog {
    pub project_identifier: u32,
    pub source: SwarmSource,
    pub into: Option<u32>,
    pub task_count: u32,
    pub issue_count: u32,
    pub items: Vec<SwarmDialogItem>,
    pub models: Vec<SwarmModelCard>,
    pub split_label: String,
    pub pr_duty_label: Option<String>,
    pub pr_duty_shortfall: u32,
    pub worktree_root: String,
    pub summary: String,
    pub launch_label: String,
    pub issues_note: Option<String>,
}

/// Open issues as the Issues panel last read them, with their owning threads (F9).
pub struct OpenIssues<'a> {
    pub rows: &'a [IssuePanelRow],
    pub loaded: bool,
}

impl StateStore {
    pub fn swarm_dialog(
        &self,
        query: &SwarmDialogQuery,
        issues: OpenIssues<'_>,
    ) -> Option<SwarmDialog> {
        let project = self.active_project()?;
        let tasks = task_items(project);
        let issue_rows = issue_items(issues.rows);
        let open = |items: &[SwarmDialogItem]| items.iter().filter(|item| item.selectable).count();
        let task_count = open(&tasks);
        let issue_count = open(&issue_rows);
        let mut items = match query.source {
            SwarmSource::Tasks => tasks,
            SwarmSource::Issues => issue_rows,
            SwarmSource::Both => tasks.into_iter().chain(issue_rows).collect(),
        };
        for item in &mut items {
            item.selected = item.selectable
                && query
                    .items
                    .as_ref()
                    .is_none_or(|chosen| chosen.contains(&item.item));
        }
        let picked = items.iter().filter(|item| item.selected).count();
        let models = self.model_cards(&query.providers);
        let preferences = self.worktree_context().preferences;
        let root = preferences.project_override.unwrap_or(preferences.root);
        Some(SwarmDialog {
            project_identifier: project.id,
            source: query.source,
            into: query.into,
            task_count: u32::try_from(task_count).unwrap_or(u32::MAX),
            issue_count: u32::try_from(issue_count).unwrap_or(u32::MAX),
            split_label: split_label(picked, &models),
            pr_duty_label: pr_duty(project, picked).0,
            pr_duty_shortfall: pr_duty(project, picked).1,
            summary: summary(picked, &root),
            launch_label: launch_label(picked),
            worktree_root: root,
            items,
            models,
            issues_note: (!issues.loaded && query.source != SwarmSource::Tasks)
                .then(|| "Issues have not loaded yet".into()),
        })
    }

    fn model_cards(&self, chosen: &[ProviderKind]) -> Vec<SwarmModelCard> {
        let settings = &self.state.settings.providers;
        let fallback = [settings.default_provider];
        let chosen = if chosen.is_empty() {
            &fallback[..]
        } else {
            chosen
        };
        LAUNCHABLE
            .into_iter()
            .map(|provider| SwarmModelCard {
                provider,
                model_name: provider_model_name(&self.state.settings, provider),
                selected: chosen.contains(&provider),
            })
            .collect()
    }
}

/// The model a provider's member runs: Settings' default model on the default provider,
/// otherwise that provider's own default (F15).
pub fn provider_model_name(settings: &crate::Settings, provider: ProviderKind) -> String {
    settings
        .providers
        .default_model
        .clone()
        .filter(|name| provider == settings.providers.default_provider && !name.trim().is_empty())
        .unwrap_or_else(|| crate::duty::default_provider_model(provider).into())
}

fn task_items(project: &ProjectItem) -> Vec<SwarmDialogItem> {
    project
        .tasks
        .iter()
        .filter(|task| task.state != TaskState::Done)
        .map(|task| {
            let busy = task.state == TaskState::Now || task.assigned_thread_id.is_some();
            let draft = task.state == TaskState::Draft;
            SwarmDialogItem {
                item: SwarmItem::Task(task.id),
                title: task.text.clone(),
                issue: false,
                note: if busy {
                    "has an agent"
                } else if draft {
                    "lock the spec first"
                } else {
                    "task"
                }
                .into(),
                selectable: !busy && !draft,
                selected: false,
            }
        })
        .collect()
}

fn issue_items(rows: &[IssuePanelRow]) -> Vec<SwarmDialogItem> {
    rows.iter()
        // Questions are answered with Accept, not built (F9).
        .filter(|row| row.question.is_none())
        .map(|row| {
            let busy = row.thread_identifier.is_some();
            SwarmDialogItem {
                item: SwarmItem::Issue(row.number),
                title: row.title.clone(),
                issue: true,
                note: if busy {
                    "has an agent".into()
                } else {
                    format!("#{}", row.number)
                },
                selectable: !busy,
                selected: false,
            }
        })
        .collect()
}

fn split_label(picked: usize, models: &[SwarmModelCard]) -> String {
    let names: Vec<String> = models
        .iter()
        .filter(|card| card.selected)
        .map(|card| card.model_name.clone())
        .collect();
    swarm::split_label(picked, &names)
        .map(|split| format!("Round-robin: {split}"))
        .unwrap_or_else(|| "Pick more than one model to spread the swarm across them.".into())
}

/// The capacity line and how many PR duty agents to add (one reviewer per 3 open PRs).
pub(crate) fn pr_duty(project: &ProjectItem, picked: usize) -> (Option<String>, u32) {
    let reviewers = project
        .duty_roles
        .iter()
        .filter(|duty| duty.role == DutyRole::PrDuty)
        .map(|duty| duty.agents.len())
        .sum();
    let open = project
        .threads
        .iter()
        .filter(|thread| {
            thread.state == ThreadState::Review && thread.pull_request_number.is_some()
        })
        .count();
    let shortfall = swarm::pr_duty_shortfall(picked, open, reviewers);
    (
        swarm::pr_duty_capacity_label(picked, open, reviewers),
        if picked == 0 {
            0
        } else {
            u32::try_from(shortfall).unwrap_or(u32::MAX)
        },
    )
}

fn summary(picked: usize, root: &str) -> String {
    if picked == 0 {
        return "Select at least one item".into();
    }
    let drive: String = root.chars().take(2).collect();
    let place = if drive.ends_with(':') {
        format!(" on {drive}")
    } else {
        String::new()
    };
    format!(
        "{picked} worktree{}{place} · all start now",
        if picked == 1 { "" } else { "s" }
    )
}

fn launch_label(picked: usize) -> String {
    match picked {
        0 => "Launch".into(),
        1 => "Launch 1 agent".into(),
        count => format!("Launch {count} agents"),
    }
}
