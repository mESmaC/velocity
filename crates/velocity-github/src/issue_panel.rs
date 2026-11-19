//! The F9 rail and spec-question projection. Refreshing never opens a browser.
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use specta::Type;

use crate::pull_requests::Connection;
use crate::{GitHubIssue, IssueClient, PullRequestError};

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
    pub thread_identifier: Option<u32>,
    pub thread_label: Option<String>,
}

impl IssuePanelRow {
    pub fn from_issue(issue: GitHubIssue) -> Self {
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
        Self {
            number: issue.number,
            title: issue.title,
            labels: issue.labels,
            scout: issue.scout,
            question,
            summary: body_summary(&issue.body),
            blocked_by,
            thread_identifier: None,
            thread_label: None,
        }
    }
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

#[derive(Debug, Serialize, Type)]
pub struct IssueDiscussion {
    pub summary: String,
    pub comments: Vec<IssueAgentComment>,
}

#[derive(Debug, Serialize, Type)]
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

/// The open issues are read in full at least this often, even when the probe saw no change.
/// Renaming a repository label, for one, changes what the rail shows without moving any
/// issue's `updatedAt`.
pub const ISSUE_FULL_REFRESH: Duration = Duration::from_secs(10 * 60);

/// The last full read of the open issues and what the probe showed just before it, kept
/// between refreshes so unchanged issues only need a lightweight probe (#228).
#[derive(Default)]
pub struct IssuePanelCache {
    probe: Option<Vec<serde_json::Value>>,
    read_at: Option<Instant>,
    panel: Option<(String, Vec<IssuePanelRow>)>,
}

impl IssueClient {
    /// Read discussion on demand so comment latency stays outside the work inventory.
    pub fn discussion(&self, number: u32) -> Result<IssueDiscussion, PullRequestError> {
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
        let variables = self.transport.variables(number);
        let issue: Discussion =
            self.transport
                .graphql_at(query, variables.clone(), "/data/repository/issue")?;
        let comments = self.transport.complete_connection(
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

    /// GitHub can close a blocker in another repository without editing the dependent issue.
    /// The probe includes the open blocker counts so the rail picks up those changes.
    /// Periodic full reads also catch changes that leave the timestamps and counts unchanged.
    pub fn cached_panel(
        &self,
        cache: &mut IssuePanelCache,
    ) -> Result<(String, Vec<IssuePanelRow>), PullRequestError> {
        let probe = self.probe_open_issues()?;
        let fresh = cache
            .read_at
            .is_some_and(|read_at| read_at.elapsed() < ISSUE_FULL_REFRESH);
        if let Some(panel) = &cache.panel
            && fresh
            && cache.probe.as_ref() == Some(&probe)
        {
            return Ok(panel.clone());
        }
        // The probe was taken before this read, so a change in between is read again next time.
        let panel = self.panel()?;
        *cache = IssuePanelCache {
            probe: Some(probe),
            read_at: Some(Instant::now()),
            panel: Some(panel.clone()),
        };
        Ok(panel)
    }

    fn probe_open_issues(&self) -> Result<Vec<serde_json::Value>, PullRequestError> {
        self.transport.connection(
            "query($owner:String!,$name:String!,$cursor:String) { repository(owner:$owner,name:$name) {
              issues(states:OPEN,first:100,after:$cursor,orderBy:{field:CREATED_AT,direction:ASC}) {
                nodes { number updatedAt issueDependenciesSummary { blockedBy } }
                pageInfo { hasNextPage endCursor } } } }",
            self.transport.variables(0),
            "/data/repository/issues",
        )
    }

    pub fn panel(&self) -> Result<(String, Vec<IssuePanelRow>), PullRequestError> {
        let mut rows: Vec<_> = self
            .open_issues()?
            .into_iter()
            .map(IssuePanelRow::from_issue)
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
        Ok((self.transport.repository.clone(), rows))
    }

    pub fn get(&self, number: u32) -> Result<GitHubIssue, PullRequestError> {
        self.read_issue(number)
    }

    pub fn open_in_browser(&self, number: u32) -> Result<(), PullRequestError> {
        let output = crate::command::gh(&[
            "issue",
            "view",
            &number.to_string(),
            "--repo",
            &self.transport.repository,
            "--web",
        ])
        .output()?;
        if !output.status.success() {
            return Err(PullRequestError::Command {
                code: output.status.code(),
                message: String::from_utf8_lossy(&output.stderr).trim().into(),
            });
        }
        Ok(())
    }
}
