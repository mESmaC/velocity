//! Continuous work selection and the Duty projection (F8). No UI owns scheduling state.
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::{BTreeMap, HashSet};
use velocity_core::issue_holds::{
    BLOCKED_UPSTREAM_LABEL, TRACKING_LABEL, has_label, is_person_hold_label, parse_person_hold,
    parse_upstream_target,
};
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
    /// A base the integration build has not covered for too long (#503), and landed issues
    /// that have waited too long for a green integration build (#504).
    #[serde(default)]
    pub integration_notice: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct DispatchPreview {
    pub key: String,
    pub label: String,
    pub title: String,
    /// Open issues that wait on this one; the reason it comes first (#510).
    #[serde(default)]
    pub unblocks: u32,
}

pub enum DispatchWork {
    Task(TaskItem),
    /// An issue and how many open issues wait on it (#510).
    Issue(Box<GitHubIssue>, u32),
}

impl DispatchWork {
    fn preview(&self) -> DispatchPreview {
        let (key, label, title, unblocks) = match self {
            Self::Task(task) => (
                format!("task:{}", task.id),
                format!("Task #{}", task.id),
                &task.text,
                0,
            ),
            Self::Issue(issue, unblocks) => (
                format!("issue:{}", issue.number),
                format!("#{}", issue.number),
                &issue.title,
                *unblocks,
            ),
        };
        DispatchPreview {
            key,
            label,
            title: title.clone(),
            unblocks,
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
            integration_notice: None,
        }
    }
}

/// How long a landed issue may wait for a green integration build before the project
/// says so (#504). Integration batches run at least every two hours.
pub const INTEGRATION_WAIT_LIMIT_SECONDS: u64 = 6 * 60 * 60;

/// Open issues whose landed change has waited past the limit, oldest wait first.
pub fn stalled_landed_issues(
    project: &ProjectItem,
    repository: &str,
    issues: &[GitHubIssue],
    now: u64,
) -> Vec<u32> {
    let mut stalled: Vec<_> = issues
        .iter()
        .filter(|issue| issue.state == IssueState::Open)
        .filter_map(|issue| {
            let identity = ThreadIssue::new(repository.into(), issue.number);
            let since = project
                .landed_awaiting_integration(&identity, None)?
                .since?;
            (now.saturating_sub(since) >= INTEGRATION_WAIT_LIMIT_SECONDS)
                .then_some((since, issue.number))
        })
        .collect();
    stalled.sort_unstable();
    stalled.into_iter().map(|(_, number)| number).collect()
}

/// How long the base may hold lands no green integration build covers before the project
/// says so (#503). A batch waits at most two hours for its build, and the build itself
/// may take another: past three hours the build is failing or not running.
pub const UNCOVERED_BASE_LIMIT_SECONDS: u64 = 3 * 60 * 60;
/// The same limit in lands: two full batches of ten.
pub const UNCOVERED_LAND_LIMIT: u32 = 20;

/// "The integration build has not passed on the base for 3 h 20 min (14 lands)", once the
/// base is past either limit. `since` is when the oldest uncovered land landed.
pub fn uncovered_base_notice(lands: u32, since: Option<u64>, now: u64) -> Option<String> {
    let waited = since.map(|since| now.saturating_sub(since));
    let overdue = waited.is_some_and(|waited| waited >= UNCOVERED_BASE_LIMIT_SECONDS);
    if lands == 0 || !(overdue || lands >= UNCOVERED_LAND_LIMIT) {
        return None;
    }
    let count = if lands == 1 {
        "1 land".to_owned()
    } else {
        format!("{lands} lands")
    };
    Some(match waited {
        Some(waited) => {
            let (hours, minutes) = (waited / 3600, waited % 3600 / 60);
            format!(
                "The integration build has not passed on the base for {hours} h {minutes} min ({count})"
            )
        }
        None => format!("The integration build has not passed on the last {count}"),
    })
}

/// What the Duty view says about integration: the uncovered base, the landed issues that
/// wait, and the last build's outcome.
pub fn integration_notice(
    uncovered: Option<&str>,
    stalled: &[u32],
    last_build: Option<&str>,
) -> Option<String> {
    let waiting = stalled_notice(stalled);
    let said: Vec<&str> = uncovered.into_iter().chain(waiting.as_deref()).collect();
    if said.is_empty() {
        return None;
    }
    let reason = last_build.unwrap_or("no integration build has finished");
    Some(format!("{}. Last build: {reason}", said.join(". ")))
}

/// "N landed issues wait for a green integration build (#1, #2)".
fn stalled_notice(stalled: &[u32]) -> Option<String> {
    let (first, rest) = stalled.split_first()?;
    let listed = std::iter::once(first)
        .chain(rest.iter().take(4))
        .map(|number| format!("#{number}"))
        .collect::<Vec<_>>()
        .join(", ");
    let more = if rest.len() > 4 { ", …" } else { "" };
    let count = stalled.len();
    let subject = if count == 1 {
        "1 landed issue waits".to_owned()
    } else {
        format!("{count} landed issues wait")
    };
    Some(format!(
        "{subject} for a green integration build ({listed}{more})"
    ))
}

/// Why an open issue was not dispatched when a slot was free (#509).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipCategory {
    Overlap,
    Label,
    Blocker,
    Decision,
    Claimed,
    LaunchHold,
    Attempted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkippedIssue {
    pub number: u32,
    pub category: SkipCategory,
    pub reason: String,
}

/// Non-hold labels that keep an issue out of automatic dispatch because another dedicated
/// workflow handles them (`spec-question` Questions + Accept, `release-blocker` release gate,
/// `no-change` bounded review + 2-hour auto-return). Machine-checkable holds (`tracking`,
/// `blocked-upstream`, `hold`, plus retired hold labels surfaced on Work) are checked explicitly.
const SKIP_LABELS: &[&str] = &[
    "spec-question",
    "release-blocker",
    IssueClient::NO_CHANGE_LABEL,
];

/// Explains why an open issue was skipped by continuous dispatch (#509).
/// Returns `None` if the issue is closed, already has an active thread, already landed,
/// or is currently dispatchable.
pub fn issue_skip_reason(
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    claimed: &HashSet<String>,
    now: u64,
) -> Option<SkippedIssue> {
    if issue.state != IssueState::Open {
        return None;
    }
    let identity = ThreadIssue::new(repository.into(), issue.number);
    if issue_thread(project, &identity).is_some()
        || project.rosen_issue_completed(&identity)
        || project
            .landed_awaiting_integration(&identity, Some(&issue.updated_at))
            .is_some()
    {
        return None;
    }
    if let Some(wait) = project
        .continuous_dispatch
        .issue_overlaps
        .get(&issue.number)
        .filter(|wait| wait.issue.repository.eq_ignore_ascii_case(repository))
    {
        return Some(SkippedIssue {
            number: issue.number,
            category: SkipCategory::Overlap,
            reason: format!("overlap: {}", wait.reason()),
        });
    }
    if issue.has_open_blockers() {
        let blockers = if issue.open_blockers.is_empty() {
            format!("{} open blocker(s)", issue.open_blocker_count)
        } else {
            format!("blocked by {}", issue.open_blockers.join(", "))
        };
        return Some(SkippedIssue {
            number: issue.number,
            category: SkipCategory::Blocker,
            reason: blockers,
        });
    }
    if has_label(&issue.labels, TRACKING_LABEL) {
        return Some(SkippedIssue {
            number: issue.number,
            category: SkipCategory::Label,
            reason: "tracking issue (closes when checklist completes)".into(),
        });
    }
    if has_label(&issue.labels, BLOCKED_UPSTREAM_LABEL) {
        let detail = parse_upstream_target(&issue.body, Some(repository)).map_or_else(
            || "blocked upstream (missing target)".into(),
            |target| format!("blocked upstream: {}", target.display_summary()),
        );
        return Some(SkippedIssue {
            number: issue.number,
            category: SkipCategory::Blocker,
            reason: detail,
        });
    }
    if let Some(hold) = parse_person_hold(&issue.body, &issue.labels) {
        return Some(SkippedIssue {
            number: issue.number,
            category: SkipCategory::Label,
            reason: format!("hold: {} · {}", hold.reason, hold.release_action),
        });
    }
    if let Some(label) = issue.labels.iter().find(|label| {
        SKIP_LABELS
            .iter()
            .any(|skip| label.eq_ignore_ascii_case(skip))
    }) {
        return Some(SkippedIssue {
            number: issue.number,
            category: SkipCategory::Label,
            reason: format!("label: {label}"),
        });
    }
    if needs_owner_decision(&format!("{}\n{}", issue.title, issue.body)) {
        return Some(SkippedIssue {
            number: issue.number,
            category: SkipCategory::Decision,
            reason: "awaits owner decision".into(),
        });
    }
    if project
        .continuous_dispatch
        .launch_holds(now)
        .any(|held| held.number == issue.number && held.repository.eq_ignore_ascii_case(repository))
    {
        return Some(SkippedIssue {
            number: issue.number,
            category: SkipCategory::LaunchHold,
            reason: "launch failure backoff".into(),
        });
    }
    if project
        .remote_work_hold(&identity, Some(&issue.updated_at), now)
        .is_some()
    {
        return Some(SkippedIssue {
            number: issue.number,
            category: SkipCategory::Claimed,
            reason: "in progress on another machine".into(),
        });
    }
    if claimed.contains(&format!("issue:{}", issue.number)) {
        return Some(SkippedIssue {
            number: issue.number,
            category: SkipCategory::Claimed,
            reason: "claimed in AIRC".into(),
        });
    }
    if project
        .continuous_dispatch
        .reporting_no_change(repository, issue.number)
    {
        return Some(SkippedIssue {
            number: issue.number,
            category: SkipCategory::Attempted,
            reason: "reporting no-change".into(),
        });
    }
    if project
        .continuous_dispatch
        .attempted(repository, issue.number, &issue.updated_at)
    {
        return Some(SkippedIssue {
            number: issue.number,
            category: SkipCategory::Attempted,
            reason: "already attempted at this version".into(),
        });
    }
    None
}

/// Formats skipped issues for the project health strip and `velocity.log` (#509).
pub fn summarize_skipped_issues(skipped: &[SkippedIssue], limit: usize) -> String {
    let shown: Vec<String> = skipped
        .iter()
        .take(limit)
        .map(|item| format!("#{} ({})", item.number, item.reason))
        .collect();
    let remaining = skipped.len().saturating_sub(limit);
    if remaining > 0 {
        format!("{} · +{remaining} more", shown.join(", "))
    } else {
        shown.join(", ")
    }
}

/// Shared automatic-work eligibility for continuous dispatch and the F16 work pool.
/// Owner questions wait for an answer even before somebody labels them (#356, #389).
pub fn actionable(issue: &GitHubIssue) -> bool {
    if issue.state != IssueState::Open
        || issue.has_open_blockers()
        || issue.is_spec_question()
        || has_label(&issue.labels, TRACKING_LABEL)
        || has_label(&issue.labels, BLOCKED_UPSTREAM_LABEL)
        || issue.labels.iter().any(|label| is_person_hold_label(label))
        || issue.labels.iter().any(|label| {
            SKIP_LABELS
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

/// Whether continuous dispatch may start an agent on this issue now.
pub fn dispatchable_issue(
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    claimed: &HashSet<String>,
) -> bool {
    let identity = ThreadIssue::new(repository.into(), issue.number);
    actionable(issue)
        && !claimed.contains(&format!("issue:{}", issue.number))
        && issue_thread(project, &identity).is_none()
        && !project.rosen_issue_completed(&identity)
        // A landed issue waits for its integration build to close it (#504).
        && project
            .landed_awaiting_integration(&identity, Some(&issue.updated_at))
            .is_none()
        && !project
            .continuous_dispatch
            .attempted(repository, issue.number, &issue.updated_at)
        && !project
            .continuous_dispatch
            .reporting_no_change(repository, issue.number)
}

/// Ready issues with the number of open issues each unblocks, critical path first (#510):
/// the issue the most open work waits on, then milestone (none last), then age.
/// `unblocks` comes from `DependencyGraph::unblock_counts`; a missing issue unblocks nothing.
pub fn ordered_issues(
    project: &ProjectItem,
    repository: &str,
    issues: &[GitHubIssue],
    claimed: &HashSet<String>,
    unblocks: &BTreeMap<u32, u32>,
) -> Vec<(GitHubIssue, u32)> {
    let mut ready: Vec<_> = issues
        .iter()
        .filter(|issue| dispatchable_issue(project, repository, issue, claimed))
        .map(|issue| {
            let unblocked = unblocks.get(&issue.number).copied().unwrap_or(0);
            (issue.clone(), unblocked)
        })
        .collect();
    let milestone = |issue: &GitHubIssue| {
        issue
            .milestone
            .as_ref()
            .map_or(u32::MAX, |milestone| milestone.number)
    };
    ready.sort_by(|(left, left_unblocks), (right, right_unblocks)| {
        right_unblocks
            .cmp(left_unblocks)
            .then_with(|| milestone(left).cmp(&milestone(right)))
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
    unblocks: &BTreeMap<u32, u32>,
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
            ordered_issues(project, repository, issues, claimed, unblocks)
                .into_iter()
                .map(|(issue, unblocked)| DispatchWork::Issue(Box::new(issue), unblocked)),
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
