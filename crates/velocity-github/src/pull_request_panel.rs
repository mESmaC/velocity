//! The reference rail's PR projection (SPEC.md F6/F7). CI is informative only.
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use specta::Type;
use velocity_core::review_diff::DiffSummary;
use velocity_core::review_tier::{ReviewTier, assess_tier, sensitive_paths};

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
    /// PR duty's review tier (F7); absent until the PR's diff could be read.
    pub tier: Option<ReviewTier>,
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

/// What the panel read about each open PR beyond the list query, kept between refreshes so
/// a refresh costs the same few requests however many PRs are open (#228):
/// - each PR's diff summary at the head it was read at, read again only after the head moves;
/// - each PR's reviewer claim, read from its comments again only after the PR's `updatedAt`
///   or comment count moves. Posting or editing a comment moves `updatedAt`, and deleting one
///   changes the count, so a claim, a takeover or a removed claim reaches the rail on the next
///   refresh.
#[derive(Debug, Default)]
pub struct PullRequestPanelCache {
    summaries: BTreeMap<u32, (String, DiffSummary)>,
    reviewers: BTreeMap<u32, (ClaimFingerprint, Option<String>)>,
}

/// The listed fields that move when a PR's conversation comments change.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ClaimFingerprint {
    updated_at: String,
    comments: u32,
}

#[derive(Deserialize)]
struct PanelEntry {
    #[serde(flatten)]
    pull_request: PullRequest,
    #[serde(rename = "headRepository")]
    head_repository: Option<HeadRepository>,
    commits: CommitConnection,
    comments: CommentCount,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CommentCount {
    total_count: u32,
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
    /// The panel with each row's review tier (F7 Tiered review), read from the PR's diff and
    /// the Sensitive paths in `agents_markdown`. A diff that cannot be read leaves that row
    /// without a tier and is tried again on the next refresh. Keep `cache` between refreshes:
    /// with it, a refresh where no PR changed is one request.
    pub fn tiered_panel(
        &self,
        cache: &mut PullRequestPanelCache,
        agents_markdown: &str,
    ) -> Result<Vec<PullRequestPanelRow>, PullRequestError> {
        let entries = self.panel_entries()?;
        let sensitive = sensitive_paths(agents_markdown);
        let listed = |number: &u32| {
            entries
                .iter()
                .any(|entry| entry.pull_request.number == *number)
        };
        cache.summaries.retain(|number, _| listed(number));
        cache.reviewers.retain(|number, _| listed(number));
        let mut rows = Vec::with_capacity(entries.len());
        for entry in entries {
            let (number, head) = (
                entry.pull_request.number,
                entry.pull_request.head_ref_oid.clone(),
            );
            let reviewer = self.cached_reviewer(cache, &entry)?;
            let mut row = self.panel_row(entry, reviewer);
            row.tier = self
                .cached_summary(cache, number, head)
                .map(|summary| assess_tier(summary, &sensitive).tier);
            rows.push(row);
        }
        Ok(rows)
    }

    /// The PR's reviewer claim, from its comments only when they may have changed.
    fn cached_reviewer(
        &self,
        cache: &mut PullRequestPanelCache,
        entry: &PanelEntry,
    ) -> Result<Option<String>, PullRequestError> {
        let number = entry.pull_request.number;
        let fingerprint = ClaimFingerprint {
            updated_at: entry.pull_request.updated_at.clone(),
            comments: entry.comments.total_count,
        };
        if let Some((read_at, reviewer)) = cache.reviewers.get(&number)
            && *read_at == fingerprint
        {
            return Ok(reviewer.clone());
        }
        // Honor the repository's explicit claim comment. Read all pages so a busy
        // discussion cannot hide its reviewer. The fingerprint is from the list, read before
        // the comments, so a comment posted in between is read again next refresh.
        let reviewer = self
            .comments(number)?
            .into_iter()
            .find_map(|comment| claim_reviewer(&comment.body, comment.author));
        cache
            .reviewers
            .insert(number, (fingerprint, reviewer.clone()));
        Ok(reviewer)
    }

    fn cached_summary<'cache>(
        &self,
        cache: &'cache mut PullRequestPanelCache,
        number: u32,
        head: String,
    ) -> Option<&'cache DiffSummary> {
        let current = cache
            .summaries
            .get(&number)
            .is_some_and(|(read_at, _)| *read_at == head);
        if !current {
            let summary = self.diff_summary(number).ok()?;
            cache.summaries.insert(number, (head, summary));
        }
        cache.summaries.get(&number).map(|(_, summary)| summary)
    }

    fn panel_entries(&self) -> Result<Vec<PanelEntry>, PullRequestError> {
        let query = format!(
            "query($owner:String!,$name:String!,$cursor:String) {{
              repository(owner:$owner,name:$name) {{
                pullRequests(states:OPEN,first:100,after:$cursor,
                  orderBy:{{field:UPDATED_AT,direction:DESC}}) {{
                  nodes {{ {FIELDS}
                    headRepository {{ nameWithOwner }}
                    commits(last:1) {{ nodes {{ commit {{ statusCheckRollup {{ state }} }} }} }}
                    comments {{ totalCount }}
                  }} pageInfo {{ hasNextPage endCursor }}
                }}
              }}
            }}"
        );
        self.connection(&query, self.variables(0), "/data/repository/pullRequests")
    }

    fn panel_row(&self, entry: PanelEntry, reviewer: Option<String>) -> PullRequestPanelRow {
        let checks = entry
            .commits
            .nodes
            .first()
            .and_then(|node| node.commit.status_check_rollup.as_ref())
            .map(|rollup| check_status(&rollup.state))
            .unwrap_or(CheckStatus::Unavailable);
        let review = review_status(&entry.pull_request, reviewer.is_some(), checks);
        PullRequestPanelRow {
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
            tier: None,
            thread_identifier: None,
        }
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
