//! The reference rail's PR projection (SPEC.md F6/F7). CI is informative only.
use serde::{Deserialize, Serialize};
use specta::Type;

use crate::pull_request_queries::FIELDS;
use crate::{PullRequest, PullRequestClient, PullRequestError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passing,
    Running,
    Failing,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Draft,
    Checking,
    Unclaimed,
    Reviewing,
    ChangesRequested,
    Approved,
}

impl ReviewStatus {
    fn label(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Checking => "checking",
            Self::Unclaimed => "needs a reviewer",
            Self::Reviewing => "reviewing",
            Self::ChangesRequested => "changes req.",
            Self::Approved => "approved",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PullRequestPanelRow {
    pub number: u32,
    pub title: String,
    pub head_branch: String,
    pub head_in_project: bool,
    pub additions: u32,
    pub deletions: u32,
    pub checks: CheckStatus,
    pub review: ReviewStatus,
    pub review_label: String,
    pub reviewer: Option<String>,
    pub thread_identifier: Option<u32>,
}

pub struct PullRequestThread<'a> {
    pub identifier: u32,
    pub number: Option<u32>,
    pub branch: &'a str,
}

impl PullRequestPanelRow {
    pub fn owning_thread(&self, threads: &[PullRequestThread<'_>]) -> Option<u32> {
        // A recorded PR identity wins over a provisional branch association.
        threads
            .iter()
            .find(|thread| thread.number == Some(self.number))
            .or_else(|| {
                threads.iter().find(|thread| {
                    self.head_in_project
                        && thread.number.is_none()
                        && !self.head_branch.is_empty()
                        && thread.branch == self.head_branch
                })
            })
            .map(|thread| thread.identifier)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct PullRequestPanelSnapshot {
    pub revision: u32,
    pub project_identifier: Option<u32>,
    pub loading: bool,
    pub rows: Vec<PullRequestPanelRow>,
    pub error: Option<String>,
}

impl PullRequestPanelSnapshot {
    pub fn select(&mut self, project_identifier: Option<u32>) {
        if self.project_identifier != project_identifier {
            self.project_identifier = project_identifier;
            self.rows.clear();
            self.error = None;
            self.loading = project_identifier.is_some();
            self.revision = self.revision.wrapping_add(1);
        }
    }

    pub fn update(&mut self, result: Result<Vec<PullRequestPanelRow>, String>) -> bool {
        let mut next = self.clone();
        next.loading = false;
        match result {
            Ok(rows) => {
                next.rows = rows;
                next.error = None;
            }
            // Keep the last successful snapshot visible, marked stale by the error.
            Err(error) => next.error = Some(error),
        }
        if *self == next {
            return false;
        }
        next.revision = self.revision.wrapping_add(1);
        *self = next;
        true
    }
}

#[derive(Deserialize)]
struct PanelEntry {
    #[serde(flatten)]
    pull_request: PullRequest,
    #[serde(rename = "headRepository")]
    head_repository: Option<HeadRepository>,
    commits: CommitConnection,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct HeadRepository {
    name_with_owner: String,
}

#[derive(Deserialize)]
struct CommitConnection {
    nodes: Vec<CommitNode>,
}

#[derive(Deserialize)]
struct CommitNode {
    commit: CommitStatus,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommitStatus {
    status_check_rollup: Option<CheckRollup>,
}

#[derive(Deserialize)]
struct CheckRollup {
    state: String,
}

impl PullRequestClient {
    pub fn panel(&self) -> Result<Vec<PullRequestPanelRow>, PullRequestError> {
        let query = format!(
            "query($owner:String!,$name:String!,$cursor:String) {{
              repository(owner:$owner,name:$name) {{
                pullRequests(states:OPEN,first:100,after:$cursor,
                  orderBy:{{field:UPDATED_AT,direction:DESC}}) {{
                  nodes {{ {FIELDS}
                    headRepository {{ nameWithOwner }}
                    commits(last:1) {{ nodes {{ commit {{ statusCheckRollup {{ state }} }} }} }}
                  }} pageInfo {{ hasNextPage endCursor }}
                }}
              }}
            }}"
        );
        let entries: Vec<PanelEntry> =
            self.connection(&query, self.variables(0), "/data/repository/pullRequests")?;
        entries
            .into_iter()
            .map(|entry| self.panel_row(entry))
            .collect()
    }

    fn panel_row(&self, entry: PanelEntry) -> Result<PullRequestPanelRow, PullRequestError> {
        // Honor the repository's explicit claim comment. Read all pages so a busy
        // discussion cannot hide its reviewer.
        let reviewer = self
            .comments(entry.pull_request.number)?
            .into_iter()
            .find_map(|comment| claim_reviewer(&comment.body, comment.author));
        let checks = entry
            .commits
            .nodes
            .first()
            .and_then(|node| node.commit.status_check_rollup.as_ref())
            .map(|rollup| check_status(&rollup.state))
            .unwrap_or(CheckStatus::Unavailable);
        let review = review_status(&entry.pull_request, reviewer.is_some(), checks);
        Ok(PullRequestPanelRow {
            number: entry.pull_request.number,
            title: entry.pull_request.title,
            head_branch: entry.pull_request.head_ref_name,
            head_in_project: entry.head_repository.is_some_and(|repository| {
                repository
                    .name_with_owner
                    .eq_ignore_ascii_case(&self.repository)
            }),
            additions: entry.pull_request.additions,
            deletions: entry.pull_request.deletions,
            checks,
            review,
            review_label: review.label().into(),
            reviewer,
            thread_identifier: None,
        })
    }

    /// Only an explicit row click opens a browser; refreshes never do.
    pub fn open_in_browser(&self, number: u32) -> Result<(), PullRequestError> {
        let output = crate::command::gh()
            .args([
                "pr",
                "view",
                &number.to_string(),
                "--repo",
                &self.repository,
                "--web",
            ])
            .output()?;
        if !output.status.success() {
            return Err(PullRequestError::Command {
                code: output.status.code(),
                message: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        Ok(())
    }
}

/// The reviewer a claim comment names. `Reviewing · <nick>` is a PR duty agent's claim
/// (F7): every agent shares one GitHub login, so the nick, not the author, is the reviewer.
/// A bare `Reviewing` names its author.
pub(crate) fn claim_reviewer(body: &str, author: Option<crate::GitHubActor>) -> Option<String> {
    let body = body.trim();
    let (lead, nick) = match body.split_once('·') {
        Some((lead, nick)) => (lead.trim(), Some(nick.trim())),
        None => (body, None),
    };
    if !lead.eq_ignore_ascii_case("Reviewing") {
        return None;
    }
    match nick {
        Some(nick) if !nick.is_empty() && !nick.contains(char::is_whitespace) => Some(nick.into()),
        Some(_) => None,
        None => author.map(|author| author.login),
    }
}

fn check_status(state: &str) -> CheckStatus {
    match state {
        "SUCCESS" => CheckStatus::Passing,
        "ERROR" | "FAILURE" => CheckStatus::Failing,
        "PENDING" | "EXPECTED" => CheckStatus::Running,
        _ => CheckStatus::Unavailable,
    }
}

fn review_status(pull_request: &PullRequest, claimed: bool, checks: CheckStatus) -> ReviewStatus {
    if pull_request.is_draft {
        return ReviewStatus::Draft;
    }
    match pull_request.review_decision.as_deref() {
        Some("APPROVED") => ReviewStatus::Approved,
        Some("CHANGES_REQUESTED") => ReviewStatus::ChangesRequested,
        _ if claimed => ReviewStatus::Reviewing,
        _ if checks == CheckStatus::Running => ReviewStatus::Checking,
        _ => ReviewStatus::Unclaimed,
    }
}
