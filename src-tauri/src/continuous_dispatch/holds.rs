//! Automated maintenance for machine-checkable issue holds (#526):
//! - `tracking` issues close automatically once every checklist item is ticked and no open
//!   GitHub blockers remain.
//! - `blocked-upstream` issues are checked daily against their recorded upstream PR or
//!   release, and return to dispatch automatically when it ships.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use velocity_core::issue_holds::{
    BLOCKED_UPSTREAM_LABEL, HOLD_LABEL, TRACKING_LABEL, has_label, is_legacy_tracking_issue,
    is_retired_hold_label, legacy_upstream_target, parse_checklist,
};
use velocity_github::{GitHubIssue, IssueClient, IssueState, UpstreamStatus};

use super::Reader;
use crate::threads::report;

/// How often dispatch re-checks an upstream PR or release for a `blocked-upstream` issue.
const UPSTREAM_CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
/// Backoff when a GitHub check or close fails so one broken issue never stalls dispatch.
const ERROR_RETRY_INTERVAL: Duration = Duration::from_secs(60 * 60);

struct UpstreamCheck {
    updated_at: String,
    checked_at: Instant,
    failed: bool,
}

#[derive(Default)]
pub(super) struct Holds {
    upstream: BTreeMap<u32, UpstreamCheck>,
    tracking_retry: BTreeMap<u32, Instant>,
    /// Legacy retired-hold issues already checked at `updated_at` for `tracking` / `blocked-upstream` (#617).
    legacy_checked: BTreeMap<u32, (String, Option<Instant>)>,
}

impl Holds {
    pub(super) fn new() -> Self {
        Self::default()
    }

    fn upstream_due(&self, issue: &GitHubIssue) -> bool {
        let Some(previous) = self.upstream.get(&issue.number) else {
            return true;
        };
        if previous.updated_at != issue.updated_at {
            return true;
        }
        let interval = if previous.failed {
            ERROR_RETRY_INTERVAL
        } else {
            UPSTREAM_CHECK_INTERVAL
        };
        previous.checked_at.elapsed() >= interval
    }

    fn legacy_migration_due(&self, issue: &GitHubIssue) -> bool {
        match self.legacy_checked.get(&issue.number) {
            None => true,
            Some((updated_at, _)) if updated_at != &issue.updated_at => true,
            Some((_, Some(failed_at))) => failed_at.elapsed() >= ERROR_RETRY_INTERVAL,
            Some((_, None)) => false,
        }
    }
}

pub(super) fn refresh(reader: &mut Reader) {
    if reader.backlog.error.is_some() {
        return;
    }
    let Some(client) = reader.client.as_ref() else {
        return;
    };
    let issues = reader.backlog.issues.clone();
    let changed_legacy = migrate_legacy_holds(client, &mut reader.holds, &issues);
    let changed_tracking = check_tracking(client, &mut reader.holds, &issues);
    let changed_upstream = check_upstream(client, &mut reader.holds, &issues);
    if changed_legacy || changed_tracking || changed_upstream {
        reader.refreshed = None;
    }
}

/// Automatically relabels legacy `needs-owner` issues that are clearly `tracking` checklist
/// issues or `blocked-upstream` issues so project health classifies them accurately (#617).
fn migrate_legacy_holds(client: &IssueClient, holds: &mut Holds, issues: &[GitHubIssue]) -> bool {
    let candidates: Vec<&GitHubIssue> = issues
        .iter()
        .filter(|issue| {
            issue.state == IssueState::Open
                && !has_label(&issue.labels, TRACKING_LABEL)
                && !has_label(&issue.labels, BLOCKED_UPSTREAM_LABEL)
                && !has_label(&issue.labels, HOLD_LABEL)
                && issue
                    .labels
                    .iter()
                    .any(|label| is_retired_hold_label(label))
        })
        .collect();
    holds
        .legacy_checked
        .retain(|number, _| candidates.iter().any(|issue| issue.number == *number));
    let mut changed = false;
    for issue in candidates {
        if !holds.legacy_migration_due(issue) {
            continue;
        }
        match migrate_one_legacy_hold(client, issue) {
            Ok(true) => {
                holds.legacy_checked.remove(&issue.number);
                changed = true;
            }
            Ok(false) => {
                holds
                    .legacy_checked
                    .insert(issue.number, (issue.updated_at.clone(), None));
            }
            Err(error) => {
                holds.legacy_checked.insert(
                    issue.number,
                    (issue.updated_at.clone(), Some(Instant::now())),
                );
                report::<()>(Err(format!(
                    "Could not migrate legacy hold on {}#{}: {error}",
                    client.repository(),
                    issue.number
                )));
            }
        }
    }
    changed
}

fn migrate_one_legacy_hold(
    client: &IssueClient,
    issue: &GitHubIssue,
) -> Result<bool, velocity_github::GitHubCommandError> {
    if is_legacy_tracking_issue(&issue.title, &issue.body, &issue.labels) {
        return client.migrate_tracking_hold(issue.number);
    }
    let comments = client.read_comment_bodies(issue.number)?;
    if let Some(target) = legacy_upstream_target(
        &issue.body,
        &comments,
        &issue.labels,
        Some(client.repository()),
    ) {
        return client.migrate_upstream_hold(issue.number, &target);
    }
    Ok(false)
}

fn check_tracking(client: &IssueClient, holds: &mut Holds, issues: &[GitHubIssue]) -> bool {
    let tracking: Vec<&GitHubIssue> = issues
        .iter()
        .filter(|issue| {
            issue.state == IssueState::Open
                && has_label(&issue.labels, TRACKING_LABEL)
                && !issue.has_open_blockers()
                && parse_checklist(&issue.body).is_complete()
        })
        .collect();
    holds
        .tracking_retry
        .retain(|number, _| tracking.iter().any(|issue| issue.number == *number));
    let mut changed = false;
    for issue in tracking {
        if holds
            .tracking_retry
            .get(&issue.number)
            .is_some_and(|failed| failed.elapsed() < ERROR_RETRY_INTERVAL)
        {
            continue;
        }
        match client.close_completed_tracking_issue(issue.number) {
            Ok(closed) => {
                holds.tracking_retry.remove(&issue.number);
                changed |= closed;
            }
            Err(error) => {
                holds.tracking_retry.insert(issue.number, Instant::now());
                report::<()>(Err(format!(
                    "Could not close completed tracking issue {}#{}: {error}",
                    client.repository(),
                    issue.number
                )));
            }
        }
    }
    changed
}

fn check_upstream(client: &IssueClient, holds: &mut Holds, issues: &[GitHubIssue]) -> bool {
    let waiting: Vec<&GitHubIssue> = issues
        .iter()
        .filter(|issue| {
            issue.state == IssueState::Open && has_label(&issue.labels, BLOCKED_UPSTREAM_LABEL)
        })
        .collect();
    holds
        .upstream
        .retain(|number, _| waiting.iter().any(|issue| issue.number == *number));
    let mut changed = false;
    for issue in waiting {
        if !holds.upstream_due(issue) {
            continue;
        }
        match check_one_upstream(client, issue) {
            Ok(true) => {
                holds.upstream.remove(&issue.number);
                changed = true;
            }
            Ok(false) => {
                holds.upstream.insert(
                    issue.number,
                    UpstreamCheck {
                        updated_at: issue.updated_at.clone(),
                        checked_at: Instant::now(),
                        failed: false,
                    },
                );
            }
            Err(error) => {
                holds.upstream.insert(
                    issue.number,
                    UpstreamCheck {
                        updated_at: issue.updated_at.clone(),
                        checked_at: Instant::now(),
                        failed: true,
                    },
                );
                report::<()>(Err(format!(
                    "Could not check upstream wait for {}#{}: {error}",
                    client.repository(),
                    issue.number
                )));
            }
        }
    }
    changed
}

fn check_one_upstream(
    client: &IssueClient,
    issue: &GitHubIssue,
) -> Result<bool, velocity_github::GitHubCommandError> {
    let Some(target) = client.find_upstream_target(issue)? else {
        return Ok(false);
    };
    match client.check_upstream_target(&target)? {
        UpstreamStatus::Waiting => Ok(false),
        UpstreamStatus::Shipped { summary } => {
            client.unblock_upstream_issue(issue.number, &target, &summary)
        }
    }
}
