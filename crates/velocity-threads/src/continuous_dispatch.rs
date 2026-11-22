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
use velocity_store::issues::{ThreadIssue, issue_thread, issue_thread_label};
use velocity_store::rosen_work::IssueVersion;
use velocity_store::{
    ProjectItem, StateStore, TaskAttribution, TaskItem, ThreadState, VersionControl,
};

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
            let version = IssueVersion::from_github(issue);
            let since = project
                .landed_awaiting_integration_for_version(&identity, version)?
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
    Completed,
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

/// Explains why an open issue was skipped by continuous dispatch (#509, #602).
/// Returns `None` only when the issue is closed, already has an actively running thread,
/// or is currently dispatchable.
pub fn issue_skip_reason(
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    claimed: &HashSet<String>,
    now: u64,
) -> Option<SkippedIssue> {
    issue_skip_reason_in_projects(&[], project, repository, issue, claimed, now)
}

/// Like [`issue_skip_reason`], also treating cross-project blockers whose Rosen land passed
/// integration in another registered project as satisfied (#608).
pub fn issue_skip_reason_in_projects(
    projects: &[ProjectItem],
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
    let version = IssueVersion::from_github(issue);
    if let Some(thread) = issue_thread(project, &identity) {
        if matches!(thread.state, ThreadState::Working | ThreadState::Review) {
            return None;
        }
        return Some(SkippedIssue {
            number: issue.number,
            category: SkipCategory::Claimed,
            reason: format!(
                "kept on thread {} ({})",
                thread.id,
                issue_thread_label(thread)
            ),
        });
    }
    if let Some(hold) = project.reopened_landed_hold(&identity, version) {
        return Some(SkippedIssue {
            number: issue.number,
            category: SkipCategory::Completed,
            reason: velocity_core::reopened_land::reopened_already_landed_reason(&hold.commit),
        });
    }
    if project
        .landed_awaiting_integration_for_version(&identity, version)
        .is_some()
    {
        return Some(SkippedIssue {
            number: issue.number,
            category: SkipCategory::Completed,
            reason: "landed · waiting for integration".into(),
        });
    }
    if project.rosen_issue_completed_for_version(&identity, version) {
        return Some(SkippedIssue {
            number: issue.number,
            category: SkipCategory::Completed,
            reason: "completed by landed Rosen change".into(),
        });
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
    let (named_blockers, unnamed_blockers) =
        unsatisfied_blockers_in_projects(projects, project, repository, issue);
    if unnamed_blockers > 0 || !named_blockers.is_empty() {
        let blockers = if named_blockers.is_empty() {
            format!("{unnamed_blockers} open blocker(s)")
        } else {
            format!("blocked by {}", named_blockers.join(", "))
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

/// Unsatisfied open blockers for `issue` in `project` (#529, #608).
/// A blocker whose Rosen land Velocity has confirmed integration-green in `project` or any
/// registered sibling project in `projects` is satisfied for dispatch even while the issue
/// host still lists it open during the after-land review window.
fn unsatisfied_blockers_in_projects<'a>(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issue: &'a GitHubIssue,
) -> (Vec<&'a str>, u32) {
    let unnamed = issue
        .open_blocker_count
        .saturating_sub(u32::try_from(issue.open_blockers.len()).unwrap_or(u32::MAX));
    let named = issue
        .open_blockers
        .iter()
        .map(String::as_str)
        .filter(|blocker| !blocker_satisfied_in_projects(projects, project, repository, blocker))
        .collect();
    (named, unnamed)
}

fn blocker_satisfied_in_projects(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    blocker: &str,
) -> bool {
    let Some(reference) = velocity_core::issue_dependencies::IssueReference::parse(blocker) else {
        return false;
    };
    let identity = ThreadIssue::new(
        reference.repository_or(repository).to_owned(),
        reference.number(),
    );
    project.rosen_issue_integration_green(&identity)
        || projects.iter().any(|candidate| {
            candidate.id != project.id && candidate.rosen_issue_integration_green(&identity)
        })
}

/// Whether `issue` still waits on any open blocker not yet confirmed integration-green (#529).
pub fn has_unsatisfied_blockers(
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
) -> bool {
    has_unsatisfied_blockers_in_projects(&[], project, repository, issue)
}

/// Like [`has_unsatisfied_blockers`], also checking sibling projects in `projects` (#608).
pub fn has_unsatisfied_blockers_in_projects(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
) -> bool {
    let (named, unnamed) = unsatisfied_blockers_in_projects(projects, project, repository, issue);
    unnamed > 0 || !named.is_empty()
}

fn actionable_without_blockers(issue: &GitHubIssue) -> bool {
    if issue.state != IssueState::Open
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

/// Shared automatic-work eligibility for continuous dispatch and the F16 work pool.
/// Owner questions wait for an answer even before somebody labels them (#356, #389).
pub fn actionable(issue: &GitHubIssue) -> bool {
    !issue.has_open_blockers() && actionable_without_blockers(issue)
}

/// Automatic-work eligibility in `project`, treating blockers whose Rosen land is
/// confirmed integration-green as satisfied even while still open for review (#529).
pub fn actionable_in_project(project: &ProjectItem, repository: &str, issue: &GitHubIssue) -> bool {
    actionable_in_projects(&[], project, repository, issue)
}

/// Like [`actionable_in_project`], also checking sibling projects in `projects` for
/// integration-green cross-project blockers (#608).
pub fn actionable_in_projects(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
) -> bool {
    !has_unsatisfied_blockers_in_projects(projects, project, repository, issue)
        && actionable_without_blockers(issue)
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
    dispatchable_issue_in_projects(&[], project, repository, issue, claimed)
}

/// Like [`dispatchable_issue`], also checking sibling projects in `projects` for
/// integration-green cross-project blockers (#608).
pub fn dispatchable_issue_in_projects(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    claimed: &HashSet<String>,
) -> bool {
    let identity = ThreadIssue::new(repository.into(), issue.number);
    let version = IssueVersion::from_github(issue);
    actionable_in_projects(projects, project, repository, issue)
        && !claimed.contains(&format!("issue:{}", issue.number))
        && issue_thread(project, &identity).is_none()
        && !project.rosen_issue_completed_for_version(&identity, version)
        && project.reopened_landed_hold(&identity, version).is_none()
        // A landed issue waits for its integration build to close it (#504).
        && project
            .landed_awaiting_integration_for_version(&identity, version)
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
    ordered_issues_in_projects(&[], project, repository, issues, claimed, unblocks)
}

/// Like [`ordered_issues`], also checking sibling projects in `projects` (#608).
pub fn ordered_issues_in_projects(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issues: &[GitHubIssue],
    claimed: &HashSet<String>,
    unblocks: &BTreeMap<u32, u32>,
) -> Vec<(GitHubIssue, u32)> {
    let mut ready: Vec<_> = issues
        .iter()
        .filter(|issue| {
            dispatchable_issue_in_projects(projects, project, repository, issue, claimed)
        })
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
    ordered_work_in_projects(
        &[],
        project,
        repository,
        issues,
        claimed,
        issues_known,
        unblocks,
    )
}

/// Like [`ordered_work`], also checking sibling projects in `projects` (#608).
pub fn ordered_work_in_projects(
    projects: &[ProjectItem],
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
            ordered_issues_in_projects(projects, project, repository, issues, claimed, unblocks)
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

/// Snapshot of local AIRC members currently alive on this machine for one project (#566).
#[derive(Clone, Debug, Default)]
pub struct LiveLocalMembers {
    /// Duty scout or review-swarm worker nicknames with a running or claiming session.
    pub running_scouts: HashSet<String>,
    /// Build thread IDs with a live ACP session in `ThreadRuntimes`.
    pub live_threads: HashSet<u32>,
    /// Spec interview thread IDs currently running.
    pub running_interviews: HashSet<u32>,
}

/// Result of splitting a project's AIRC claims into live holders vs dead local holders (#566).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FilteredClaims {
    /// Active piece keys (remote claims and live local claims) that hold work out of dispatch.
    pub active: HashSet<String>,
    /// Claims held by known-dead local members that dispatch ignores and releases.
    pub dead_local: Vec<velocity_airc::Claim>,
}

/// Reports a stale local AIRC claim released for project health and diagnostics (#566).
pub fn stale_claim_reason(nick: &str) -> String {
    format!("stale AIRC claim by {nick}, released")
}

/// Extracts the GitHub issue number from an `issue:<n>` piece key.
pub fn issue_number_from_piece(piece: &str) -> Option<u32> {
    piece
        .strip_prefix("issue:")
        .and_then(|value| value.parse::<u32>().ok())
}

/// Whether a local AIRC member identity belongs to a live thread, interview or scout on
/// this machine (#566). Foreign machine-scoped members and system names return `true`.
pub fn is_live_local_member(project: &ProjectItem, member: &str, live: &LiveLocalMembers) -> bool {
    if member.contains('/')
        || member.starts_with("velocity-machine-")
        || matches!(member, "you" | "velocity")
    {
        return true;
    }
    if let Some(nickname) = member.strip_prefix("duty-") {
        return live.running_scouts.contains(nickname);
    }
    if let Some(id) = member
        .strip_prefix("spec-")
        .and_then(|value| value.parse::<u32>().ok())
    {
        return live.running_interviews.contains(&id)
            || project
                .threads
                .iter()
                .any(|thread| thread.id == id && thread.state == ThreadState::Spec);
    }
    let thread = project
        .threads
        .iter()
        .find(|thread| thread.matches_airc_member(member))
        .or_else(|| {
            velocity_core::thread_titles::thread_id_from_member(member)
                .and_then(|id| project.threads.iter().find(|thread| thread.id == id))
        });
    match thread {
        Some(thread) => {
            !thread.supervision.released
                && !matches!(thread.state, ThreadState::Merged | ThreadState::Done)
        }
        None => velocity_core::thread_titles::thread_id_from_member(member)
            .is_some_and(|id| live.live_threads.contains(&id)),
    }
}

/// Splits `claims` into active claims that hold work out of dispatch and dead local claims
/// that dispatch must ignore and release (#566). Claims owned by other machines stay active.
pub fn filter_live_claims(
    project: &ProjectItem,
    claims: &[velocity_airc::ScopedClaim],
    live: &LiveLocalMembers,
) -> FilteredClaims {
    let mut result = FilteredClaims::default();
    for scoped in claims {
        if !scoped.local || is_live_local_member(project, &scoped.claim.member, live) {
            result.active.insert(scoped.claim.key().to_owned());
        } else {
            result.dead_local.push(scoped.claim.clone());
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use velocity_airc::{Claim, ScopedClaim};
    use velocity_store::ThreadItem;

    fn sample_claim(member: &str, nick: &str, piece: &str, local: bool) -> ScopedClaim {
        ScopedClaim {
            claim: Claim {
                member: member.into(),
                nick: nick.into(),
                piece: piece.into(),
                task: format!("Work on {piece}"),
                branch: String::new(),
                change: None,
                files: Vec::new(),
                since: "2026-11-19T23:12:00.000Z".into(),
            },
            local,
        }
    }

    fn sample_issue(number: u32) -> GitHubIssue {
        GitHubIssue {
            id: format!("I_{number}"),
            number,
            title: format!("Issue #{number}"),
            body: String::new(),
            state: IssueState::Open,
            author: None,
            labels: Vec::new(),
            scout: None,
            milestone: None,
            url: format!("https://github.com/owner/repo/issues/{number}"),
            created_at: "2026-11-19T23:12:00Z".into(),
            updated_at: "2026-11-19T23:12:00Z".into(),
            closed_at: None,
            reopened_at: None,
            reopened_by_import: false,
            reopened_by_non_person: false,
            open_blocker_count: 0,
            open_blockers: Vec::new(),
            closed_blockers: Vec::new(),
            host: velocity_github::IssueHost::GitHub,
        }
    }

    // Risk (#566): a dead local scout or ended/deleted thread holding an AIRC piece must
    // not keep an issue out of dispatch, while live local and remote claims remain active.
    #[test]
    fn dead_local_claims_are_ignored_for_dispatch_while_live_and_remote_hold() {
        let mut project = ProjectItem::new(1, "collapse".into(), "Z:/repo".into());
        let mut merged = ThreadItem::new(10, "Finished".into());
        merged.state = ThreadState::Merged;
        let mut working = ThreadItem::new(20, "Active".into());
        working.state = ThreadState::Working;
        project.threads.push(merged);
        project.threads.push(working);

        let mut live = LiveLocalMembers::default();
        live.running_scouts.insert("scout-2".into());

        let claims = vec![
            sample_claim("duty-scout-1", "scout-1-1", "issue:152", true),
            sample_claim("thread-10", "finished-10", "issue:153", true),
            sample_claim("thread-99", "deleted-99", "issue:154", true),
            sample_claim("thread-20", "active-20", "issue:155", true),
            sample_claim("duty-scout-2", "scout-2", "issue:156", true),
            sample_claim("abcdef0123456789/thread-5", "remote-5", "issue:157", false),
        ];
        let filtered = filter_live_claims(&project, &claims, &live);

        assert_eq!(filtered.dead_local.len(), 3);
        assert!(dispatchable_issue(
            &project,
            "owner/repo",
            &sample_issue(152),
            &filtered.active
        ));
        assert!(dispatchable_issue(
            &project,
            "owner/repo",
            &sample_issue(153),
            &filtered.active
        ));
        assert!(dispatchable_issue(
            &project,
            "owner/repo",
            &sample_issue(154),
            &filtered.active
        ));
        assert!(!dispatchable_issue(
            &project,
            "owner/repo",
            &sample_issue(155),
            &filtered.active
        ));
        assert!(!dispatchable_issue(
            &project,
            "owner/repo",
            &sample_issue(156),
            &filtered.active
        ));
        assert!(!dispatchable_issue(
            &project,
            "owner/repo",
            &sample_issue(157),
            &filtered.active
        ));
        assert_eq!(
            stale_claim_reason("scout-1-1"),
            "stale AIRC claim by scout-1-1, released"
        );
    }

    // Risk (#602): a persisted completed Rosen work item must not hide a reopened open
    // issue from dispatch, and issue_skip_reason must report every open issue dispatch drops.
    #[test]
    fn reopened_issue_dispatches_across_save_and_dropped_issues_report_reasons()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = std::env::temp_dir().join(format!(
            "velocity-dispatch-602-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let mut project = ProjectItem::new(1, "collapse".into(), "Z:/repo".into());
        project.version_control = VersionControl::Rosen;
        project.spec_locked = true;
        let nov_18 = velocity_store::rosen_work::parse_timestamp_seconds("2026-11-18T12:00:00Z")
            .ok_or("valid timestamp")?
            .to_string();
        let nov_21_land =
            velocity_store::rosen_work::parse_timestamp_seconds("2026-11-21T09:00:00Z")
                .ok_or("valid timestamp")?
                .to_string();
        project
            .rosen_work_items
            .push(velocity_core::work_pool::WorkItem {
                key: velocity_core::work_pool::WorkKey::issue("owner/repo", 39),
                title: "Build web side".into(),
                thread_identifier: None,
                change: Some("chg-39".into()),
                status: velocity_core::work_pool::WorkStatus::Green,
                claimed_at: Some(nov_18.clone()),
                updated_at: Some(nov_18.clone()),
                landed_at: Some(nov_18.clone()),
                green_at: Some(nov_18),
                reverted_at: None,
                landed_commit: Some("4a63d3f".into()),
                revert_reason: None,
            });
        project
            .rosen_work_items
            .push(velocity_core::work_pool::WorkItem {
                key: velocity_core::work_pool::WorkKey::issue("owner/repo", 40),
                title: "Waiting for integration".into(),
                thread_identifier: None,
                change: Some("chg-40".into()),
                status: velocity_core::work_pool::WorkStatus::Landed,
                claimed_at: Some(nov_21_land.clone()),
                updated_at: Some(nov_21_land.clone()),
                landed_at: Some(nov_21_land),
                green_at: None,
                reverted_at: None,
                landed_commit: Some("deadbeef".into()),
                revert_reason: None,
            });
        let mut kept_error = ThreadItem::new(50, "Failed issue".into());
        kept_error.state = ThreadState::Error;
        kept_error.issue = Some(ThreadIssue::new("owner/repo".into(), 41));
        project.threads.push(kept_error);
        let mut active = ThreadItem::new(51, "Active issue".into());
        active.state = ThreadState::Working;
        active.issue = Some(ThreadIssue::new("owner/repo".into(), 42));
        project.threads.push(active);

        std::fs::create_dir_all(&directory)?;
        let mut persisted = velocity_store::PersistedState::default();
        persisted.projects.push(project);
        std::fs::write(
            directory.join(velocity_store::STATE_FILE_NAME),
            serde_json::to_vec(&persisted)?,
        )?;
        let loaded = StateStore::load_or_default(&directory)?;
        let project = loaded.project(1).ok_or("reloaded project missing")?;
        let claimed = HashSet::new();

        // Un-reopened green issue is skipped with an explicit reason instead of disappearing.
        let mut still_green = sample_issue(39);
        still_green.title = "Build web side".into();
        still_green.updated_at = "2026-11-18T11:30:00Z".into();
        assert!(!dispatchable_issue(
            project,
            "owner/repo",
            &still_green,
            &claimed
        ));
        let skip_green = issue_skip_reason(project, "owner/repo", &still_green, &claimed, 0);
        assert_eq!(
            skip_green.as_ref().map(|item| item.category),
            Some(SkipCategory::Completed)
        );

        // Reopened issue (newer updated_at / reopen event) is dispatchable again.
        let mut reopened = sample_issue(39);
        reopened.title = "Build web side".into();
        reopened.updated_at = "2026-11-21T10:00:00Z".into();
        assert!(dispatchable_issue(
            project,
            "owner/repo",
            &reopened,
            &claimed
        ));
        assert!(issue_skip_reason(project, "owner/repo", &reopened, &claimed, 0).is_none());

        // In-flight land stays held after Velocity's land comment bumps updated_at, and reports a reason.
        let mut awaiting = sample_issue(40);
        awaiting.title = "Waiting for integration".into();
        awaiting.updated_at = "2026-11-21T09:00:05Z".into();
        assert!(!dispatchable_issue(
            project,
            "owner/repo",
            &awaiting,
            &claimed
        ));
        let skip_landed = issue_skip_reason(project, "owner/repo", &awaiting, &claimed, 0);
        assert_eq!(
            skip_landed.as_ref().map(|item| item.reason.as_str()),
            Some("landed · waiting for integration")
        );

        // Reopening an issue with a Landed work item makes it dispatchable again.
        let mut reopened_landed = awaiting;
        reopened_landed.updated_at = "2026-11-21T10:00:00Z".into();
        reopened_landed.reopened_at = Some("2026-11-21T10:00:00Z".into());
        assert!(dispatchable_issue(
            project,
            "owner/repo",
            &reopened_landed,
            &claimed
        ));
        assert!(issue_skip_reason(project, "owner/repo", &reopened_landed, &claimed, 0).is_none());

        // Kept failed thread is reported; actively working thread is not flagged as skipped.
        let kept_skip = issue_skip_reason(project, "owner/repo", &sample_issue(41), &claimed, 0);
        assert_eq!(
            kept_skip.as_ref().map(|item| item.category),
            Some(SkipCategory::Claimed)
        );
        assert!(issue_skip_reason(project, "owner/repo", &sample_issue(42), &claimed, 0).is_none());

        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    // Risk (#608): an issue blocked by a sibling project's issue must show "blocked by <repo>#N"
    // and return to dispatch when that sibling issue passes integration (or closes) across save/load.
    #[test]
    fn cross_project_blocker_waits_and_returns_when_sibling_passes_integration()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory =
            std::env::temp_dir().join(format!("velocity-dispatch-608-{}", uuid::Uuid::new_v4()));
        let mut home = ProjectItem::new(1, "velocity".into(), "Z:/velocity".into());
        home.version_control = VersionControl::Rosen;
        home.spec_locked = true;
        home.github_remote = Some("mESmaC/velocity".into());

        let mut sibling = ProjectItem::new(2, "collapse.sh".into(), "Z:/collapse".into());
        sibling.version_control = VersionControl::Rosen;
        sibling.spec_locked = true;
        sibling.github_remote = Some("mESmaC/collapse.sh".into());
        sibling
            .rosen_work_items
            .push(velocity_core::work_pool::WorkItem {
                key: velocity_core::work_pool::WorkKey::issue("mESmaC/collapse.sh", 216),
                title: "Desktop session release token".into(),
                thread_identifier: None,
                change: Some("chg-216".into()),
                status: velocity_core::work_pool::WorkStatus::Landed,
                claimed_at: None,
                updated_at: None,
                landed_at: None,
                green_at: None,
                reverted_at: None,
                landed_commit: Some("abc1234".into()),
                revert_reason: None,
            });

        std::fs::create_dir_all(&directory)?;
        let mut persisted = velocity_store::PersistedState::default();
        persisted.projects.push(home);
        persisted.projects.push(sibling);
        std::fs::write(
            directory.join(velocity_store::STATE_FILE_NAME),
            serde_json::to_vec(&persisted)?,
        )?;
        let loaded = StateStore::load_or_default(&directory)?;
        let projects = &loaded.state().projects;
        let home_project = loaded.project(1).ok_or("home project missing")?;
        let claimed = HashSet::new();

        let mut blocked = sample_issue(607);
        blocked.open_blocker_count = 1;
        blocked.open_blockers = vec!["mESmaC/collapse.sh#216".into()];

        // While the sibling issue has not passed integration, home issue waits with blocked_by reason.
        assert!(!dispatchable_issue_in_projects(
            projects,
            home_project,
            "mESmaC/velocity",
            &blocked,
            &claimed,
        ));
        let skipped = issue_skip_reason_in_projects(
            projects,
            home_project,
            "mESmaC/velocity",
            &blocked,
            &claimed,
            0,
        )
        .ok_or("expected blocked issue to be skipped")?;
        assert_eq!(skipped.category, SkipCategory::Blocker);
        assert_eq!(skipped.reason, "blocked by mESmaC/collapse.sh#216");

        // Once the sibling issue reaches Green in its own project, home issue returns to dispatch.
        let mut green_projects = projects.clone();
        green_projects[1].rosen_work_items[0].status = velocity_core::work_pool::WorkStatus::Green;
        assert!(dispatchable_issue_in_projects(
            &green_projects,
            &green_projects[0],
            "mESmaC/velocity",
            &blocked,
            &claimed,
        ));
        assert!(
            issue_skip_reason_in_projects(
                &green_projects,
                &green_projects[0],
                "mESmaC/velocity",
                &blocked,
                &claimed,
                0,
            )
            .is_none()
        );

        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    // Risk (#615): a reopened issue whose fix is already in land history must be held out of
    // dispatch with "reopened, already landed in <commit>", while a genuine person reopen
    // strictly after the land commit remains dispatchable (#602).
    #[test]
    fn reopened_already_landed_hold_blocks_dispatch_until_person_reopens_after_land()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = ProjectItem::new(1, "velocity".into(), "Z:/velocity".into());
        project.version_control = VersionControl::Rosen;
        project.spec_locked = true;
        let land_time = velocity_store::rosen_work::parse_timestamp_seconds("2026-11-22T10:00:00Z")
            .ok_or("timestamp")?;
        project.continuous_dispatch.reopened_landed_holds.insert(
            609,
            velocity_store::continuous_dispatch::ReopenedLandedHold {
                issue: ThreadIssue::new("mESmaC/velocity".into(), 609),
                commit: "9b7f817abcdef1234567890abcdef12345678901".into(),
                landed_at: land_time,
                land_summary: "Fix issue (#609)".into(),
                updated_at: "2026-11-22T09:00:00Z".into(),
                reopened_by_import: true,
                detected_at_ms: 1_700_000_000_000,
            },
        );
        let claimed = HashSet::new();

        let mut import_reopened = sample_issue(609);
        import_reopened.updated_at = "2026-11-22T09:00:00Z".into();
        import_reopened.reopened_by_import = true;
        assert!(!dispatchable_issue(
            &project,
            "mESmaC/velocity",
            &import_reopened,
            &claimed
        ));
        let skipped = issue_skip_reason(&project, "mESmaC/velocity", &import_reopened, &claimed, 0)
            .ok_or("expected reopened-landed issue to be skipped")?;
        assert_eq!(skipped.category, SkipCategory::Completed);
        assert_eq!(skipped.reason, "reopened, already landed in 9b7f817abcde");

        // When a person genuinely reopens the regression after the landing commit, dispatch allows it.
        let mut person_reopened = sample_issue(609);
        person_reopened.updated_at = "2026-11-22T13:00:00Z".into();
        person_reopened.reopened_at = Some("2026-11-22T13:00:00Z".into());
        assert!(dispatchable_issue(
            &project,
            "mESmaC/velocity",
            &person_reopened,
            &claimed
        ));
        assert!(
            issue_skip_reason(&project, "mESmaC/velocity", &person_reopened, &claimed, 0).is_none()
        );
        Ok(())
    }
}
