//! The F9 rail and spec-question projection. Refreshing never opens a browser.

use serde::{Deserialize, Serialize};
use specta::Type;
use velocity_core::issue_holds::{
    BLOCKED_UPSTREAM_LABEL, TRACKING_LABEL, has_label, parse_checklist, parse_person_hold,
    parse_upstream_target,
};

use crate::issues::IssueBackend;
use crate::repository_client::Connection;
use crate::{GitHubCommandError, GitHubIssue, IssueClient};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct SpecQuestion {
    pub recommendation: String,
    pub blocked_items: u32,
}

impl SpecQuestion {
    pub fn from_body(body: &str) -> Self {
        let mut recommendation = Vec::new();
        let mut collecting = false;
        let mut blocks = false;
        let mut blocked_items = 0_u32;
        for line in body.lines() {
            let plain = line.trim().replace("**", "");
            let lower = plain.to_ascii_lowercase();
            if let Some((label, answer)) = plain.split_once(':')
                && matches!(
                    label.to_ascii_lowercase().as_str(),
                    "recommended answer" | "recommendation" | "recommended"
                )
            {
                collecting = true;
                recommendation.push(answer.trim().to_owned());
            } else if collecting && (plain.is_empty() || plain.starts_with('#')) {
                collecting = false;
            } else if collecting {
                recommendation.push(plain.clone());
            }
            if lower.starts_with("blocks:") || lower.starts_with("blocked items:") {
                blocks = true;
            } else if blocks && (plain.starts_with("- ") || plain.starts_with("* ")) {
                blocked_items = blocked_items.saturating_add(1);
            } else if blocks && !plain.is_empty() {
                blocks = false;
            }
        }
        Self {
            recommendation: recommendation.join(" ").trim().to_owned(),
            blocked_items,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct IssuePanelRow {
    pub number: u32,
    pub title: String,
    pub labels: Vec<String>,
    pub scout: Option<String>,
    pub question: Option<SpecQuestion>,
    /// The body's opening paragraph, shown when the row is expanded (#209).
    #[serde(default)]
    pub summary: String,
    pub blocked_by: Option<String>,
    /// Open issues that wait on this one, directly or through others (#510).
    /// Continuous dispatch starts the highest count first.
    #[serde(default)]
    pub unblocks: u32,
    pub thread_identifier: Option<u32>,
    pub thread_label: Option<String>,
    /// Why continuous dispatch holds this issue instead of starting it (#525).
    #[serde(default)]
    pub hold: Option<IssueHold>,
    /// A visible hold needing a person (hardware, scope hold, or retired hold label), shown
    /// as one line on the Work view with its release action (#526).
    #[serde(default)]
    pub person_hold: Option<VisibleHold>,
    /// Upstream PR or release waited on for a `blocked-upstream` issue, checked daily (#526).
    #[serde(default)]
    pub upstream_wait: Option<String>,
    /// Checklist progress for a `tracking` issue that closes automatically when complete (#526).
    #[serde(default)]
    pub tracking: Option<TrackingProgress>,
}

/// A visible person hold shown on the project's Work view with the action that releases it (#526).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct VisibleHold {
    pub reason: String,
    pub release_action: String,
}

/// Checklist progress on a `tracking` issue (#526).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct TrackingProgress {
    pub checked: u32,
    pub total: u32,
}

/// An overlap hold as the Work card shows it: the state, the reason, and the files the
/// blocker's open change has edited that the held issue also names.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct IssueHold {
    pub label: String,
    pub reason: String,
    pub paths: Vec<String>,
}

impl IssueHold {
    pub fn new(label: String, reason: String, paths: Vec<String>) -> Self {
        Self {
            label,
            reason,
            paths,
        }
    }
}

impl IssuePanelRow {
    pub fn from_issue(issue: GitHubIssue) -> Self {
        Self::from_issue_for_repository(None, issue)
    }

    pub fn from_issue_for_repository(repository: Option<&str>, issue: GitHubIssue) -> Self {
        let question = issue
            .labels
            .iter()
            .any(|label| label.eq_ignore_ascii_case("spec-question"))
            .then(|| SpecQuestion::from_body(&issue.body));
        let blocked_by = if !issue.open_blockers.is_empty() {
            Some(issue.open_blockers.join(", "))
        } else if issue.open_blocker_count == 1 {
            Some("1 issue".into())
        } else if issue.open_blocker_count > 1 {
            Some(format!("{} issues", issue.open_blocker_count))
        } else {
            None
        };
        let (person_hold, upstream_wait, tracking) = classify_holds(repository, &issue);
        Self {
            number: issue.number,
            title: issue.title,
            labels: issue.labels,
            scout: issue.scout,
            question,
            summary: body_summary(&issue.body),
            blocked_by,
            unblocks: 0,
            thread_identifier: None,
            thread_label: None,
            hold: None,
            person_hold,
            upstream_wait,
            tracking,
        }
    }
}

fn classify_holds(
    repository: Option<&str>,
    issue: &GitHubIssue,
) -> (
    Option<VisibleHold>,
    Option<String>,
    Option<TrackingProgress>,
) {
    let tracking = has_label(&issue.labels, TRACKING_LABEL).then(|| {
        let progress = parse_checklist(&issue.body);
        TrackingProgress {
            checked: progress.checked,
            total: progress.total(),
        }
    });
    let mut person_hold = parse_person_hold(&issue.body, &issue.labels).map(|hold| VisibleHold {
        reason: hold.reason,
        release_action: hold.release_action,
    });
    let upstream_wait = if has_label(&issue.labels, BLOCKED_UPSTREAM_LABEL) {
        match parse_upstream_target(&issue.body, repository) {
            Some(target) => Some(target.display_summary()),
            None => {
                // Never hide a `blocked-upstream` issue whose target is missing (#526).
                person_hold.get_or_insert_with(|| VisibleHold {
                    reason: "Blocked upstream with no PR or release reference".into(),
                    release_action: "Add `Blocked upstream: owner/repo#n` or release hold".into(),
                });
                None
            }
        }
    } else {
        None
    };
    (person_hold, upstream_wait, tracking)
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct IssuePanelSnapshot {
    pub revision: u32,
    pub project_identifier: Option<u32>,
    pub repository: Option<String>,
    pub loading: bool,
    pub rows: Vec<IssuePanelRow>,
    pub error: Option<String>,
    /// Issue numbers whose rows show their details. Every row starts collapsed so a long
    /// list stays scrollable (#209); the set is per project and survives refreshes.
    #[serde(default)]
    pub expanded: Vec<u32>,
}

impl IssuePanelSnapshot {
    pub fn select(&mut self, project: Option<u32>) {
        if self.project_identifier != project {
            self.project_identifier = project;
            self.repository = None;
            self.rows.clear();
            self.expanded.clear();
            self.error = None;
            self.loading = project.is_some();
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn update(&mut self, result: Result<(String, Vec<IssuePanelRow>), String>) {
        self.loading = false;
        match result {
            Ok((repository, rows)) => {
                self.repository = Some(repository);
                self.expanded
                    .retain(|number| rows.iter().any(|row| row.number == *number));
                self.rows = rows;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        self.revision = self.revision.wrapping_add(1);
    }

    /// Expands a collapsed row or collapses an expanded one. Returns false for a number the
    /// panel doesn't list, so a stale click changes nothing.
    pub fn toggle_expanded(&mut self, number: u32) -> bool {
        if !self.rows.iter().any(|row| row.number == number) {
            return false;
        }
        if let Some(position) = self.expanded.iter().position(|item| *item == number) {
            self.expanded.remove(position);
        } else {
            self.expanded.push(number);
        }
        self.revision = self.revision.wrapping_add(1);
        true
    }
}

/// Characters of an issue body an expanded row shows before it truncates.
const SUMMARY_LIMIT: usize = 280;

#[derive(Clone, Debug, Serialize, Type)]
pub struct IssueDiscussion {
    pub summary: String,
    pub comments: Vec<IssueAgentComment>,
}

#[derive(Clone, Debug, Serialize, Type)]
pub struct IssueAgentComment {
    pub id: String,
    pub author: String,
    pub body: String,
}

fn agent_comment(body: &str) -> Option<(String, String)> {
    let (byline, message) = body.split_once("\n\n")?;
    let byline = byline.strip_prefix("Ready to close · ").unwrap_or(byline);
    let author = byline.strip_prefix("Velocity agent")?;
    let author = if author.is_empty() || author.starts_with(" · ") {
        "Velocity agent"
    } else {
        author.strip_prefix(" `")?.split_once('`')?.0
    };
    Some((author.into(), message.trim().into()))
}

/// The first prose paragraph of an issue body: headings, HTML comments, images and code
/// fences are skipped, since the rail has room for a few lines of plain text only.
fn body_summary(body: &str) -> String {
    let mut words = Vec::new();
    let mut in_fence = false;
    let mut in_comment = false;
    for line in body.lines() {
        let plain = line.trim();
        if plain.starts_with("```") {
            if !words.is_empty() {
                break;
            }
            in_fence = !in_fence;
            continue;
        }
        if in_comment {
            in_comment = !plain.contains("-->");
            continue;
        }
        if plain.starts_with("<!--") {
            in_comment = !plain.contains("-->");
            continue;
        }
        let skipped =
            in_fence || plain.starts_with('#') || plain.starts_with('<') || plain.starts_with("![");
        if plain.is_empty() || skipped {
            if !words.is_empty() {
                break;
            }
            continue;
        }
        words.push(plain.replace("**", ""));
    }
    truncate(words.join(" "))
}

fn truncate(text: String) -> String {
    match text.char_indices().nth(SUMMARY_LIMIT) {
        Some((end, _)) => format!("{}…", text[..end].trim_end()),
        None => text,
    }
}

/// The last panel projection read from the shared open-issues cache (#228, #556).
#[derive(Default)]
pub struct IssuePanelCache {
    version: Option<u64>,
    panel: Option<(String, Vec<IssuePanelRow>)>,
}

impl IssueClient {
    /// Read discussion on demand so comment latency stays outside the work inventory.
    /// Reuses cached discussions when the issue's `updated_at` in the shared open-issues
    /// cache has not moved, and reads comments via conditional REST (`If-None-Match`) when
    /// the summary is already known from the shared cache (#556, #589).
    pub fn discussion(&self, number: u32) -> Result<IssueDiscussion, GitHubCommandError> {
        if let Some(cached) =
            crate::issue_cache::get_discussion(self.host(), self.repository(), number)
        {
            return Ok(cached);
        }
        let cached_issue = crate::issue_cache::cached_issue(self.host(), self.repository(), number);
        let discussion = match &self.backend {
            IssueBackend::Collapse(collapse) => {
                self.discussion_via_collapse(collapse, number, cached_issue.as_ref())?
            }
            IssueBackend::GitHub(_) => {
                if let Some(issue) = &cached_issue {
                    self.discussion_via_rest(number, &issue.body)?
                } else {
                    self.discussion_via_graphql(number)?
                }
            }
        };
        crate::issue_cache::store_discussion(
            self.host(),
            self.repository(),
            number,
            cached_issue.map(|issue| issue.updated_at),
            &discussion,
        );
        Ok(discussion)
    }

    fn discussion_via_collapse(
        &self,
        collapse: &crate::collapse_issues::CollapseIssueTransport,
        number: u32,
        cached_issue: Option<&GitHubIssue>,
    ) -> Result<IssueDiscussion, GitHubCommandError> {
        let body = match cached_issue {
            Some(issue) => issue.body.clone(),
            None => collapse.read_issue(number)?.body,
        };
        let comments = collapse.issue_comments(number)?;
        Ok(IssueDiscussion {
            summary: body_summary(&body),
            comments: comments
                .into_iter()
                .rev()
                .filter_map(|comment| {
                    let (author, body) = agent_comment(&comment.body)?;
                    Some(IssueAgentComment {
                        id: comment.id.to_string(),
                        author,
                        body,
                    })
                })
                .take(5)
                .collect(),
        })
    }

    fn discussion_via_rest(
        &self,
        number: u32,
        body: &str,
    ) -> Result<IssueDiscussion, GitHubCommandError> {
        let transport = self.github_transport()?;
        #[derive(Deserialize)]
        struct RestComment {
            node_id: String,
            #[serde(default)]
            body: String,
        }
        let endpoint = format!(
            "repos/{}/issues/{number}/comments?per_page=100",
            self.repository()
        );
        let comments: Vec<RestComment> = transport.command.get_paginated(&endpoint)?;
        Ok(IssueDiscussion {
            summary: body_summary(body),
            comments: comments
                .into_iter()
                .rev()
                .filter_map(|comment| {
                    let (author, body) = agent_comment(&comment.body)?;
                    Some(IssueAgentComment {
                        id: comment.node_id,
                        author,
                        body,
                    })
                })
                .take(5)
                .collect(),
        })
    }

    fn discussion_via_graphql(&self, number: u32) -> Result<IssueDiscussion, GitHubCommandError> {
        let transport = self.github_transport()?;
        #[derive(Deserialize)]
        struct Comment {
            id: String,
            body: String,
        }
        #[derive(Deserialize)]
        struct Discussion {
            body: String,
            comments: Connection<Comment>,
        }
        let query = "query($owner:String!,$name:String!,$number:Int!,$cursor:String) {
          repository(owner:$owner,name:$name) { issue(number:$number) {
            body
            comments(first:100,after:$cursor) { nodes { id body }
              pageInfo { hasNextPage endCursor } } } } }";
        let variables = transport.variables(number);
        let issue: Discussion =
            transport.graphql_at(query, variables.clone(), "/data/repository/issue")?;
        let comments = transport.complete_connection(
            query,
            variables,
            "/data/repository/issue/comments",
            issue.comments,
        )?;
        Ok(IssueDiscussion {
            summary: body_summary(&issue.body),
            comments: comments
                .into_iter()
                .rev()
                .filter_map(|comment| {
                    let (author, body) = agent_comment(&comment.body)?;
                    Some(IssueAgentComment {
                        id: comment.id,
                        author,
                        body,
                    })
                })
                .take(5)
                .collect(),
        })
    }

    /// Reads the panel rows from the shared per-repository open-issues cache (#556),
    /// skipping row reconstruction when the shared cache version is unchanged.
    pub fn cached_panel(
        &self,
        cache: &mut IssuePanelCache,
    ) -> Result<(String, Vec<IssuePanelRow>), GitHubCommandError> {
        let issues = self.open_issues()?;
        let version = crate::issue_cache::version(self.host(), self.repository());
        if let Some(panel) = &cache.panel
            && version.is_some()
            && cache.version == version
        {
            return Ok(panel.clone());
        }
        let panel = self.project_panel(issues);
        *cache = IssuePanelCache {
            version,
            panel: Some(panel.clone()),
        };
        Ok(panel)
    }

    pub fn panel(&self) -> Result<(String, Vec<IssuePanelRow>), GitHubCommandError> {
        let issues = self.open_issues()?;
        Ok(self.project_panel(issues))
    }

    fn project_panel(&self, issues: Vec<GitHubIssue>) -> (String, Vec<IssuePanelRow>) {
        let repository = self.repository().to_owned();
        let mut rows: Vec<_> = issues
            .into_iter()
            .map(|issue| IssuePanelRow::from_issue_for_repository(Some(&repository), issue))
            .collect();
        rows.sort_by_key(|row| {
            (
                row.question.is_none(),
                std::cmp::Reverse(
                    row.question
                        .as_ref()
                        .map_or(0, |question| question.blocked_items),
                ),
                row.number,
            )
        });
        (repository, rows)
    }

    pub fn get(&self, number: u32) -> Result<GitHubIssue, GitHubCommandError> {
        self.read_issue(number)
    }

    /// Web URL for issue `number` on this client's configured issue host (#589).
    pub fn issue_url(&self, number: u32) -> String {
        match self.host() {
            velocity_core::collapse::IssueHost::Collapse => {
                velocity_core::collapse::issue_page(self.repository(), number).unwrap_or_else(
                    || format!("https://collapse.sh/{}/issues/{number}", self.repository()),
                )
            }
            velocity_core::collapse::IssueHost::GitHub => {
                format!("https://github.com/{}/issues/{number}", self.repository())
            }
        }
    }

    pub fn open_in_browser(&self, number: u32) -> Result<(), GitHubCommandError> {
        let repository = self.repository();
        if self.host() == velocity_core::collapse::IssueHost::Collapse {
            return Err(GitHubCommandError::Invalid(format!(
                "Open {} directly through the desktop opener",
                self.issue_url(number)
            )));
        }
        let output = crate::command::gh(&[
            "issue",
            "view",
            &number.to_string(),
            "--repo",
            repository,
            "--web",
        ])
        .output()?;
        if !output.status.success() {
            return Err(GitHubCommandError::Command {
                code: output.status.code(),
                message: String::from_utf8_lossy(&output.stderr).trim().into(),
            });
        }
        Ok(())
    }
}
