//! GitHub comments and the durable effects of landed Rosen issue work.
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{IssueClient, IssueState, PullRequestError};

const READY_LABEL: &str = "ready-to-close";

pub enum RosenIssueEffect<'a> {
    Landed { change: &'a str },
    Reviewed { by: &'a str },
    Watcher,
    Reverted { reason: &'a str },
}

impl IssueClient {
    pub fn post_issue_progress(
        &self,
        number: u32,
        byline: &str,
        body: &str,
        ready: bool,
    ) -> Result<String, PullRequestError> {
        let issue = self.read_issue(number)?;
        if ready {
            self.label_issue_ready(number)?;
        }
        let heading = if ready { "Ready to close · " } else { "" };
        self.add_issue_comment(&issue.id, &format!("{heading}{byline}\n\n{body}"))
    }

    fn label_issue_ready(&self, number: u32) -> Result<(), PullRequestError> {
        let labels = format!("repos/{}/labels", self.repository());
        let label = format!("{labels}/{READY_LABEL}");
        if self
            .transport
            .command
            .read::<Value>(&["api", &label])
            .is_err()
            && let Err(error) =
                self.transport
                    .command
                    .api::<Value>("POST", &labels, &json!({"name": READY_LABEL}))
        {
            // Another commenter can create the repository label at the same time.
            self.transport
                .command
                .read::<Value>(&["api", &label])
                .map_err(|_| error)?;
        }
        let endpoint = format!("repos/{}/issues/{number}/labels", self.repository());
        let _: Value =
            self.transport
                .command
                .api("POST", &endpoint, &json!({"labels": [READY_LABEL]}))?;
        Ok(())
    }

    fn add_issue_comment(&self, subject: &str, body: &str) -> Result<String, PullRequestError> {
        self.transport.graphql_at(
            "mutation($input:AddCommentInput!) { addComment(input:$input) { commentEdge { node { url } } } }",
            json!({"input":{"subjectId":subject,"body":body}}),
            "/data/addComment/commentEdge/node/url",
        )
    }

    /// Applies one saved Rosen event effect. A lagging mirror leaves the close pending.
    pub fn follow_rosen_issue(
        &self,
        repository: &str,
        number: u32,
        commit: &str,
        sequence: u64,
        effect: RosenIssueEffect<'_>,
    ) -> Result<bool, PullRequestError> {
        if !self.repository().eq_ignore_ascii_case(repository) {
            return Err(PullRequestError::Invalid(
                "The thread's issue repository differs from the project's mirror".into(),
            ));
        }
        let state = match effect {
            RosenIssueEffect::Landed { .. } => None,
            RosenIssueEffect::Reviewed { .. } | RosenIssueEffect::Watcher => {
                Some(IssueState::Closed)
            }
            RosenIssueEffect::Reverted { .. } => Some(IssueState::Open),
        };
        if state == Some(IssueState::Closed) && !self.rosen_commit_mirrored(commit)? {
            return Ok(false);
        }
        let (kind, body) = match effect {
            RosenIssueEffect::Landed { change } => (
                "landed",
                format!("Velocity · Rosen change `{change}` landed as commit `{commit}`."),
            ),
            RosenIssueEffect::Reverted { reason } => {
                let end = reason.floor_char_boundary(32_000);
                let output = &reason[..end];
                let truncated = if end < reason.len() {
                    "\n\n[Output truncated.]"
                } else {
                    ""
                };
                (
                    "reopen",
                    format!(
                        "Rosen reverted landed commit `{commit}`. Reopening this issue.\n\n{output}{truncated}"
                    ),
                )
            }
            RosenIssueEffect::Reviewed { by } => (
                "close",
                format!(
                    "Velocity review swarm · `{by}` reviewed Rosen commit `{commit}` and confirmed it addresses this issue."
                ),
            ),
            RosenIssueEffect::Watcher => (
                "close",
                format!(
                    "Velocity main watcher · Rosen commit `{commit}` passed its post-land build. \
                     The 30-minute review window elapsed without review coverage."
                ),
            ),
        };
        let marker = format!("<!-- velocity-rosen-issue:{kind}:{commit}:{sequence} -->");
        self.sync_rosen_issue(number, &marker, &body, state)?;
        Ok(true)
    }

    /// Returns false while the mirror's default branch has not caught up to the land.
    pub fn rosen_commit_mirrored(&self, commit: &str) -> Result<bool, PullRequestError> {
        if commit.len() != 40 || !commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(PullRequestError::Invalid(
                "Invalid landed commit SHA".into(),
            ));
        }
        let branch = self
            .transport
            .default_branch
            .as_deref()
            .ok_or_else(|| PullRequestError::Missing("default branch".into()))?;
        let mut variables = self.transport.variables(0);
        variables["branch"] = format!("refs/heads/{branch}").into();
        // Resolve the branch via GraphQL so names with slashes need no URL escaping.
        let tip: String = self.transport.graphql_at(
            "query($owner:String!,$name:String!,$branch:String!) {
              repository(owner:$owner,name:$name) { ref(qualifiedName:$branch) { target { oid } } } }",
            variables,
            "/data/repository/ref/target/oid",
        )?;
        let endpoint = format!("repos/{}/compare/{commit}...{tip}", self.repository());
        #[derive(Deserialize)]
        struct Comparison {
            status: String,
        }
        let comparison: Comparison = self.transport.command.read(&["api", &endpoint])?;
        Ok(matches!(comparison.status.as_str(), "ahead" | "identical"))
    }

    /// The marker makes a retry after an ambiguous mutation response reuse its comment.
    /// Comment first, state second: a failure can never lose the evidence on a closed issue.
    fn sync_rosen_issue(
        &self,
        number: u32,
        marker: &str,
        body: &str,
        state: Option<IssueState>,
    ) -> Result<(), PullRequestError> {
        let issue = self.read_issue(number)?;
        if !self.has_rosen_comment(number, marker)? {
            self.add_issue_comment(&issue.id, &format!("{body}\n\n{marker}"))?;
        }
        if state.is_some()
            && issue
                .labels
                .iter()
                .any(|label| label.eq_ignore_ascii_case(READY_LABEL))
        {
            let endpoint = format!(
                "repos/{}/issues/{number}/labels/{READY_LABEL}",
                self.repository()
            );
            self.transport
                .command
                .text(&["api", "--method", "DELETE", &endpoint])?;
        }
        let Some(state) = state.filter(|state| *state != issue.state) else {
            return Ok(());
        };
        let reopen = state == IssueState::Open;
        let mutation = if reopen {
            "mutation($input:ReopenIssueInput!) { reopenIssue(input:$input) { issue { id } } }"
        } else {
            "mutation($input:CloseIssueInput!) { closeIssue(input:$input) { issue { id } } }"
        };
        let path = if reopen {
            "/data/reopenIssue"
        } else {
            "/data/closeIssue"
        };
        let _: Value =
            self.transport
                .graphql_at(mutation, json!({"input":{"issueId":issue.id}}), path)?;
        Ok(())
    }

    fn has_rosen_comment(&self, number: u32, marker: &str) -> Result<bool, PullRequestError> {
        #[derive(Deserialize)]
        struct Comment {
            body: String,
        }
        let comments: Vec<Comment> = self.transport.connection(
            "query($owner:String!,$name:String!,$number:Int!,$cursor:String) {
              repository(owner:$owner,name:$name) { issue(number:$number) {
                comments(first:100,after:$cursor) { nodes { body }
                  pageInfo { hasNextPage endCursor } } } } }",
            self.transport.variables(number),
            "/data/repository/issue/comments",
        )?;
        Ok(comments
            .iter()
            .any(|comment| comment.body.lines().any(|line| line == marker)))
    }
}
