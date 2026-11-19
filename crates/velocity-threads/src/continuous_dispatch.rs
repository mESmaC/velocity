//! Continuous work selection and the Duty projection (F8). No UI owns scheduling state.
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashSet;
use velocity_github::{GitHubIssue, IssueClient, IssueState};
use velocity_store::continuous_dispatch::{available_task, busy_count};
use velocity_store::issues::{ThreadIssue, issue_thread};
use velocity_store::{ProjectItem, StateStore, TaskAttribution, TaskItem, VersionControl};

use crate::{LaunchPlan, dispatch::DispatchError};

#[derive(Clone, Debug, Default, Serialize, Deserialize, Type)]
pub struct DispatchView {
    pub project_identifier: Option<u32>,
    pub available: bool,
    pub slots: u32,
    pub busy: u32,
    pub paused: bool,
    pub next: Vec<DispatchPreview>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct DispatchPreview {
    pub key: String,
    pub label: String,
    pub title: String,
}

pub enum DispatchWork {
    Task(TaskItem),
    Issue(Box<GitHubIssue>),
}

impl DispatchWork {
    fn preview(&self) -> DispatchPreview {
        let (key, label, title) = match self {
            Self::Task(task) => (
                format!("task:{}", task.id),
                format!("Task #{}", task.id),
                &task.text,
            ),
            Self::Issue(issue) => (
                format!("issue:{}", issue.number),
                format!("#{}", issue.number),
                &issue.title,
            ),
        };
        DispatchPreview {
            key,
            label,
            title: title.clone(),
        }
    }
}

impl DispatchView {
    pub fn new(project: &ProjectItem, work: &[DispatchWork], error: Option<String>) -> Self {
        Self {
            project_identifier: Some(project.id),
            available: project.version_control == VersionControl::Rosen,
            slots: project.continuous_dispatch.slots,
            busy: busy_count(project),
            paused: project.continuous_dispatch.paused,
            next: work.iter().take(5).map(DispatchWork::preview).collect(),
            error,
        }
    }
}

/// Shared automatic-work eligibility for continuous dispatch and the F16 work pool.
/// Owner questions wait for an answer even before somebody labels them (#356, #389).
pub fn actionable(issue: &GitHubIssue) -> bool {
    if issue.state != IssueState::Open
        || issue.has_open_blockers()
        || issue.is_spec_question()
        || issue.labels.iter().any(|label| {
            [
                "needs-decision",
                "needs-owner",
                "release-blocker",
                "owner-decision",
                IssueClient::NO_CHANGE_LABEL,
            ]
            .iter()
            .any(|skip| label.eq_ignore_ascii_case(skip))
        })
    {
        return false;
    }
    !needs_owner_decision(&format!("{}\n{}", issue.title, issue.body))
}

fn needs_owner_decision(text: &str) -> bool {
    let text = text.to_lowercase();
    [
        "awaiting an owner decision",
        "awaits an owner decision",
        "awaits owner decision",
        "waiting for an owner decision",
        "waiting on an owner decision",
        "needs an owner decision",
        "awaiting owner decision",
        "waiting for the owner to decide",
        "only asks for an owner decision",
        "decision needed",
        "question for the spec owner",
        "suggested: decide whether",
    ]
    .iter()
    .any(|phrase| text.contains(phrase))
}

pub fn ordered_issues(
    project: &ProjectItem,
    repository: &str,
    issues: &[GitHubIssue],
    claimed: &HashSet<String>,
) -> Vec<GitHubIssue> {
    let mut ready: Vec<_> = issues
        .iter()
        .filter(|issue| {
            let identity = ThreadIssue::new(repository.into(), issue.number);
            actionable(issue)
                && !claimed.contains(&format!("issue:{}", issue.number))
                && issue_thread(project, &identity).is_none()
                && !project.rosen_issue_completed(&identity)
                && !project.continuous_dispatch.attempted(
                    repository,
                    issue.number,
                    &issue.updated_at,
                )
                && !project
                    .continuous_dispatch
                    .reporting_no_change(repository, issue.number)
        })
        .cloned()
        .collect();
    ready.sort_by(|left, right| {
        left.milestone
            .as_ref()
            .map(|milestone| milestone.number)
            .unwrap_or(u32::MAX)
            .cmp(
                &right
                    .milestone
                    .as_ref()
                    .map(|milestone| milestone.number)
                    .unwrap_or(u32::MAX),
            )
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.number.cmp(&right.number))
    });
    ready
}

pub fn dispatchable_task(
    project: &ProjectItem,
    task: &TaskItem,
    claimed: &HashSet<String>,
) -> bool {
    available_task(project, task)
        && !claimed.contains(&format!("task:{}", task.id))
        && !needs_owner_decision(&task.text)
        && !task.text.to_lowercase().contains("release-blocker")
}

pub fn ordered_work(
    project: &ProjectItem,
    repository: &str,
    issues: &[GitHubIssue],
    claimed: &HashSet<String>,
    issues_known: bool,
) -> Vec<DispatchWork> {
    if !project.spec_locked || project.version_control != VersionControl::Rosen {
        return Vec::new();
    }
    let (mut user, mut other): (Vec<_>, Vec<_>) = project
        .tasks
        .iter()
        .filter(|task| dispatchable_task(project, task, claimed))
        .partition(|task| task.added_by == TaskAttribution::User);
    user.sort_by_key(|task| task.id);
    other.sort_by_key(|task| task.id);
    let mut work: Vec<_> = user.into_iter().cloned().map(DispatchWork::Task).collect();
    if issues_known {
        work.extend(
            ordered_issues(project, repository, issues, claimed)
                .into_iter()
                .map(|issue| DispatchWork::Issue(Box::new(issue))),
        );
        work.extend(other.into_iter().cloned().map(DispatchWork::Task));
    }
    work
}

pub fn plan_task(
    store: &StateStore,
    project: &ProjectItem,
    task: &TaskItem,
) -> Result<LaunchPlan, DispatchError> {
    let prompt = format!(
        "Continuous dispatch assigned you Task List item #{}:\n\n{}\n\nImplement it end to end. {}\n\nFollow the Rosen workspace note Velocity gave you. Name the change with rosen commit -m \"<one-line summary> (task #{})\" and end your turn. Velocity marks your task done when the change lands. If there is no work to change, explain why and end your turn.",
        task.id,
        crate::swarm::task_excerpt(task.id, &task.text),
        crate::task_spec::instructions(&[&task.text]),
        task.id,
    );
    let mut plan = crate::launch::plan_named_launch(
        project,
        &store.worktree_preferences(project),
        &store.settings(),
        &prompt,
        &task.text,
        &format!("task-{}", task.id),
    )?;
    plan.title.request = None;
    Ok(plan)
}
