use serde::{Deserialize, Serialize};
use specta::Type;
use velocity_core::collapse::IssueHost;

/// An account or named token credited on an issue or comment (GitHub or Collapse).
#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct GitHubActor {
    pub login: String,
}

/// Host-neutral alias for [`GitHubActor`] (#590).
pub type IssueActor = GitHubActor;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum IssueState {
    Open,
    Closed,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct GitHubIssue {
    pub id: String,
    pub number: u32,
    pub title: String,
    pub body: String,
    pub url: String,
    pub state: IssueState,
    pub author: Option<GitHubActor>,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    /// Latest reopen timestamp from the issue host's timeline/change feed, so a reopened
    /// issue is recognized even before its previous completed work item is replaced (#602).
    #[serde(default)]
    pub reopened_at: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    pub scout: Option<String>,
    #[serde(default)]
    pub milestone: Option<IssueMilestone>,
    #[serde(default)]
    pub open_blocker_count: u32,
    #[serde(default)]
    pub open_blockers: Vec<String>,
    /// Closed `blockedBy` dependencies, preserved so Velocity's own project can hold
    /// dispatch until the running copy contains a closed blocker's tooling land (#601).
    #[serde(default)]
    pub closed_blockers: Vec<String>,
    /// Which issue host (`GitHub` or `Collapse`) produced this issue (#590).
    #[serde(default)]
    pub host: IssueHost,
}

/// Host-neutral alias for [`GitHubIssue`] (#590).
pub type Issue = GitHubIssue;

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct IssueMilestone {
    pub number: u32,
}

impl GitHubIssue {
    pub fn has_open_blockers(&self) -> bool {
        self.open_blocker_count > 0 || !self.open_blockers.is_empty()
    }

    /// Whether the issue is labelled `spec-question`: an open SPEC.md §8 question that
    /// waits on the owner's answer (F9), so it holds no build work of its own.
    pub fn is_spec_question(&self) -> bool {
        self.labels
            .iter()
            .any(|label| label.eq_ignore_ascii_case("spec-question"))
    }
}

/// The agent compares a finding against this entire inventory, including closed
/// issues. Filing refreshes it so a newly filed or edited issue is not missed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct IssueReviewToken {
    #[serde(default)]
    pub host: IssueHost,
    pub repository: String,
    pub issues: Vec<ReviewedIssue>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct ReviewedIssue {
    pub number: u32,
    pub updated_at: String,
    pub content_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct IssueInventory {
    #[serde(default)]
    pub host: IssueHost,
    pub repository: String,
    pub issues: Vec<GitHubIssue>,
    pub review_token: IssueReviewToken,
}

impl IssueInventory {
    pub(crate) fn new(
        repository: String,
        issues: Vec<GitHubIssue>,
    ) -> Result<Self, crate::GitHubCommandError> {
        Self::new_for_host(IssueHost::GitHub, repository, issues)
    }

    pub(crate) fn new_for_host(
        host: IssueHost,
        repository: String,
        issues: Vec<GitHubIssue>,
    ) -> Result<Self, crate::GitHubCommandError> {
        let mut reviewed: Vec<_> = issues
            .iter()
            .map(|issue| {
                Ok(ReviewedIssue {
                    number: issue.number,
                    updated_at: issue.updated_at.clone(),
                    content_hash: content_hash(&serde_json::to_vec(issue)?)?,
                })
            })
            .collect::<Result<_, crate::GitHubCommandError>>()?;
        reviewed.sort_by_key(|issue| issue.number);
        Ok(Self {
            review_token: IssueReviewToken {
                host,
                repository: repository.clone(),
                issues: reviewed,
            },
            host,
            repository,
            issues,
        })
    }
}

pub(crate) fn content_hash(bytes: &[u8]) -> Result<String, crate::GitHubCommandError> {
    let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha256);
    hasher.update(bytes);
    hasher
        .try_finalize()
        .map(|hash| hash.to_string())
        .map_err(|error| crate::GitHubCommandError::Invalid(error.to_string()))
}
