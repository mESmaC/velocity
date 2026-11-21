//! GitHub operations for machine-checkable issue holds (#526):
//! - `tracking` issues close automatically when their checklist finishes.
//! - `blocked-upstream` issues record a PR or release target, checked daily and unblocked
//!   automatically when it ships.
//! - Visible person holds (`hold`, plus retired hold labels until released) record what
//!   releases them on the issue body so the Work view always shows them with a Release action.
//! - Orphaned spec issues close as `not_planned` citing the commit that removed the section.

use serde::Deserialize;
use serde_json::{Value, json};
use velocity_core::issue_dependencies::IssueReference;
use velocity_core::issue_holds::{
    BLOCKED_UPSTREAM_LABEL, HOLD_LABEL, TRACKING_LABEL, UpstreamTarget, has_label,
    is_retired_hold_label, parse_checklist, parse_upstream_target, strip_person_hold_block,
    strip_upstream_block, upsert_person_hold_block, upsert_upstream_block, validate_commit_ref,
    validate_person_hold,
};

use crate::issue_dependencies::waits_line;
use crate::{GitHubCommandError, GitHubIssue, IssueClient, IssueState};

/// Outcome of checking an upstream PR or release for a `blocked-upstream` issue (#526).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpstreamStatus {
    Waiting,
    Shipped { summary: String },
}

#[derive(Deserialize)]
struct RestIssue {
    state: String,
    #[serde(default)]
    state_reason: Option<String>,
    #[serde(default)]
    pull_request: Option<Value>,
}

#[derive(Deserialize)]
struct RestPullRequest {
    #[serde(default)]
    merged: bool,
    #[serde(default)]
    merged_at: Option<String>,
}

#[derive(Deserialize)]
struct RestRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    published_at: Option<String>,
}

impl IssueClient {
    /// Finds the upstream PR or release target recorded in an issue's body or comments.
    pub fn find_upstream_target(
        &self,
        issue: &GitHubIssue,
    ) -> Result<Option<UpstreamTarget>, GitHubCommandError> {
        if let Some(target) = parse_upstream_target(&issue.body, Some(self.repository())) {
            return Ok(Some(target));
        }
        let comments = self.issue_comment_bodies(issue.number)?;
        for body in comments.iter().rev() {
            if let Some(target) = parse_upstream_target(body, Some(self.repository())) {
                return Ok(Some(target));
            }
        }
        Ok(None)
    }

    /// Checks via GitHub REST API whether an upstream PR or release has shipped (#526).
    pub fn check_upstream_target(
        &self,
        target: &UpstreamTarget,
    ) -> Result<UpstreamStatus, GitHubCommandError> {
        match target {
            UpstreamTarget::PullRequest { repository, number } => {
                self.check_upstream_pr_or_issue(repository, *number)
            }
            UpstreamTarget::Release { repository, tag } => {
                self.check_upstream_release_tag(repository, tag)
            }
            UpstreamTarget::PullRequestRelease {
                repository,
                number,
                release_repository,
            } => self.check_upstream_pr_release(repository, *number, release_repository),
        }
    }

    fn check_upstream_pr_or_issue(
        &self,
        repository: &str,
        number: u32,
    ) -> Result<UpstreamStatus, GitHubCommandError> {
        let endpoint = format!("repos/{repository}/issues/{number}");
        let issue: RestIssue = self.transport.command.read(&["api", &endpoint])?;
        if issue.pull_request.is_some() {
            let pull = self.read_upstream_pull(repository, number)?;
            if pull.merged {
                let merged_at = pull
                    .merged_at
                    .as_deref()
                    .map(|time| format!(" ({time})"))
                    .unwrap_or_default();
                return Ok(UpstreamStatus::Shipped {
                    summary: format!("PR `{repository}#{number}` merged{merged_at}"),
                });
            }
            return Ok(UpstreamStatus::Waiting);
        }
        if issue.state.eq_ignore_ascii_case("closed") {
            let reason = issue
                .state_reason
                .as_deref()
                .map(|reason| format!(" ({reason})"))
                .unwrap_or_default();
            return Ok(UpstreamStatus::Shipped {
                summary: format!("Upstream issue `{repository}#{number}` closed{reason}"),
            });
        }
        Ok(UpstreamStatus::Waiting)
    }

    fn check_upstream_release_tag(
        &self,
        repository: &str,
        tag: &str,
    ) -> Result<UpstreamStatus, GitHubCommandError> {
        let candidates = release_tag_candidates(tag);
        for candidate in candidates {
            let endpoint = format!("repos/{repository}/releases/tags/{candidate}");
            match self
                .transport
                .command
                .read::<RestRelease>(&["api", &endpoint])
            {
                Ok(release) if !release.draft => {
                    return Ok(UpstreamStatus::Shipped {
                        summary: format!(
                            "Release `{}` in `{repository}` is published",
                            release.tag_name
                        ),
                    });
                }
                Ok(_) => return Ok(UpstreamStatus::Waiting),
                Err(error) if is_not_found(&error) => continue,
                Err(error) => return Err(error),
            }
        }
        Ok(UpstreamStatus::Waiting)
    }

    fn check_upstream_pr_release(
        &self,
        repository: &str,
        number: u32,
        release_repository: &str,
    ) -> Result<UpstreamStatus, GitHubCommandError> {
        let pull = self.read_upstream_pull(repository, number)?;
        let Some(merged_at) = pull.merged_at.filter(|_| pull.merged) else {
            return Ok(UpstreamStatus::Waiting);
        };
        let endpoint = format!("repos/{release_repository}/releases?per_page=20");
        let releases: Vec<RestRelease> = self.transport.command.read(&["api", &endpoint])?;
        if let Some(release) = releases.into_iter().find(|release| {
            !release.draft
                && release
                    .published_at
                    .as_deref()
                    .is_some_and(|published| published >= merged_at.as_str())
        }) {
            return Ok(UpstreamStatus::Shipped {
                summary: format!(
                    "PR `{repository}#{number}` merged and `{release_repository}` release `{}` published",
                    release.tag_name
                ),
            });
        }
        Ok(UpstreamStatus::Waiting)
    }

    fn read_upstream_pull(
        &self,
        repository: &str,
        number: u32,
    ) -> Result<RestPullRequest, GitHubCommandError> {
        let endpoint = format!("repos/{repository}/pulls/{number}");
        self.transport.command.read(&["api", &endpoint])
    }

    /// Removes `blocked-upstream` and posts evidence when the upstream target ships (#526).
    pub fn unblock_upstream_issue(
        &self,
        number: u32,
        target: &UpstreamTarget,
        summary: &str,
    ) -> Result<bool, GitHubCommandError> {
        let issue = self.read_issue(number)?;
        if issue.state != IssueState::Open || !has_label(&issue.labels, BLOCKED_UPSTREAM_LABEL) {
            return Ok(false);
        }
        let marker = format!(
            "<!-- velocity-upstream-unblocked:{} -->",
            target.canonical()
        );
        if !self.has_rosen_comment(number, &marker)? {
            let body = format!(
                "Velocity continuous dispatch · Upstream dependency shipped: {summary}. \
                 Removing `{BLOCKED_UPSTREAM_LABEL}` and returning this issue to dispatch.\n\n{marker}"
            );
            self.add_issue_comment(&issue.id, &body)?;
        }
        self.remove_present_labels(&issue, &[BLOCKED_UPSTREAM_LABEL])?;
        self.remove_retired_hold_labels(&issue)?;
        let cleaned = strip_upstream_block(&issue.body);
        if cleaned != issue.body {
            self.update_issue_body(&issue.id, &cleaned)?;
        }
        crate::issue_cache::invalidate(self.repository());
        Ok(true)
    }

    /// Closes a `tracking` issue automatically once every checklist item is ticked and
    /// no open GitHub blockers remain (#526).
    pub fn close_completed_tracking_issue(&self, number: u32) -> Result<bool, GitHubCommandError> {
        let issue = self.read_issue(number)?;
        if issue.state != IssueState::Open
            || !has_label(&issue.labels, TRACKING_LABEL)
            || issue.has_open_blockers()
        {
            return Ok(false);
        }
        let checklist = parse_checklist(&issue.body);
        if !checklist.is_complete() {
            return Ok(false);
        }
        let marker = "<!-- velocity-tracking-complete -->";
        if !self.has_rosen_comment(number, marker)? {
            let body = format!(
                "Velocity continuous dispatch · All {} checklist items on this tracking issue are complete. Closing.\n\n{marker}",
                checklist.checked
            );
            self.add_issue_comment(&issue.id, &body)?;
        }
        self.close_issue_with_reason(number, "completed")?;
        crate::issue_cache::invalidate(self.repository());
        Ok(true)
    }

    /// Marks an issue `blocked-upstream` with a machine-checkable upstream PR or release (#526).
    pub fn post_blocked_upstream(
        &self,
        number: u32,
        byline: &str,
        target: &UpstreamTarget,
        reason: &str,
        blockers: &[IssueReference],
    ) -> Result<String, GitHubCommandError> {
        let issue = self.read_issue(number)?;
        let waits = waits_line(&self.link_blockers(number, blockers)?);
        let updated_body = upsert_upstream_block(&issue.body, target);
        if updated_body != issue.body {
            self.update_issue_body(&issue.id, &updated_body)?;
        }
        self.label_issue(number, BLOCKED_UPSTREAM_LABEL)?;
        self.remove_present_labels(&issue, &[Self::NO_CHANGE_LABEL, HOLD_LABEL])?;
        self.remove_retired_hold_labels(&issue)?;
        let comment = format!(
            "Blocked upstream · {byline}\n\nWaits for `{}` (checked daily by Velocity).\n\n{}{waits}\n\n{}",
            target.display_summary(),
            reason.trim(),
            target.marker()
        );
        let url = self.add_issue_comment(&issue.id, &comment)?;
        crate::issue_cache::invalidate(self.repository());
        Ok(url)
    }

    /// Records a visible person hold on `number` with its release action (#526).
    /// Shown as one line on the project's Work view with a Release action.
    pub fn post_person_hold(
        &self,
        number: u32,
        byline: &str,
        reason: &str,
        release_action: &str,
        blockers: &[IssueReference],
    ) -> Result<String, GitHubCommandError> {
        let hold =
            validate_person_hold(reason, release_action).map_err(GitHubCommandError::Invalid)?;
        let issue = self.read_issue(number)?;
        let waits = waits_line(&self.link_blockers(number, blockers)?);
        let updated_body = upsert_person_hold_block(&issue.body, &hold);
        if updated_body != issue.body {
            self.update_issue_body(&issue.id, &updated_body)?;
        }
        self.label_issue(number, HOLD_LABEL)?;
        self.remove_present_labels(&issue, &[Self::NO_CHANGE_LABEL])?;
        self.remove_retired_hold_labels(&issue)?;
        let comment = format!(
            "On hold · {byline}\n\n**Hold:** {}\n**Release when:** {}{waits}\n\n{}",
            hold.reason,
            hold.release_action,
            hold.marker()
        );
        let url = self.add_issue_comment(&issue.id, &comment)?;
        crate::issue_cache::invalidate(self.repository());
        Ok(url)
    }

    /// Releases a person hold (including any legacy retired hold label) from the Work view (#526).
    pub fn release_person_hold(&self, number: u32) -> Result<(), GitHubCommandError> {
        let issue = self.read_issue(number)?;
        self.remove_present_labels(&issue, &[HOLD_LABEL, BLOCKED_UPSTREAM_LABEL])?;
        self.remove_retired_hold_labels(&issue)?;
        let cleaned = strip_person_hold_block(&issue.body);
        if cleaned != issue.body {
            self.update_issue_body(&issue.id, &cleaned)?;
        }
        self.add_issue_comment(
            &issue.id,
            "Hold released from Velocity's Work view. Returning this issue to dispatch.\n\n<!-- velocity-hold-released -->",
        )?;
        crate::issue_cache::invalidate(self.repository());
        crate::issue_cache::invalidate_discussion(self.repository(), number);
        Ok(())
    }

    /// Closes an issue as `not_planned` when its referenced spec section was removed (#526).
    pub fn close_not_planned(
        &self,
        number: u32,
        byline: &str,
        spec_section: &str,
        removed_commit: &str,
        reason: &str,
    ) -> Result<String, GitHubCommandError> {
        let section = spec_section.trim();
        if section.is_empty() {
            return Err(GitHubCommandError::Invalid(
                "Name the removed spec section in spec_section".into(),
            ));
        }
        let commit = validate_commit_ref(removed_commit).map_err(GitHubCommandError::Invalid)?;
        let reason = reason.trim();
        if reason.is_empty() {
            return Err(GitHubCommandError::Invalid(
                "Explain why the spec section is gone".into(),
            ));
        }
        let issue = self.read_issue(number)?;
        let comment = format!(
            "Closed as not planned · {byline}\n\nSpec section `{section}` was removed in commit `{commit}`.\n\n{reason}\n\n<!-- velocity-spec-orphaned:{commit} -->"
        );
        let url = self.add_issue_comment(&issue.id, &comment)?;
        self.remove_present_labels(&issue, &[Self::NO_CHANGE_LABEL, HOLD_LABEL])?;
        self.remove_retired_hold_labels(&issue)?;
        self.close_issue_with_reason(number, "not_planned")?;
        crate::issue_cache::invalidate(self.repository());
        Ok(url)
    }

    pub(crate) fn update_issue_body(
        &self,
        issue_id: &str,
        body: &str,
    ) -> Result<(), GitHubCommandError> {
        let _: Value = self.transport.graphql_at(
            "mutation($input:UpdateIssueInput!) { updateIssue(input:$input) { issue { id } } }",
            json!({"input": {"id": issue_id, "body": body}}),
            "/data/updateIssue/issue",
        )?;
        Ok(())
    }

    fn close_issue_with_reason(&self, number: u32, reason: &str) -> Result<(), GitHubCommandError> {
        let endpoint = format!("repos/{}/issues/{number}", self.repository());
        let _: Value = self.transport.command.api(
            "PATCH",
            &endpoint,
            &json!({"state": "closed", "state_reason": reason}),
        )?;
        Ok(())
    }

    fn remove_present_labels(
        &self,
        issue: &GitHubIssue,
        names: &[&str],
    ) -> Result<(), GitHubCommandError> {
        for name in names {
            if let Some(existing) = issue
                .labels
                .iter()
                .find(|label| label.eq_ignore_ascii_case(name))
            {
                self.remove_issue_label(issue.number, existing)?;
            }
        }
        Ok(())
    }

    fn remove_retired_hold_labels(&self, issue: &GitHubIssue) -> Result<(), GitHubCommandError> {
        for existing in issue
            .labels
            .iter()
            .filter(|label| is_retired_hold_label(label))
        {
            self.remove_issue_label(issue.number, existing)?;
        }
        Ok(())
    }
}

fn release_tag_candidates(tag: &str) -> Vec<String> {
    let mut tags = vec![tag.to_owned()];
    if let Some(stripped) = tag.strip_prefix('v') {
        tags.push(stripped.to_owned());
    } else if tag.chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        tags.push(format!("v{tag}"));
    }
    tags
}

fn is_not_found(error: &GitHubCommandError) -> bool {
    matches!(error, GitHubCommandError::Command { message, .. } if message.contains("HTTP 404") || message.contains("Not Found"))
}
