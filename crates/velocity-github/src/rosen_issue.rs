//! Issue completion follows Rosen's build evidence, with GitHub confirming the mirror.
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{IssueClient, IssueState, PullRequestError};

impl IssueClient {
    /// Applies one saved Rosen event effect. A lagging mirror leaves the close pending.
    pub fn follow_rosen_issue(
        &self,
        repository: &str,
        number: u32,
        commit: &str,
        sequence: u64,
        revert: Option<&str>,
    ) -> Result<bool, PullRequestError> {
        if !self.repository().eq_ignore_ascii_case(repository) {
            return Err(PullRequestError::Invalid(
                "The thread's issue repository differs from the project's mirror".into(),
            ));
        }
        if revert.is_none() && !self.rosen_commit_mirrored(commit)? {
            return Ok(false);
        }
        let (kind, body) = match revert {
            Some(reason) => (
                "reopen",
                format!(
                    "Rosen reverted landed commit `{commit}`. Reopening this issue.\n\nReason:\n\n{reason}"
                ),
            ),
            None => (
                "close",
                format!(
                    "Landed through Rosen as `{commit}`. The build passed after the land on a base \
                 including this commit. The GitHub mirror carries it."
                ),
            ),
        };
        let marker = format!("<!-- velocity-rosen-issue:{kind}:{commit}:{sequence} -->");
        self.sync_rosen_issue(number, &marker, &body, revert.is_some())?;
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
        reopen: bool,
    ) -> Result<(), PullRequestError> {
        let issue = self.read_issue(number)?;
        if !self.has_rosen_comment(number, marker)? {
            let _: Value = self.transport.graphql_at(
                "mutation($input:AddCommentInput!) { addComment(input:$input) { commentEdge { node { id } } } }",
                json!({"input":{"subjectId":issue.id,"body":format!("{body}\n\n{marker}")}}),
                "/data/addComment",
            )?;
        }
        if (issue.state == IssueState::Open) == reopen {
            return Ok(());
        }
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
