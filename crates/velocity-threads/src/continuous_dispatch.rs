//! Continuous issue selection and the Duty projection (F8). No UI owns scheduling state.
use serde::{Deserialize, Serialize};
use specta::Type;
use velocity_github::{GitHubIssue, IssueState};
use velocity_store::issues::{ThreadIssue, issue_thread};
use velocity_store::{ProjectItem, VersionControl, continuous_dispatch::busy_count};

#[derive(Clone, Debug, Default, Serialize, Deserialize, Type)]
pub struct DispatchView {
    pub project_identifier: Option<u32>,
    pub available: bool,
    pub slots: u32,
    pub busy: u32,
    pub paused: bool,
    pub next: Vec<DispatchIssue>,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct DispatchIssue {
    pub number: u32,
    pub title: String,
}

impl DispatchView {
    pub fn new(project: &ProjectItem, issues: &[GitHubIssue], error: Option<String>) -> Self {
        Self {
            project_identifier: Some(project.id),
            available: project.version_control == VersionControl::Rosen,
            slots: project.continuous_dispatch.slots,
            busy: busy_count(project),
            paused: project.continuous_dispatch.paused,
            next: issues
                .iter()
                .take(5)
                .map(|issue| DispatchIssue {
                    number: issue.number,
                    title: issue.title.clone(),
                })
                .collect(),
            error,
        }
    }
}

/// Shared automatic-work eligibility for continuous dispatch and the F16 work pool.
/// Owner questions wait for an answer even before somebody labels them (#356, #389).
pub fn actionable(issue: &GitHubIssue) -> bool {
    if issue.state != IssueState::Open
        || issue.is_spec_question()
        || issue.labels.iter().any(|label| {
            [
                "needs-decision",
                "needs-owner",
                "release-blocker",
                "owner-decision",
            ]
            .iter()
            .any(|skip| label.eq_ignore_ascii_case(skip))
        })
    {
        return false;
    }
    let text = format!("{}\n{}", issue.title, issue.body).to_lowercase();
    ![
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
    claimed: &[u32],
) -> Vec<GitHubIssue> {
    let mut ready: Vec<_> = issues
        .iter()
        .filter(|issue| {
            actionable(issue)
                && !claimed.contains(&issue.number)
                && issue_thread(project, &ThreadIssue::new(repository.into(), issue.number))
                    .is_none()
                && !project.continuous_dispatch.attempted(
                    repository,
                    issue.number,
                    &issue.updated_at,
                )
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
