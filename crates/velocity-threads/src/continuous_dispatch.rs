//! Continuous work selection and the Duty projection (F8). No UI owns scheduling state.
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::{BTreeMap, HashSet};
use velocity_core::collapse::{IssueHost, allowed_downstream_setup_page, issue_page};
use velocity_core::issue_dependencies::{
    IssueReference, explicit_text_blockers, is_owner_comment, referenced_blockers_in_text,
};
use velocity_core::issue_holds::{
    BLOCKED_UPSTREAM_LABEL, HOLD_LABEL, TRACKING_LABEL, has_label, is_person_hold_label,
    parse_checklist, parse_explicit_person_hold, parse_person_hold, parse_upstream_target,
};
use velocity_core::project_health::{BlockedDispatchIssue, BlockedIssueKind};
use velocity_github::{GitHubIssue, IssueClient, IssueState};
use velocity_store::continuous_dispatch::{available_task, busy_count, thread_attempt_summary};
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

/// Why an open issue was not dispatched when a slot was free (#509, #617).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipCategory {
    Overlap,
    Label,
    Blocker,
    NeedsOwner,
    BlockedUpstream,
    Tracking,
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

/// Partitioned view of skipped issues for project health so `needs-owner`, `blocked-upstream`,
/// and `tracking` issues are listed separately from one another and from regular blockers (#617).
#[derive(Clone, Debug, Default)]
pub struct SkippedIssueGroups<'a> {
    pub other: Vec<&'a SkippedIssue>,
    pub needs_owner: Vec<&'a SkippedIssue>,
    pub blocked_upstream: Vec<&'a SkippedIssue>,
    pub tracking: Vec<&'a SkippedIssue>,
}

/// Splits skipped issues into `other`, `needs_owner`, `blocked_upstream`, and `tracking` (#617).
pub fn partition_skipped_issues(skipped: &[SkippedIssue]) -> SkippedIssueGroups<'_> {
    let mut groups = SkippedIssueGroups::default();
    for item in skipped {
        match item.category {
            SkipCategory::NeedsOwner => groups.needs_owner.push(item),
            SkipCategory::BlockedUpstream => groups.blocked_upstream.push(item),
            SkipCategory::Tracking => groups.tracking.push(item),
            _ => groups.other.push(item),
        }
    }
    groups
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

/// Cross-project open-issue snapshot and cached comments used to resolve explicit text
/// blockers (`Waits for`, `Blocked by`, `After`) across same-host and cross-host projects (#623).
#[derive(Clone, Debug, Default)]
pub struct TextBlockerContext {
    /// Known open issue numbers per lowercased `owner/name` repository.
    /// Presence of a repository key means its open issues have been loaded; an issue number
    /// absent from that set is closed. When non-empty, a registered project missing from this
    /// map fails closed until its backlog is loaded.
    pub open_issues_by_repo: BTreeMap<String, HashSet<u32>>,
    /// Cached comment bodies per issue number in the current project's repository.
    pub comments_by_issue: BTreeMap<u32, Vec<String>>,
}

impl TextBlockerContext {
    /// Builds a context containing only `repository`'s current open issues.
    pub fn for_backlog(repository: &str, issues: &[GitHubIssue]) -> Self {
        let mut context = Self::default();
        if !repository.is_empty() {
            context.insert_open_issues(repository, issues);
        }
        context
    }

    pub fn insert_open_issues(&mut self, repository: &str, issues: &[GitHubIssue]) {
        if repository.is_empty() {
            return;
        }
        let open = issues
            .iter()
            .filter(|issue| issue.state == IssueState::Open)
            .map(|issue| issue.number)
            .collect();
        self.open_issues_by_repo
            .insert(repository.to_ascii_lowercase(), open);
    }

    pub fn insert_open_numbers(
        &mut self,
        repository: &str,
        numbers: impl IntoIterator<Item = u32>,
    ) {
        if repository.is_empty() {
            return;
        }
        self.open_issues_by_repo.insert(
            repository.to_ascii_lowercase(),
            numbers.into_iter().collect(),
        );
    }

    pub fn insert_comments(&mut self, issue_number: u32, comments: Vec<String>) {
        self.comments_by_issue.insert(issue_number, comments);
    }

    pub fn open_numbers_for(&self, repository: &str) -> Option<&HashSet<u32>> {
        self.open_issues_by_repo
            .get(&repository.to_ascii_lowercase())
    }

    pub fn comments_for(&self, issue_number: u32) -> &[String] {
        self.comments_by_issue
            .get(&issue_number)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }
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
    issue_skip_reason_with_blockers(
        projects,
        project,
        repository,
        issue,
        claimed,
        now,
        &TextBlockerContext::default(),
    )
}

/// Like [`issue_skip_reason_in_projects`], also resolving explicit text blockers (`Waits for`,
/// `Blocked by`, `After`) in the issue body and owner comments across all registered projects
/// regardless of each project's issue host (#623).
pub fn issue_skip_reason_with_blockers(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    claimed: &HashSet<String>,
    now: u64,
    context: &TextBlockerContext,
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
        unsatisfied_blockers_with_context(projects, project, repository, issue, context);
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
        let progress = parse_checklist(&issue.body);
        let reason = if progress.total() > 0 {
            format!("tracking {}/{}", progress.checked, progress.total())
        } else {
            "tracking".into()
        };
        return Some(SkippedIssue {
            number: issue.number,
            category: SkipCategory::Tracking,
            reason,
        });
    }
    if has_label(&issue.labels, BLOCKED_UPSTREAM_LABEL) {
        let detail = parse_upstream_target(&issue.body, Some(repository)).map_or_else(
            || "missing upstream target".into(),
            |target| format!("{} · {}", target.display_summary(), target.url()),
        );
        return Some(SkippedIssue {
            number: issue.number,
            category: SkipCategory::BlockedUpstream,
            reason: detail,
        });
    }
    if let Some(hold) = parse_person_hold(&issue.body, &issue.labels) {
        return Some(SkippedIssue {
            number: issue.number,
            category: SkipCategory::NeedsOwner,
            reason: format!("{} · {}", hold.reason, hold.release_action),
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

/// Builds a structured blocked-issue entry with one-click owner unblock metadata for the
/// dispatch-idle alert (#616). Returns `None` for `Tracking` issues, which are never shown
/// as blocked (#617).
pub fn blocked_dispatch_issue(
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    skipped: &SkippedIssue,
) -> Option<BlockedDispatchIssue> {
    if skipped.category == SkipCategory::Tracking {
        return None;
    }
    let issue_url = validated_issue_url(repository, issue);
    if has_label(&issue.labels, HOLD_LABEL) {
        return Some(blocked_hold_issue(repository, issue, skipped, issue_url));
    }
    if skipped.category == SkipCategory::NeedsOwner {
        return Some(blocked_needs_owner_issue(
            repository, issue, skipped, issue_url,
        ));
    }
    if skipped.category == SkipCategory::Attempted
        && skipped.reason == "already attempted at this version"
    {
        let (last_attempt_result, last_attempt_thread) =
            last_attempt_outcome(project, repository, issue.number);
        return Some(BlockedDispatchIssue {
            number: issue.number,
            repository: repository.to_owned(),
            title: issue.title.clone(),
            kind: BlockedIssueKind::Attempted,
            reason: skipped.reason.clone(),
            issue_url,
            last_attempt_result: Some(last_attempt_result),
            last_attempt_thread: last_attempt_thread.map(|id| id.to_string()),
            can_lift_hold: false,
            can_retry_attempt: true,
            can_mark_needs_owner: true,
        });
    }
    let kind = if has_label(&issue.labels, "spec-question") {
        BlockedIssueKind::NeedsOwner
    } else {
        BlockedIssueKind::Other
    };
    Some(BlockedDispatchIssue {
        number: issue.number,
        repository: repository.to_owned(),
        title: issue.title.clone(),
        kind,
        reason: skipped.reason.clone(),
        issue_url,
        last_attempt_result: None,
        last_attempt_thread: None,
        can_lift_hold: false,
        can_retry_attempt: false,
        can_mark_needs_owner: false,
    })
}

fn blocked_hold_issue(
    repository: &str,
    issue: &GitHubIssue,
    skipped: &SkippedIssue,
    issue_url: Option<String>,
) -> BlockedDispatchIssue {
    let reason = match parse_person_hold(&issue.body, &issue.labels) {
        Some(hold) if !hold.reason.trim().is_empty() => {
            if has_label(&issue.labels, "needs-owner") {
                format!("hold (needs-owner): {}", hold.reason.trim())
            } else {
                format!("hold: {}", hold.reason.trim())
            }
        }
        _ => format!("hold: {}", skipped.reason),
    };
    BlockedDispatchIssue {
        number: issue.number,
        repository: repository.to_owned(),
        title: issue.title.clone(),
        kind: BlockedIssueKind::Hold,
        reason,
        issue_url,
        last_attempt_result: None,
        last_attempt_thread: None,
        can_lift_hold: true,
        can_retry_attempt: false,
        can_mark_needs_owner: false,
    }
}

fn blocked_needs_owner_issue(
    repository: &str,
    issue: &GitHubIssue,
    skipped: &SkippedIssue,
    issue_url: Option<String>,
) -> BlockedDispatchIssue {
    let is_needs_owner = has_label(&issue.labels, "needs-owner");
    let explicit_hold = parse_explicit_person_hold(&issue.body);
    if is_needs_owner && explicit_hold.is_none() {
        return BlockedDispatchIssue {
            number: issue.number,
            repository: repository.to_owned(),
            title: issue.title.clone(),
            kind: BlockedIssueKind::NeedsOwner,
            reason: "needs-owner".into(),
            issue_url,
            last_attempt_result: None,
            last_attempt_thread: None,
            can_lift_hold: false,
            can_retry_attempt: false,
            can_mark_needs_owner: false,
        };
    }
    let reason = explicit_hold
        .map(|hold| format!("hold: {}", hold.reason.trim()))
        .unwrap_or_else(|| skipped.reason.clone());
    BlockedDispatchIssue {
        number: issue.number,
        repository: repository.to_owned(),
        title: issue.title.clone(),
        kind: if is_needs_owner {
            BlockedIssueKind::NeedsOwner
        } else {
            BlockedIssueKind::Hold
        },
        reason,
        issue_url,
        last_attempt_result: None,
        last_attempt_thread: None,
        can_lift_hold: true,
        can_retry_attempt: false,
        can_mark_needs_owner: false,
    }
}

fn validated_issue_url(repository: &str, issue: &GitHubIssue) -> Option<String> {
    if let Some(url) = allowed_downstream_setup_page(&issue.url) {
        return Some(url);
    }
    match issue.host {
        IssueHost::Collapse => issue_page(repository, issue.number),
        IssueHost::GitHub => allowed_downstream_setup_page(&format!(
            "https://github.com/{repository}/issues/{}",
            issue.number
        )),
    }
}

fn last_attempt_outcome(
    project: &ProjectItem,
    repository: &str,
    number: u32,
) -> (String, Option<u32>) {
    let attempt =
        project.continuous_dispatch.attempts.iter().find(|entry| {
            entry.number == number && entry.repository.eq_ignore_ascii_case(repository)
        });
    let thread = attempt
        .and_then(|entry| entry.thread_id)
        .and_then(|id| project.threads.iter().find(|thread| thread.id == id))
        .or_else(|| {
            project.threads.iter().rev().find(|thread| {
                thread.issue.as_ref().is_some_and(|assigned| {
                    assigned.number == number
                        && assigned.repository.eq_ignore_ascii_case(repository)
                })
            })
        });
    if let Some(thread) = thread {
        return (thread_attempt_summary(thread), Some(thread.id));
    }
    if let Some(result) = attempt
        .and_then(|entry| entry.last_result.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        return (result.to_owned(), attempt.and_then(|entry| entry.thread_id));
    }
    (
        "Previous attempt finished without updating the issue".into(),
        attempt.and_then(|entry| entry.thread_id),
    )
}

/// Formats skipped issues for the project health strip and `velocity.log` (#509, #617),
/// separating `Needs owner`, `Blocked upstream`, and `Tracking` from regular skipped issues.
pub fn summarize_skipped_issues(skipped: &[SkippedIssue], limit: usize) -> String {
    let groups = partition_skipped_issues(skipped);
    let mut sections = Vec::new();
    if !groups.other.is_empty() {
        sections.push(format_skipped_group("Skipped", &groups.other, limit));
    }
    if !groups.needs_owner.is_empty() {
        sections.push(format_skipped_group(
            "Needs owner",
            &groups.needs_owner,
            limit,
        ));
    }
    if !groups.blocked_upstream.is_empty() {
        sections.push(format_skipped_group(
            "Blocked upstream",
            &groups.blocked_upstream,
            limit,
        ));
    }
    if !groups.tracking.is_empty() {
        sections.push(format_skipped_group("Tracking", &groups.tracking, limit));
    }
    sections.join(" · ")
}

fn format_skipped_group(label: &str, items: &[&SkippedIssue], limit: usize) -> String {
    let shown: Vec<String> = items
        .iter()
        .take(limit)
        .map(|item| {
            if item.category == SkipCategory::Tracking && item.reason == "tracking" {
                format!("#{}", item.number)
            } else {
                format!("#{} ({})", item.number, item.reason)
            }
        })
        .collect();
    let remaining = items.len().saturating_sub(limit);
    if remaining > 0 {
        format!("{label}: {} (+{remaining} more)", shown.join(", "))
    } else {
        format!("{label}: {}", shown.join(", "))
    }
}

/// How long dispatch slots must be idle before retrying an attempted issue whose last
/// attempt ended without a landed change (#617).
pub const ATTEMPT_IDLE_RETRY_SECONDS: u64 = 30 * 60;

/// Why a stored "already attempted at this version" record should be cleared (#617).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AttemptClearReason {
    ClosedOrVersionChanged,
    BlockerClosed(String),
    IdleWithoutLand,
}

/// Planned reconciliation for a project's `continuous_dispatch.attempts` (#617).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AttemptReconcile {
    pub clear: Vec<(u32, AttemptClearReason)>,
    pub add_open_blockers: BTreeMap<u32, Vec<String>>,
}

/// Collects currently open blockers referenced by `issue` (`open_blockers`, explicit
/// `waits for #N` / `after #N` / `blocked by #N` references, and every same-repository `#N`
/// in `issue.body` and owner or plan `comments`, ignoring code spans and links to other
/// repositories) (#617, #618).
pub fn open_referenced_blockers_for_issue(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    open_numbers: &HashSet<u32>,
    comments: &[String],
) -> Vec<String> {
    open_referenced_blockers_for_issue_with_context(
        projects,
        project,
        repository,
        issue,
        open_numbers,
        comments,
        &TextBlockerContext::default(),
    )
}

/// Like [`open_referenced_blockers_for_issue`], also consulting cross-project open issues
/// in `context` so cross-host blockers clear once closed on their own host (#623).
pub fn open_referenced_blockers_for_issue_with_context(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    open_numbers: &HashSet<u32>,
    comments: &[String],
    context: &TextBlockerContext,
) -> Vec<String> {
    let mut open = Vec::new();
    for reference in referenced_blockers_of_issue(repository, issue, comments) {
        let key = reference.relative_to(repository);
        if !blocker_closed_or_satisfied(
            projects,
            project,
            repository,
            issue,
            open_numbers,
            &reference,
            context,
        ) && !open
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&key))
        {
            open.push(key);
        }
    }
    open
}

/// Evaluates stored `DispatchAttempt` records against the open backlog, closing blockers,
/// and slot idle time (#617, #618).
pub fn attempts_to_clear(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issues: &[GitHubIssue],
    comments_by_issue: &BTreeMap<u32, Vec<String>>,
    slots_idle_secs: Option<u64>,
    now: u64,
) -> AttemptReconcile {
    attempts_to_clear_with_context(
        projects,
        project,
        repository,
        issues,
        comments_by_issue,
        slots_idle_secs,
        now,
        &TextBlockerContext::default(),
    )
}

/// Like [`attempts_to_clear`], also consulting `context` so cross-host blockers clear
/// stored attempts once closed in their own project's backlog (#623).
#[allow(clippy::too_many_arguments)]
pub fn attempts_to_clear_with_context(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issues: &[GitHubIssue],
    comments_by_issue: &BTreeMap<u32, Vec<String>>,
    slots_idle_secs: Option<u64>,
    now: u64,
    context: &TextBlockerContext,
) -> AttemptReconcile {
    let mut plan = AttemptReconcile::default();
    let open_numbers: HashSet<u32> = issues
        .iter()
        .filter(|issue| issue.state == IssueState::Open)
        .map(|issue| issue.number)
        .collect();
    let by_number: BTreeMap<u32, &GitHubIssue> = issues
        .iter()
        .filter(|issue| issue.state == IssueState::Open)
        .map(|issue| (issue.number, issue))
        .collect();

    for attempt in &project.continuous_dispatch.attempts {
        if !attempt.repository.eq_ignore_ascii_case(repository) {
            continue;
        }
        let Some(issue) = by_number.get(&attempt.number).copied() else {
            plan.clear
                .push((attempt.number, AttemptClearReason::ClosedOrVersionChanged));
            continue;
        };
        if attempt.updated_at != issue.updated_at {
            plan.clear
                .push((attempt.number, AttemptClearReason::ClosedOrVersionChanged));
            continue;
        }
        let comments = comments_by_issue
            .get(&issue.number)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let currently_open = open_referenced_blockers_for_issue_with_context(
            projects,
            project,
            repository,
            issue,
            &open_numbers,
            comments,
            context,
        );
        if currently_open.is_empty()
            && let Some(closed) = closed_blockers_for_attempt(
                projects,
                project,
                repository,
                issue,
                attempt,
                &open_numbers,
                comments,
                context,
            )
        {
            plan.clear
                .push((attempt.number, AttemptClearReason::BlockerClosed(closed)));
            continue;
        }
        let missing_open: Vec<String> = currently_open
            .into_iter()
            .filter(|blocker| {
                !attempt
                    .open_referenced_blockers
                    .iter()
                    .any(|saved| saved.eq_ignore_ascii_case(blocker))
            })
            .collect();
        if !missing_open.is_empty() {
            plan.add_open_blockers.insert(issue.number, missing_open);
        }
        if should_clear_attempt_on_idle(project, repository, issue, attempt, slots_idle_secs, now) {
            plan.clear
                .push((attempt.number, AttemptClearReason::IdleWithoutLand));
        }
    }
    plan
}

fn referenced_blockers_of_issue(
    repository: &str,
    issue: &GitHubIssue,
    comments: &[String],
) -> Vec<IssueReference> {
    let mut references = Vec::new();
    let mut push_unique = |reference: IssueReference| {
        if !reference.is_issue(repository, repository, issue.number)
            && !references.contains(&reference)
        {
            references.push(reference);
        }
    };
    for raw in issue
        .open_blockers
        .iter()
        .chain(issue.closed_blockers.iter())
    {
        if let Some(reference) = IssueReference::parse(raw) {
            push_unique(reference);
        }
    }
    for reference in referenced_blockers_in_text(&issue.body) {
        push_unique(reference);
    }
    for reference in velocity_core::issue_dependencies::same_repo_issue_references_in_text(
        &issue.body,
        repository,
    ) {
        push_unique(reference);
    }
    for comment in comments {
        for reference in referenced_blockers_in_text(comment) {
            push_unique(reference);
        }
        if velocity_core::issue_dependencies::is_owner_or_plan_comment(comment) {
            for reference in velocity_core::issue_dependencies::same_repo_issue_references_in_text(
                comment, repository,
            ) {
                push_unique(reference);
            }
        }
    }
    references
}

/// Returns the formatted closed blockers once every blocker candidate recorded for `attempt`
/// has closed or passed integration (#617, #618).
#[allow(clippy::too_many_arguments)]
fn closed_blockers_for_attempt(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    attempt: &velocity_store::continuous_dispatch::DispatchAttempt,
    open_numbers: &HashSet<u32>,
    comments: &[String],
    context: &TextBlockerContext,
) -> Option<String> {
    if !attempt.open_referenced_blockers.is_empty() {
        let mut closed = Vec::new();
        for saved in &attempt.open_referenced_blockers {
            let reference = IssueReference::parse(saved)?;
            if !blocker_closed_or_satisfied(
                projects,
                project,
                repository,
                issue,
                open_numbers,
                &reference,
                context,
            ) {
                return None;
            }
            closed.push(reference.relative_to(repository));
        }
        return (!closed.is_empty()).then(|| closed.join(", "));
    }
    // Legacy attempts saved before #617 had no `attempted_at` or open-blocker snapshot:
    // if every referenced or linked blocker is already closed, clear so dispatch retries.
    if attempt.attempted_at.is_none() {
        let references = referenced_blockers_of_issue(repository, issue, comments);
        if !references.is_empty()
            && references.iter().all(|reference| {
                blocker_closed_or_satisfied(
                    projects,
                    project,
                    repository,
                    issue,
                    open_numbers,
                    reference,
                    context,
                )
            })
        {
            let closed: Vec<String> = references
                .iter()
                .map(|reference| reference.relative_to(repository))
                .collect();
            return Some(closed.join(", "));
        }
    }
    None
}

fn blocker_closed_or_satisfied(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    open_numbers: &HashSet<u32>,
    reference: &IssueReference,
    context: &TextBlockerContext,
) -> bool {
    let relative = reference.relative_to(repository);
    if issue
        .closed_blockers
        .iter()
        .any(|closed| closed.eq_ignore_ascii_case(&relative))
    {
        return true;
    }
    if blocker_satisfied_in_projects(projects, project, repository, &relative) {
        return true;
    }
    let target_repo = reference.repository_or(repository);
    if target_repo.eq_ignore_ascii_case(repository) {
        return !open_numbers.contains(&reference.number());
    }
    if let Some((_, _, canonical_repo)) =
        resolve_project_for_repository(projects, project, repository, target_repo)
    {
        let open_set = context
            .open_numbers_for(&canonical_repo)
            .or_else(|| context.open_numbers_for(target_repo));
        if let Some(open_set) = open_set {
            return !open_set.contains(&reference.number());
        }
    }
    false
}

fn should_clear_attempt_on_idle(
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    attempt: &velocity_store::continuous_dispatch::DispatchAttempt,
    slots_idle_secs: Option<u64>,
    now: u64,
) -> bool {
    let Some(idle) = slots_idle_secs else {
        return false;
    };
    if idle < ATTEMPT_IDLE_RETRY_SECONDS {
        return false;
    }
    if attempt
        .attempted_at
        .is_some_and(|at| now.saturating_sub(at) < ATTEMPT_IDLE_RETRY_SECONDS)
    {
        return false;
    }
    let identity = ThreadIssue::new(repository.into(), issue.number);
    let version = IssueVersion::from_github(issue);
    if issue_thread(project, &identity).is_some()
        || project
            .continuous_dispatch
            .reporting_no_change(repository, issue.number)
    {
        return false;
    }
    !issue_has_landed_change(project, &identity, version)
}

fn issue_has_landed_change(
    project: &ProjectItem,
    identity: &ThreadIssue,
    version: IssueVersion<'_>,
) -> bool {
    project.rosen_issue_completed_for_version(identity, version)
        || project
            .landed_awaiting_integration_for_version(identity, version)
            .is_some()
        || project.reopened_landed_hold(identity, version).is_some()
}

/// Unsatisfied open blockers for `issue` in `project` (#529, #608, #623), combining host
/// dependency links (`open_blockers`) with explicit text blockers (`Waits for`, `Blocked by`,
/// `After`) in `issue.body` and owner comments resolved across `projects` regardless of host.
pub fn unsatisfied_blockers_with_context(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    context: &TextBlockerContext,
) -> (Vec<String>, u32) {
    let unnamed = issue
        .open_blocker_count
        .saturating_sub(u32::try_from(issue.open_blockers.len()).unwrap_or(u32::MAX));
    let mut named: Vec<String> = issue
        .open_blockers
        .iter()
        .filter(|blocker| !blocker_satisfied_in_projects(projects, project, repository, blocker))
        .cloned()
        .collect();
    for blocker in
        unsatisfied_text_blockers_with_context(projects, project, repository, issue, context)
    {
        if !named
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&blocker))
        {
            named.push(blocker);
        }
    }
    (named, unnamed)
}

/// Unsatisfied explicit text blockers (`Waits for`, `Blocked by`, `After` in `issue.body` or
/// owner comments) not already satisfied or listed in `issue.open_blockers` / `closed_blockers` (#623).
pub fn unsatisfied_text_blockers_with_context(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    context: &TextBlockerContext,
) -> Vec<String> {
    if context.open_issues_by_repo.is_empty() {
        return Vec::new();
    }
    let comments = context.comments_for(issue.number);
    let mut unsatisfied = Vec::new();
    for reference in explicit_text_blockers_for_issue(repository, issue, comments) {
        let relative = reference.relative_to(repository);
        if issue_lists_blocker(issue, &relative) {
            continue;
        }
        let target_repo = reference.repository_or(repository);
        let Some((target_project, _, canonical_repo)) =
            resolve_project_for_repository(projects, project, repository, target_repo)
        else {
            continue;
        };
        if text_blocker_satisfied_green(
            projects,
            project,
            target_project,
            repository,
            target_repo,
            &canonical_repo,
            &reference,
        ) {
            continue;
        }
        let open_set = context
            .open_numbers_for(&canonical_repo)
            .or_else(|| context.open_numbers_for(target_repo));
        let is_open_or_unloaded = open_set.is_none_or(|set| set.contains(&reference.number()));
        if is_open_or_unloaded
            && !unsatisfied
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(&relative))
        {
            unsatisfied.push(relative);
        }
    }
    unsatisfied
}

/// Explicit text blockers on `issue` (body or owner comments) that live on the same issue
/// host as `project`, are currently confirmed open and unsatisfied, and are not already
/// linked in `issue.open_blockers` or `issue.closed_blockers` (#623).
pub fn same_host_text_blockers_to_link(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    context: &TextBlockerContext,
) -> Vec<IssueReference> {
    let subject_host = project.effective_issue_host();
    let comments = context.comments_for(issue.number);
    let mut to_link = Vec::new();
    for reference in explicit_text_blockers_for_issue(repository, issue, comments) {
        let relative = reference.relative_to(repository);
        if issue_lists_blocker(issue, &relative) {
            continue;
        }
        let target_repo = reference.repository_or(repository);
        let Some((target_project, target_host, canonical_repo)) =
            resolve_project_for_repository(projects, project, repository, target_repo)
        else {
            continue;
        };
        if target_host != subject_host {
            continue;
        }
        if text_blocker_satisfied_green(
            projects,
            project,
            target_project,
            repository,
            target_repo,
            &canonical_repo,
            &reference,
        ) {
            continue;
        }
        let confirmed_open = context
            .open_numbers_for(&canonical_repo)
            .or_else(|| context.open_numbers_for(target_repo))
            .is_some_and(|set| set.contains(&reference.number()));
        if !confirmed_open {
            continue;
        }
        let canonical = if canonical_repo.eq_ignore_ascii_case(repository) {
            IssueReference::new(None, reference.number())
        } else {
            IssueReference::new(Some(&canonical_repo), reference.number())
        };
        if let Some(canonical) = canonical
            && !to_link.contains(&canonical)
        {
            to_link.push(canonical);
        }
    }
    to_link
}

/// Project IDs of registered sibling projects referenced by explicit text blockers across
/// `issues` (body or cached owner comments) (#623).
pub fn referenced_blocker_project_ids(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issues: &[GitHubIssue],
    comments_by_issue: &BTreeMap<u32, Vec<String>>,
) -> Vec<u32> {
    let mut ids = Vec::new();
    for issue in issues {
        let comments = comments_by_issue
            .get(&issue.number)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        for reference in explicit_text_blockers_for_issue(repository, issue, comments) {
            let target_repo = reference.repository_or(repository);
            if let Some((target_project, _, _)) =
                resolve_project_for_repository(projects, project, repository, target_repo)
                && target_project.id != project.id
                && !ids.contains(&target_project.id)
            {
                ids.push(target_project.id);
            }
        }
    }
    ids
}

fn explicit_text_blockers_for_issue(
    repository: &str,
    issue: &GitHubIssue,
    comments: &[String],
) -> Vec<IssueReference> {
    let mut references = Vec::new();
    let mut push_unique = |reference: IssueReference| {
        if reference.is_issue(repository, repository, issue.number) {
            return;
        }
        let repo = reference.repository_or(repository);
        let duplicate = references.iter().any(|existing: &IssueReference| {
            existing.number() == reference.number()
                && existing
                    .repository_or(repository)
                    .eq_ignore_ascii_case(repo)
        });
        if !duplicate {
            references.push(reference);
        }
    };
    for reference in explicit_text_blockers(&issue.body) {
        push_unique(reference);
    }
    for comment in comments {
        if is_owner_comment(comment) {
            for reference in explicit_text_blockers(comment) {
                push_unique(reference);
            }
        }
    }
    references
}

fn issue_lists_blocker(issue: &GitHubIssue, relative: &str) -> bool {
    issue
        .open_blockers
        .iter()
        .chain(issue.closed_blockers.iter())
        .any(|existing| existing.eq_ignore_ascii_case(relative))
}

fn resolve_project_for_repository<'a>(
    projects: &'a [ProjectItem],
    current: &'a ProjectItem,
    current_repository: &str,
    target_repository: &str,
) -> Option<(&'a ProjectItem, IssueHost, String)> {
    if target_repository.eq_ignore_ascii_case(current_repository) {
        return Some((
            current,
            current.effective_issue_host(),
            current_repository.to_owned(),
        ));
    }
    let matched = projects
        .iter()
        .chain(std::iter::once(current))
        .find(|item| project_matches_repository(item, target_repository))?;
    let host = matched.effective_issue_host();
    let canonical = matched
        .issue_host_repository()
        .map(|(_, repo)| repo)
        .unwrap_or_else(|| target_repository.to_owned());
    Some((matched, host, canonical))
}

fn project_matches_repository(item: &ProjectItem, repository: &str) -> bool {
    item.collapse_repository
        .as_deref()
        .is_some_and(|configured| configured.eq_ignore_ascii_case(repository))
        || item
            .github_remote
            .as_deref()
            .is_some_and(|configured| configured.eq_ignore_ascii_case(repository))
}

fn text_blocker_satisfied_green(
    projects: &[ProjectItem],
    project: &ProjectItem,
    target_project: &ProjectItem,
    repository: &str,
    target_repo: &str,
    canonical_repo: &str,
    reference: &IssueReference,
) -> bool {
    let canonical_identity = ThreadIssue::new(canonical_repo.to_owned(), reference.number());
    if target_project.rosen_issue_integration_green(&canonical_identity) {
        return true;
    }
    if !canonical_repo.eq_ignore_ascii_case(target_repo) {
        let raw_identity = ThreadIssue::new(target_repo.to_owned(), reference.number());
        if target_project.rosen_issue_integration_green(&raw_identity) {
            return true;
        }
    }
    let relative = reference.relative_to(repository);
    blocker_satisfied_in_projects(projects, project, repository, &relative)
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
    has_unsatisfied_blockers_with_context(
        projects,
        project,
        repository,
        issue,
        &TextBlockerContext::default(),
    )
}

/// Like [`has_unsatisfied_blockers_in_projects`], also resolving explicit text blockers in
/// `issue.body` and cached owner comments against `context` (#623).
pub fn has_unsatisfied_blockers_with_context(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    context: &TextBlockerContext,
) -> bool {
    let (named, unnamed) =
        unsatisfied_blockers_with_context(projects, project, repository, issue, context);
    unnamed > 0 || !named.is_empty()
}

fn actionable_without_blockers(issue: &GitHubIssue) -> bool {
    !(issue.state != IssueState::Open
        || issue.is_spec_question()
        || has_label(&issue.labels, TRACKING_LABEL)
        || has_label(&issue.labels, BLOCKED_UPSTREAM_LABEL)
        || issue.labels.iter().any(|label| is_person_hold_label(label))
        || issue.labels.iter().any(|label| {
            SKIP_LABELS
                .iter()
                .any(|skip| label.eq_ignore_ascii_case(skip))
        }))
}

/// Shared automatic-work eligibility for continuous dispatch and the F16 work pool.
/// Only explicit labels (`needs-owner`, `hold`, `tracking`, `blocked-upstream`, etc.)
/// park an issue; prose discussing decisions never skips dispatch (#617).
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
    actionable_in_projects_with_blockers(
        projects,
        project,
        repository,
        issue,
        &TextBlockerContext::default(),
    )
}

/// Like [`actionable_in_projects`], also resolving explicit text blockers via `context` (#623).
pub fn actionable_in_projects_with_blockers(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    context: &TextBlockerContext,
) -> bool {
    !has_unsatisfied_blockers_with_context(projects, project, repository, issue, context)
        && actionable_without_blockers(issue)
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
    dispatchable_issue_with_blockers(
        projects,
        project,
        repository,
        issue,
        claimed,
        &TextBlockerContext::default(),
    )
}

/// Like [`dispatchable_issue_in_projects`], also checking explicit text blockers in `context` (#623).
pub fn dispatchable_issue_with_blockers(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    claimed: &HashSet<String>,
    context: &TextBlockerContext,
) -> bool {
    let identity = ThreadIssue::new(repository.into(), issue.number);
    let version = IssueVersion::from_github(issue);
    actionable_in_projects_with_blockers(projects, project, repository, issue, context)
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
    let context = TextBlockerContext::for_backlog(repository, issues);
    ordered_issues_with_blockers(
        projects, project, repository, issues, claimed, unblocks, &context,
    )
}

/// Like [`ordered_issues_in_projects`], also resolving explicit text blockers across
/// `projects` using `context` (#623).
pub fn ordered_issues_with_blockers(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issues: &[GitHubIssue],
    claimed: &HashSet<String>,
    unblocks: &BTreeMap<u32, u32>,
    context: &TextBlockerContext,
) -> Vec<(GitHubIssue, u32)> {
    let mut owned_context = None;
    let effective_context =
        if !repository.is_empty() && context.open_numbers_for(repository).is_none() {
            let mut cloned = context.clone();
            cloned.insert_open_issues(repository, issues);
            owned_context.insert(cloned)
        } else {
            context
        };
    let mut ready: Vec<_> = issues
        .iter()
        .filter(|issue| {
            dispatchable_issue_with_blockers(
                projects,
                project,
                repository,
                issue,
                claimed,
                effective_context,
            )
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
    // Dispatch starts ready `self-improvement` issues ahead of other ready issues (#622).
    let self_improvement = |issue: &GitHubIssue| {
        has_label(
            &issue.labels,
            velocity_core::improvement_scout::SELF_IMPROVEMENT_LABEL,
        )
    };
    ready.sort_by(|(left, left_unblocks), (right, right_unblocks)| {
        self_improvement(right)
            .cmp(&self_improvement(left))
            .then_with(|| right_unblocks.cmp(left_unblocks))
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
    let context = TextBlockerContext::for_backlog(repository, issues);
    ordered_work_with_blockers(
        projects,
        project,
        repository,
        issues,
        claimed,
        issues_known,
        unblocks,
        &context,
    )
}

/// Like [`ordered_work_in_projects`], also resolving explicit text blockers via `context` (#623).
#[allow(clippy::too_many_arguments)]
pub fn ordered_work_with_blockers(
    projects: &[ProjectItem],
    project: &ProjectItem,
    repository: &str,
    issues: &[GitHubIssue],
    claimed: &HashSet<String>,
    issues_known: bool,
    unblocks: &BTreeMap<u32, u32>,
    context: &TextBlockerContext,
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
            ordered_issues_with_blockers(
                projects, project, repository, issues, claimed, unblocks, context,
            )
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

/// Whether a thread's own land, abandon or empty-dispatch cleanup is still pending
/// and responsible for releasing its AIRC claim (#656).
fn has_pending_finish_cleanup(project: &ProjectItem, thread: &velocity_store::ThreadItem) -> bool {
    project.continuous_dispatch.cleanup.contains(&thread.id)
        || thread.rosen_follow_ups.iter().any(|follow_up| {
            matches!(
                follow_up,
                velocity_store::rosen::RosenFollowUp::FinishLanded
                    | velocity_store::rosen::RosenFollowUp::FinishAbandoned
            )
        })
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
                && (!matches!(thread.state, ThreadState::Merged | ThreadState::Done)
                    || has_pending_finish_cleanup(project, thread))
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

    // Risk (#656): landed, abandoned, or empty-dispatch threads whose own finish follow-up
    // or cleanup has not finished yet must not be flagged and reaped as stale AIRC claims,
    // while finished threads without pending cleanup must still be reaped.
    #[test]
    fn pending_finish_cleanup_threads_are_not_flagged_as_stale_claims() {
        let mut project = ProjectItem::new(1, "velocity".into(), "Z:/repo".into());

        let mut landing = ThreadItem::new(10, "Landing".into());
        landing.state = ThreadState::Merged;
        landing
            .rosen_follow_ups
            .push(velocity_store::rosen::RosenFollowUp::FinishLanded);

        let mut abandoning = ThreadItem::new(11, "Abandoning".into());
        abandoning.state = ThreadState::Done;
        abandoning
            .rosen_follow_ups
            .push(velocity_store::rosen::RosenFollowUp::FinishAbandoned);

        let mut cleaning = ThreadItem::new(12, "Cleaning".into());
        cleaning.state = ThreadState::Done;
        project.continuous_dispatch.cleanup.push(12);

        let mut stale_merged = ThreadItem::new(13, "StaleMerged".into());
        stale_merged.state = ThreadState::Merged;

        let mut released_landing = ThreadItem::new(14, "Released".into());
        released_landing.state = ThreadState::Merged;
        released_landing.supervision.released = true;
        released_landing
            .rosen_follow_ups
            .push(velocity_store::rosen::RosenFollowUp::FinishLanded);

        project.threads.extend([
            landing,
            abandoning,
            cleaning,
            stale_merged,
            released_landing,
        ]);

        let live = LiveLocalMembers::default();
        let claims = vec![
            sample_claim("thread-10", "issue-627-10", "issue:627", true),
            sample_claim("thread-11", "issue-628-11", "issue:628", true),
            sample_claim("thread-12", "issue-629-12", "issue:629", true),
            sample_claim("thread-13", "issue-630-13", "issue:630", true),
            sample_claim("thread-14", "issue-631-14", "issue:631", true),
        ];
        let filtered = filter_live_claims(&project, &claims, &live);

        let dead_members: Vec<&str> = filtered
            .dead_local
            .iter()
            .map(|claim| claim.member.as_str())
            .collect();
        assert_eq!(dead_members, vec!["thread-13", "thread-14"]);
        assert!(filtered.active.contains("issue:627"));
        assert!(filtered.active.contains("issue:628"));
        assert!(filtered.active.contains("issue:629"));
        assert!(!filtered.active.contains("issue:630"));
        assert!(!filtered.active.contains("issue:631"));

        // Once FinishLanded finishes, a remaining claim on that merged thread is stale.
        project.threads[0].rosen_follow_ups.clear();
        assert!(!is_live_local_member(&project, "thread-10", &live));
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

    // Risk (#617): prose discussing owner decisions must dispatch normally, while explicit
    // hold labels (`needs-owner`, `blocked-upstream`, `tracking`) are separated in health.
    #[test]
    fn decision_prose_dispatches_and_health_separates_hold_kinds()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = ProjectItem::new(1, "velocity".into(), "Z:/velocity".into());
        project.version_control = VersionControl::Rosen;
        project.spec_locked = true;
        let claimed = HashSet::new();

        let mut prose = sample_issue(616);
        prose.title = "Tell the owner when dispatch is idle".into();
        prose.body = "Document what the owner gets told. No decision needed yet; suggested: decide whether to log or show in health. Previously awaiting an owner decision was matched by text.".into();
        assert!(actionable(&prose));
        assert!(dispatchable_issue(
            &project,
            "mESmaC/velocity",
            &prose,
            &claimed
        ));
        assert!(issue_skip_reason(&project, "mESmaC/velocity", &prose, &claimed, 0).is_none());

        let mut needs_owner = sample_issue(555);
        needs_owner.labels = vec!["needs-owner".into()];
        needs_owner.body = "Needs physical hardware setup.".into();
        let mut upstream = sample_issue(478);
        upstream.labels = vec!["blocked-upstream".into()];
        upstream.body = "Blocked upstream: `tauri-apps/wry#1862`".into();
        let mut tracking = sample_issue(438);
        tracking.labels = vec!["tracking".into()];
        tracking.body = "- [x] #419\n- [ ] #418\n".into();
        let mut blocked = sample_issue(418);
        blocked.open_blocker_count = 1;
        blocked.open_blockers = vec!["#419".into()];

        let skipped: Vec<SkippedIssue> = [&needs_owner, &upstream, &tracking, &blocked]
            .into_iter()
            .filter_map(|issue| issue_skip_reason(&project, "mESmaC/velocity", issue, &claimed, 0))
            .collect();
        let groups = partition_skipped_issues(&skipped);
        assert_eq!(groups.needs_owner.len(), 1);
        assert_eq!(groups.needs_owner[0].number, 555);
        assert_eq!(groups.blocked_upstream.len(), 1);
        assert_eq!(groups.blocked_upstream[0].number, 478);
        assert!(
            groups.blocked_upstream[0]
                .reason
                .contains("tauri-apps/wry#1862")
        );
        assert!(
            groups.blocked_upstream[0]
                .reason
                .contains("https://github.com/tauri-apps/wry/issues/1862")
        );
        assert_eq!(groups.tracking.len(), 1);
        assert_eq!(groups.tracking[0].number, 438);
        assert_eq!(groups.other.len(), 1);
        assert_eq!(groups.other[0].number, 418);

        let summary = summarize_skipped_issues(&skipped, 6);
        assert!(summary.contains("Skipped: #418"));
        assert!(summary.contains("Needs owner: #555"));
        assert!(summary.contains("Blocked upstream: #478"));
        assert!(summary.contains("Tracking: #438"));
        Ok(())
    }

    // Risk (#617): an attempted issue whose blocker closes must clear automatically and
    // dispatch again without looping, and idle slots after 30 min clear unlanded attempts.
    #[test]
    fn attempted_issue_clears_when_blocker_closes_or_after_idle_without_land()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut project = ProjectItem::new(1, "velocity".into(), "Z:/velocity".into());
        project.version_control = VersionControl::Rosen;
        project.spec_locked = true;
        let repo = "mESmaC/velocity";
        let mut issue_418 = sample_issue(418);
        issue_418.body = "Remove git project mode. Waits for #419.".into();
        let issue_419 = sample_issue(419);

        // Attempt recorded while #419 was open.
        project.continuous_dispatch.attempts.push(
            velocity_store::continuous_dispatch::DispatchAttempt {
                repository: repo.into(),
                number: 418,
                updated_at: issue_418.updated_at.clone(),
                attempted_at: Some(1_000),
                open_referenced_blockers: vec!["#419".into()],
                thread_id: None,
                last_result: None,
            },
        );
        // While #419 is still open, attempt stays.
        let while_open = attempts_to_clear(
            &[],
            &project,
            repo,
            &[issue_418.clone(), issue_419],
            &BTreeMap::new(),
            None,
            1_100,
        );
        assert!(while_open.clear.is_empty());

        // Once #419 closes (absent from open backlog), attempt clears so #418 dispatches again.
        let after_close = attempts_to_clear(
            &[],
            &project,
            repo,
            &[issue_418.clone()],
            &BTreeMap::new(),
            None,
            1_200,
        );
        assert_eq!(
            after_close.clear,
            vec![(418, AttemptClearReason::BlockerClosed("#419".into()))]
        );

        // If #418 is re-attempted AFTER #419 is already closed, open_referenced_blockers is empty
        // so it must NOT immediately clear again in a loop.
        project.continuous_dispatch.attempts[0]
            .open_referenced_blockers
            .clear();
        project.continuous_dispatch.attempts[0].attempted_at = Some(1_300);
        let after_retry = attempts_to_clear(
            &[],
            &project,
            repo,
            &[issue_418.clone()],
            &BTreeMap::new(),
            Some(60),
            1_400,
        );
        assert!(after_retry.clear.is_empty());

        // After 30 min of idle slots with no landed change, the attempt clears for retry.
        let after_idle = attempts_to_clear(
            &[],
            &project,
            repo,
            &[issue_418.clone()],
            &BTreeMap::new(),
            Some(ATTEMPT_IDLE_RETRY_SECONDS),
            1_300 + ATTEMPT_IDLE_RETRY_SECONDS,
        );
        assert_eq!(
            after_idle.clear,
            vec![(418, AttemptClearReason::IdleWithoutLand)]
        );

        // If #418 HAS landed, idle timeout must NOT clear its attempt.
        project
            .rosen_work_items
            .push(velocity_core::work_pool::WorkItem {
                key: velocity_core::work_pool::WorkKey::issue(repo, 418),
                title: issue_418.title.clone(),
                thread_identifier: None,
                change: Some("chg-418".into()),
                status: velocity_core::work_pool::WorkStatus::Landed,
                claimed_at: None,
                updated_at: None,
                landed_at: None,
                green_at: None,
                reverted_at: None,
                landed_commit: Some("abc1234".into()),
                revert_reason: None,
            });
        let landed_idle = attempts_to_clear(
            &[],
            &project,
            repo,
            &[issue_418],
            &BTreeMap::new(),
            Some(ATTEMPT_IDLE_RETRY_SECONDS),
            1_300 + ATTEMPT_IDLE_RETRY_SECONDS,
        );
        assert!(landed_idle.clear.is_empty());
        Ok(())
    }

    #[test]
    fn blocked_dispatch_issue_classifies_needs_owner_hold_and_attempted_with_result() {
        let repo = "mESmaC/velocity";
        let mut project = ProjectItem::new(1, "velocity".into(), r"Z:\velocity".into());
        project.spec_locked = true;
        project.continuous_dispatch.slots = 3;

        // #478: needs-owner -> open issue only.
        let mut issue_478 = sample_issue(478);
        issue_478.labels = vec!["needs-owner".into()];
        issue_478.url = "https://collapse.sh/mESmaC/velocity/issues/478".into();
        let skipped_478 = issue_skip_reason(&project, repo, &issue_478, &HashSet::new(), 1_000);
        let blocked_478 = skipped_478
            .as_ref()
            .and_then(|skip| blocked_dispatch_issue(&project, repo, &issue_478, skip));
        assert!(blocked_478.is_some());
        if let Some(entry) = blocked_478 {
            assert_eq!(entry.kind, BlockedIssueKind::NeedsOwner);
            assert_eq!(entry.reason, "needs-owner");
            assert_eq!(
                entry.issue_url.as_deref(),
                Some("https://collapse.sh/mESmaC/velocity/issues/478")
            );
            assert!(!entry.can_lift_hold);
        }

        // #438: both hold and needs-owner -> shows hold reason and offers Lift hold + Open.
        let mut issue_438 = sample_issue(438);
        issue_438.labels = vec!["hold".into(), "needs-owner".into()];
        issue_438.body =
            "Hold: Waiting on physical hardware\nRelease when: Hardware arrives".into();
        let skipped_438 = issue_skip_reason(&project, repo, &issue_438, &HashSet::new(), 1_000);
        let blocked_438 = skipped_438
            .as_ref()
            .and_then(|skip| blocked_dispatch_issue(&project, repo, &issue_438, skip));
        assert!(blocked_438.is_some());
        if let Some(entry) = blocked_438 {
            assert_eq!(entry.kind, BlockedIssueKind::Hold);
            assert!(entry.reason.contains("Waiting on physical hardware"));
            assert!(entry.can_lift_hold);
            assert!(entry.issue_url.is_some());
        }

        // #417: already attempted at this version -> shows last attempt result + Try again + Needs owner.
        let issue_417 = sample_issue(417);
        let mut attempt = velocity_store::continuous_dispatch::DispatchAttempt::new(
            repo.into(),
            417,
            issue_417.updated_at.clone(),
        );
        attempt.last_result = Some("No change: waited on #421".into());
        project.continuous_dispatch.attempts.push(attempt);
        let skipped_417 = issue_skip_reason(&project, repo, &issue_417, &HashSet::new(), 1_000);
        let blocked_417 = skipped_417
            .as_ref()
            .and_then(|skip| blocked_dispatch_issue(&project, repo, &issue_417, skip));
        assert!(blocked_417.is_some());
        if let Some(entry) = blocked_417 {
            assert_eq!(entry.kind, BlockedIssueKind::Attempted);
            assert!(entry.can_retry_attempt);
            assert!(entry.can_mark_needs_owner);
            assert_eq!(
                entry.last_attempt_result.as_deref(),
                Some("No change: waited on #421")
            );
        }

        // Tracking issues are excluded from blocked issues (#617).
        let mut tracking = sample_issue(500);
        tracking.labels = vec!["tracking".into()];
        let skipped_tracking = issue_skip_reason(&project, repo, &tracking, &HashSet::new(), 1_000);
        assert!(
            skipped_tracking
                .as_ref()
                .and_then(|skip| blocked_dispatch_issue(&project, repo, &tracking, skip))
                .is_none()
        );
    }

    // Risk (#618): an attempted issue whose owner/plan comment lists open same-repo issues
    // by number (like #417 listing #421-#424) must wait until all of them close, ignore
    // code spans and links to other repositories, and dispatch again once every one closes.
    #[test]
    fn plan_comment_open_numbers_clear_attempt_only_once_all_close() {
        let mut project = ProjectItem::new(1, "velocity".into(), "Z:/velocity".into());
        project.version_control = VersionControl::Rosen;
        project.spec_locked = true;
        let repo = "mESmaC/velocity";

        let mut issue_417 = sample_issue(417);
        issue_417.body = "Rosen vocabulary sweep. Ignore `#900` and [#901](https://github.com/other/repo/issues/901).".into();
        let plan_comment = "\
**Decided (owner delegated, 2026-11-17): phased.**\n\
2. Rust and IPC names change inside: #421 (status pane), #422 (thread view), #423 (workspaces), #424 (PR duty).\n\
3. One final sweep for leftovers, after those land.\n\
Also mentions `#902` in code and mESmaC/rosen#903 in another repo.\n"
            .to_owned();
        let agent_progress =
            "Velocity agent `issue-417-10` · [Thread 10](velocity://thread/1/10)\n\nStarting work; unrelated #904 is open."
                .to_owned();
        let comments = vec![plan_comment, agent_progress];

        let open_at_attempt: HashSet<u32> = [417, 421, 422, 423, 424, 900, 901, 902, 903, 904]
            .into_iter()
            .collect();
        let captured = open_referenced_blockers_for_issue(
            &[],
            &project,
            repo,
            &issue_417,
            &open_at_attempt,
            &comments,
        );
        assert_eq!(captured, vec!["#421", "#422", "#423", "#424"]);

        project.continuous_dispatch.attempts.push(
            velocity_store::continuous_dispatch::DispatchAttempt {
                repository: repo.into(),
                number: 417,
                updated_at: issue_417.updated_at.clone(),
                attempted_at: Some(1_000),
                open_referenced_blockers: captured,
                thread_id: None,
                last_result: None,
            },
        );
        let comments_by_issue = BTreeMap::from([(417, comments)]);

        // Closing #421-#423 while #424 is still open must NOT clear the attempt yet.
        let partial = attempts_to_clear(
            &[],
            &project,
            repo,
            &[issue_417.clone(), sample_issue(424), sample_issue(900)],
            &comments_by_issue,
            None,
            1_200,
        );
        assert!(partial.clear.is_empty());

        // Once #424 also closes, all four blockers are closed and the attempt clears.
        let all_closed = attempts_to_clear(
            &[],
            &project,
            repo,
            &[issue_417.clone(), sample_issue(900), sample_issue(904)],
            &comments_by_issue,
            None,
            1_300,
        );
        assert_eq!(
            all_closed.clear,
            vec![(
                417,
                AttemptClearReason::BlockerClosed("#421, #422, #423, #424".into())
            )]
        );

        // After clearing, #417 is dispatchable again.
        project.continuous_dispatch.attempts.clear();
        assert!(dispatchable_issue(
            &project,
            repo,
            &issue_417,
            &HashSet::new()
        ));
    }

    // Risk (#618): restarting Velocity 24 minutes into an idle spell must preserve the
    // persisted idle_since_secs so the 30-minute attempt retry fires 6 minutes after restart.
    #[test]
    fn persisted_idle_spell_triggers_attempt_retry_across_restart()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory =
            std::env::temp_dir().join(format!("velocity-dispatch-618-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory)?;
        let mut project = ProjectItem::new(1, "velocity".into(), directory.display().to_string());
        project.version_control = VersionControl::Rosen;
        project.spec_locked = true;
        project.continuous_dispatch.slots = 3;
        let repo = "mESmaC/velocity";
        let issue_417 = sample_issue(417);
        project.continuous_dispatch.attempts.push(
            velocity_store::continuous_dispatch::DispatchAttempt {
                repository: repo.into(),
                number: 417,
                updated_at: issue_417.updated_at.clone(),
                attempted_at: Some(1_000),
                open_referenced_blockers: Vec::new(),
                thread_id: None,
                last_result: None,
            },
        );
        // Idle spell started at T=1_000.
        project.continuous_dispatch.idle_since_secs = Some(1_000);

        let mut persisted = velocity_store::PersistedState::default();
        persisted.projects.push(project);
        std::fs::write(
            directory.join(velocity_store::STATE_FILE_NAME),
            serde_json::to_vec(&persisted)?,
        )?;

        // Simulate auto-restart at T=24 min (2_440) and check at T=30 min (2_800).
        let reloaded = StateStore::load_or_default(&directory)?;
        let project = reloaded.project(1).ok_or("reloaded project missing")?;
        let idle_since = project.continuous_dispatch.idle_since_secs;
        assert_eq!(idle_since, Some(1_000));

        let at_24m: u64 = 1_000 + 24 * 60;
        let before_threshold = attempts_to_clear(
            &[],
            project,
            repo,
            std::slice::from_ref(&issue_417),
            &BTreeMap::new(),
            idle_since.map(|since| at_24m.saturating_sub(since)),
            at_24m,
        );
        assert!(before_threshold.clear.is_empty());

        let at_30m = 1_000 + ATTEMPT_IDLE_RETRY_SECONDS;
        let after_restart_threshold = attempts_to_clear(
            &[],
            project,
            repo,
            &[issue_417],
            &BTreeMap::new(),
            idle_since.map(|since| at_30m.saturating_sub(since)),
            at_30m,
        );
        assert_eq!(
            after_restart_threshold.clear,
            vec![(417, AttemptClearReason::IdleWithoutLand)]
        );
        std::fs::remove_dir_all(directory)?;
        Ok(())
    }

    // Risk (#622): dispatch must start ready `self-improvement` issues ahead of other ready
    // issues so fleet-speed fixes land before lower-leverage work.
    #[test]
    fn self_improvement_issues_dispatch_ahead_of_other_ready_issues() {
        let mut project = ProjectItem::new(1, "velocity".into(), r"C:\velocity".into());
        project.version_control = VersionControl::Rosen;
        project.spec_locked = true;
        let repo = "mESmaC/velocity";

        let mut regular_blocker = sample_issue(10);
        regular_blocker.created_at = "2026-11-20T00:00:00Z".into();

        let mut self_improvement = sample_issue(20);
        self_improvement.created_at = "2026-11-22T12:00:00Z".into();
        self_improvement.labels.push("self-improvement".into());

        let mut earlier_self_improvement = sample_issue(19);
        earlier_self_improvement.created_at = "2026-11-22T10:00:00Z".into();
        earlier_self_improvement
            .labels
            .push("self-improvement".into());

        let mut unblocks = BTreeMap::new();
        unblocks.insert(10, 5);
        unblocks.insert(20, 1);

        let ordered = ordered_issues(
            &project,
            repo,
            &[regular_blocker, earlier_self_improvement, self_improvement],
            &HashSet::new(),
            &unblocks,
        );
        let numbers: Vec<u32> = ordered.into_iter().map(|(issue, _)| issue.number).collect();
        // Self-improvement issues come first (ordered by unblocks, then age), then regular work.
        assert_eq!(numbers, vec![20, 19, 10]);
    }

    // Risk (#623): explicit text blockers (`Waits for`, `Blocked by`, `After` in issue bodies
    // or owner comments) must block dispatch while open and unblock once closed, both on the
    // same issue host (with native link candidates) and across GitHub <-> Collapse projects,
    // while bare `#N` mentions and agent comments never block dispatch.
    #[test]
    fn explicit_text_blockers_hold_and_unblock_for_same_host_and_cross_host_projects()
    -> Result<(), Box<dyn std::error::Error>> {
        let mut collapse_sh = ProjectItem::new(1, "collapse.sh".into(), "Z:/collapse".into());
        collapse_sh.version_control = VersionControl::Rosen;
        collapse_sh.spec_locked = true;
        collapse_sh.github_remote = Some("mESmaC/collapse.sh".into());
        collapse_sh.issue_host = Some(IssueHost::GitHub);

        let mut velocity = ProjectItem::new(2, "velocity".into(), "Z:/velocity".into());
        velocity.version_control = VersionControl::Rosen;
        velocity.spec_locked = true;
        velocity.github_remote = Some("mESmaC/velocity".into());
        velocity.collapse_repository = Some("mESmaC/velocity".into());
        velocity.issue_host = Some(IssueHost::Collapse);

        let mut rosen = ProjectItem::new(3, "rosen".into(), "Z:/rosen".into());
        rosen.version_control = VersionControl::Rosen;
        rosen.spec_locked = true;
        rosen.github_remote = Some("mESmaC/rosen".into());
        rosen.collapse_repository = Some("mESmaC/rosen".into());
        rosen.issue_host = Some(IssueHost::Collapse);

        let projects = vec![collapse_sh.clone(), velocity.clone(), rosen.clone()];
        let claimed = HashSet::new();

        // --- Same-host (Collapse) case: velocity#621 waits for #619, plus owner comment ---
        let mut velocity_621 = sample_issue(621);
        velocity_621.host = IssueHost::Collapse;
        velocity_621.body = "Waits for #619.\nSee #618 for context.".into();

        let mut velocity_622 = sample_issue(622);
        velocity_622.host = IssueHost::Collapse;
        velocity_622.body = "Ordinary issue mentioning #619 and mESmaC/rosen#449.".into();

        let mut same_host_ctx = TextBlockerContext::default();
        same_host_ctx.insert_open_numbers("mESmaC/velocity", [618, 619, 620, 621, 622]);
        same_host_ctx.insert_open_numbers("mESmaC/rosen", [449]);
        same_host_ctx.insert_comments(
            621,
            vec![
                "After #620 lands we can simplify this.".into(),
                "Velocity agent `issue-621-1` · [Thread 1](velocity://thread/2/1)\n\nAfter #618 we saw nothing.".into(),
            ],
        );

        // Bare `#619` and cross-repo mentions without an explicit phrase never block dispatch.
        assert!(dispatchable_issue_with_blockers(
            &projects,
            &velocity,
            "mESmaC/velocity",
            &velocity_622,
            &claimed,
            &same_host_ctx,
        ));
        assert!(
            same_host_text_blockers_to_link(
                &projects,
                &velocity,
                "mESmaC/velocity",
                &velocity_622,
                &same_host_ctx,
            )
            .is_empty()
        );

        // velocity#621 is blocked by #619 (body) and #620 (owner comment), NOT #618 (agent comment / bare).
        assert!(!dispatchable_issue_with_blockers(
            &projects,
            &velocity,
            "mESmaC/velocity",
            &velocity_621,
            &claimed,
            &same_host_ctx,
        ));
        let skipped_same = issue_skip_reason_with_blockers(
            &projects,
            &velocity,
            "mESmaC/velocity",
            &velocity_621,
            &claimed,
            0,
            &same_host_ctx,
        )
        .ok_or("expected velocity#621 to be skipped")?;
        assert_eq!(skipped_same.category, SkipCategory::Blocker);
        assert_eq!(skipped_same.reason, "blocked by #619, #620");

        let to_link: Vec<String> = same_host_text_blockers_to_link(
            &projects,
            &velocity,
            "mESmaC/velocity",
            &velocity_621,
            &same_host_ctx,
        )
        .into_iter()
        .map(|r| r.to_string())
        .collect();
        assert_eq!(to_link, vec!["#619", "#620"]);

        // Cross-repo same-host (rosen on Collapse -> velocity on Collapse) is also linkable.
        let mut rosen_450 = sample_issue(450);
        rosen_450.host = IssueHost::Collapse;
        rosen_450.body = "Blocked by mESmaC/velocity#619.".into();
        let rosen_links: Vec<String> = same_host_text_blockers_to_link(
            &projects,
            &rosen,
            "mESmaC/rosen",
            &rosen_450,
            &same_host_ctx,
        )
        .into_iter()
        .map(|r| r.to_string())
        .collect();
        assert_eq!(rosen_links, vec!["mESmaC/velocity#619"]);

        // Once #619 and #620 close on Collapse, velocity#621 dispatches.
        same_host_ctx.insert_open_numbers("mESmaC/velocity", [618, 621, 622]);
        assert!(dispatchable_issue_with_blockers(
            &projects,
            &velocity,
            "mESmaC/velocity",
            &velocity_621,
            &claimed,
            &same_host_ctx,
        ));

        // --- Cross-host case: collapse.sh#225 (GitHub) waits for velocity#619 & rosen#449 (Collapse) ---
        let mut collapse_225 = sample_issue(225);
        collapse_225.host = IssueHost::GitHub;
        collapse_225.body =
            "Waits for mESmaC/velocity#619 and mESmaC/rosen#449.\n\nDetails.".into();

        // Referenced sibling project IDs include both velocity and rosen.
        assert_eq!(
            referenced_blocker_project_ids(
                &projects,
                &collapse_sh,
                "mESmaC/collapse.sh",
                std::slice::from_ref(&collapse_225),
                &BTreeMap::new(),
            ),
            vec![2, 3]
        );

        // Before sibling backlogs load, dispatch fails closed instead of starting prematurely.
        let mut cross_host_ctx = TextBlockerContext::default();
        cross_host_ctx.insert_open_numbers("mESmaC/collapse.sh", [225]);
        assert!(!dispatchable_issue_with_blockers(
            &projects,
            &collapse_sh,
            "mESmaC/collapse.sh",
            &collapse_225,
            &claimed,
            &cross_host_ctx,
        ));

        // With both Collapse sibling issues open, collapse.sh#225 is blocked and NOT auto-linked on GitHub.
        cross_host_ctx.insert_open_numbers("mESmaC/velocity", [619]);
        cross_host_ctx.insert_open_numbers("mESmaC/rosen", [449]);
        assert!(!dispatchable_issue_with_blockers(
            &projects,
            &collapse_sh,
            "mESmaC/collapse.sh",
            &collapse_225,
            &claimed,
            &cross_host_ctx,
        ));
        let skipped_cross = issue_skip_reason_with_blockers(
            &projects,
            &collapse_sh,
            "mESmaC/collapse.sh",
            &collapse_225,
            &claimed,
            0,
            &cross_host_ctx,
        )
        .ok_or("expected collapse.sh#225 to be skipped")?;
        assert_eq!(skipped_cross.category, SkipCategory::Blocker);
        assert_eq!(
            skipped_cross.reason,
            "blocked by mESmaC/velocity#619, mESmaC/rosen#449"
        );
        assert!(
            same_host_text_blockers_to_link(
                &projects,
                &collapse_sh,
                "mESmaC/collapse.sh",
                &collapse_225,
                &cross_host_ctx,
            )
            .is_empty()
        );

        // Closing only velocity#619 keeps collapse.sh#225 waiting on rosen#449.
        cross_host_ctx.insert_open_numbers("mESmaC/velocity", []);
        assert!(!dispatchable_issue_with_blockers(
            &projects,
            &collapse_sh,
            "mESmaC/collapse.sh",
            &collapse_225,
            &claimed,
            &cross_host_ctx,
        ));

        // Once rosen#449 also closes, collapse.sh#225 becomes dispatchable.
        cross_host_ctx.insert_open_numbers("mESmaC/rosen", []);
        assert!(dispatchable_issue_with_blockers(
            &projects,
            &collapse_sh,
            "mESmaC/collapse.sh",
            &collapse_225,
            &claimed,
            &cross_host_ctx,
        ));
        assert!(
            issue_skip_reason_with_blockers(
                &projects,
                &collapse_sh,
                "mESmaC/collapse.sh",
                &collapse_225,
                &claimed,
                0,
                &cross_host_ctx,
            )
            .is_none()
        );

        // Prior attempt on collapse.sh#225 also clears once cross-host blockers close.
        let mut attempted_collapse = collapse_sh.clone();
        attempted_collapse.continuous_dispatch.attempts.push(
            velocity_store::continuous_dispatch::DispatchAttempt {
                repository: "mESmaC/collapse.sh".into(),
                number: 225,
                updated_at: collapse_225.updated_at.clone(),
                attempted_at: Some(1_000),
                open_referenced_blockers: vec![
                    "mESmaC/velocity#619".into(),
                    "mESmaC/rosen#449".into(),
                ],
                thread_id: None,
                last_result: None,
            },
        );
        let reconcile = attempts_to_clear_with_context(
            &projects,
            &attempted_collapse,
            "mESmaC/collapse.sh",
            &[collapse_225],
            &BTreeMap::new(),
            None,
            1_200,
            &cross_host_ctx,
        );
        assert_eq!(
            reconcile.clear,
            vec![(
                225,
                AttemptClearReason::BlockerClosed("mESmaC/velocity#619, mESmaC/rosen#449".into())
            )]
        );
        Ok(())
    }
}
