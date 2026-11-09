use serde::{Deserialize, Serialize};
use specta::Type;

use crate::GitHubActor;

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
    #[serde(default)]
    pub labels: Vec<String>,
    pub scout: Option<String>,
}

/// The agent compares a finding against this entire inventory, including closed
/// issues. Filing refreshes it so a newly filed or edited issue is not missed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
pub struct IssueReviewToken {
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
    pub repository: String,
    pub issues: Vec<GitHubIssue>,
    pub review_token: IssueReviewToken,
}

impl IssueInventory {
    pub(crate) fn new(
        repository: String,
        issues: Vec<GitHubIssue>,
    ) -> Result<Self, crate::PullRequestError> {
        let mut reviewed: Vec<_> = issues
            .iter()
            .map(|issue| {
                Ok(ReviewedIssue {
                    number: issue.number,
                    updated_at: issue.updated_at.clone(),
                    content_hash: content_hash(&serde_json::to_vec(issue)?)?,
                })
            })
            .collect::<Result<_, crate::PullRequestError>>()?;
        reviewed.sort_by_key(|issue| issue.number);
        Ok(Self {
            review_token: IssueReviewToken {
                repository: repository.clone(),
                issues: reviewed,
            },
            repository,
            issues,
        })
    }
}

pub(crate) fn content_hash(bytes: &[u8]) -> Result<String, crate::PullRequestError> {
    let mut hasher = gix::hash::hasher(gix::hash::Kind::Sha256);
    hasher.update(bytes);
    hasher
        .try_finalize()
        .map(|hash| hash.to_string())
        .map_err(|error| crate::PullRequestError::Invalid(error.to_string()))
}
