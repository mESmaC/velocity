use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct GitHubActor {
    pub login: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestRepository {
    pub name_with_owner: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PullRequest {
    pub id: String,
    pub number: u32,
    pub title: String,
    pub body: String,
    pub url: String,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub author: Option<GitHubActor>,
    pub head_ref_oid: String,
    pub head_ref_name: String,
    pub base_ref_name: String,
    pub additions: u32,
    pub deletions: u32,
    #[serde(default)]
    pub changed_files: u32,
    pub created_at: String,
    pub updated_at: String,
    pub closed_at: Option<String>,
    pub merged_at: Option<String>,
    pub review_decision: Option<String>,
    pub merge_state_status: String,
    #[serde(default)]
    pub head_repository: Option<PullRequestRepository>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PullRequestState {
    Open,
    Closed,
    Merged,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestComment {
    pub id: String,
    pub body: String,
    pub url: String,
    pub author: Option<GitHubActor>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct ReviewCommit {
    pub oid: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReview {
    pub id: String,
    pub body: String,
    pub state: String,
    pub author: Option<GitHubActor>,
    pub submitted_at: Option<String>,
    pub commit: Option<ReviewCommit>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestReviewThread {
    pub id: String,
    pub path: String,
    pub line: Option<u32>,
    pub start_line: Option<u32>,
    pub is_resolved: bool,
    pub is_outdated: bool,
    #[serde(default)]
    pub comments: Vec<PullRequestComment>,
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
#[serde(tag = "__typename")]
pub enum PullRequestCheck {
    #[serde(rename_all = "camelCase")]
    CheckRun {
        name: String,
        status: String,
        conclusion: Option<String>,
        details_url: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    StatusContext {
        context: String,
        state: String,
        target_url: Option<String>,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, Type)]
pub struct PullRequestSnapshot {
    pub pull_request: PullRequest,
    pub checks: Vec<PullRequestCheck>,
    pub reviews: Vec<PullRequestReview>,
    pub comments: Vec<PullRequestComment>,
    pub review_threads: Vec<PullRequestReviewThread>,
}
