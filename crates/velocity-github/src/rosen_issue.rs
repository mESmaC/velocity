//! GitHub comments and the durable effects of landed Rosen issue work.
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{GitHubIssue, IssueClient, IssueState, PullRequestError};

const READY_LABEL: &str = "ready-to-close";
const NO_CHANGE_MARKER: &str = "<!-- velocity-issue-no-change:";

pub enum RosenIssueEffect<'a> {
    Landed { change: &'a str },
    Reviewed { by: &'a str },
    Watcher,
    Reverted { reason: &'a str },
}

impl IssueClient {
    pub const NO_CHANGE_LABEL: &str = "no-change";

    pub fn open_no_change_issues(&self) -> Result<Vec<GitHubIssue>, PullRequestError> {
        Ok(self
            .open_issues()?
            .into_iter()
            .filter(issue_has_no_change_label)
            .collect())
    }

    pub fn post_issue_progress(
        &self,
        number: u32,
        byline: &str,
        body: &str,
        ready: bool,
    ) -> Result<String, PullRequestError> {
        let issue = self.read_issue(number)?;
        if ready {
            self.label_issue(number, READY_LABEL)?;
        }
        let heading = if ready { "Ready to close · " } else { "" };
        self.add_issue_comment(&issue.id, &format!("{heading}{byline}\n\n{body}"))
    }

    pub fn post_no_change(
        &self,
        number: u32,
        change: &str,
        byline: &str,
        reason: &str,
    ) -> Result<(), PullRequestError> {
        self.post_issue_for_verification(number, change, "Ended with no change", byline, reason)
    }

    pub fn post_overlap_review(
        &self,
        number: u32,
        source: u32,
        commit: &str,
    ) -> Result<(), PullRequestError> {
        self.post_issue_for_verification(
            number,
            &format!("overlap:{source}:{commit}"),
            "Check against an overlapping land",
            "Velocity continuous dispatch",
            &format!(
                "This issue waited for #{source}. Rosen commit `{commit}` landed for that issue. \
                 Check this issue's acceptance criteria against the current base. Close it if \
                 the fix is verified, or return it to dispatch with the remaining work."
            ),
        )
    }

    fn post_issue_for_verification(
        &self,
        number: u32,
        attempt: &str,
        heading: &str,
        byline: &str,
        reason: &str,
    ) -> Result<(), PullRequestError> {
        let marker = format!("{NO_CHANGE_MARKER}{attempt} -->");
        if self.has_rosen_comment(number, &marker)? {
            // A reviewer may have removed the label before the local acknowledgement.
            return Ok(());
        }
        let issue = self.read_issue(number)?;
        if issue.state == IssueState::Open && !issue_has_no_change_label(&issue) {
            self.label_issue(number, Self::NO_CHANGE_LABEL)?;
        }
        let end = reason.floor_char_boundary(60_000);
        let truncated = if end < reason.len() {
            "\n\n[Message truncated.]"
        } else {
            ""
        };
        self.add_issue_comment(
            &issue.id,
            &format!(
                "## {heading}\n\n{byline}\n\n{}{truncated}\n\n{marker}",
                &reason[..end]
            ),
        )?;
        Ok(())
    }

    pub fn no_change_discussion(
        &self,
        number: u32,
    ) -> Result<(GitHubIssue, Vec<String>, String), PullRequestError> {
        let issue = self.read_issue(number)?;
        if issue.state != IssueState::Open || !issue_has_no_change_label(&issue) {
            return Err(PullRequestError::Invalid(
                "The issue is no longer waiting for no-change review".into(),
            ));
        }
        let comments = self.issue_comment_bodies(number)?;
        let (report, attempt) = comments
            .iter()
            .rev()
            .find_map(|body| {
                let attempt = body
                    .lines()
                    .last()?
                    .strip_prefix(NO_CHANGE_MARKER)?
                    .strip_suffix(" -->")?;
                Some((body, attempt.to_owned()))
            })
            .ok_or_else(|| {
                PullRequestError::Missing("The no-change report is still pending".into())
            })?;
        let mut discussion = vec![report.clone()];
        discussion.extend(
            comments
                .iter()
                .rev()
                .filter(|body| *body != report)
                .take(2)
                .cloned(),
        );
        Ok((issue, discussion, attempt))
    }

    pub fn resolve_no_change(
        &self,
        number: u32,
        attempt: &str,
        reviewer: &str,
        evidence: &str,
        close: bool,
    ) -> Result<(), PullRequestError> {
        let (_, _, current) = self.no_change_discussion(number)?;
        if current != attempt || evidence.trim().is_empty() {
            return Err(PullRequestError::Invalid(
                "Review the current report and supply verification or retry instructions".into(),
            ));
        }
        let decision = if close {
            "verified already fixed"
        } else {
            "returned to dispatch"
        };
        let marker = format!("<!-- velocity-no-change-review:{current}:{close} -->");
        let body = format!("Velocity review swarm · `{reviewer}` {decision}.\n\n{evidence}");
        self.sync_rosen_issue(number, &marker, &body, close.then_some(IssueState::Closed))?;
        if !close {
            self.remove_issue_label(number, Self::NO_CHANGE_LABEL)?;
        }
        Ok(())
    }

    fn label_issue(&self, number: u32, name: &str) -> Result<(), PullRequestError> {
        let labels = format!("repos/{}/labels", self.repository());
        let label = format!("{labels}/{name}");
        if self
            .transport
            .command
            .read::<Value>(&["api", &label])
            .is_err()
            && let Err(error) = self.transport.command.api::<Value>(
                "POST",
                &labels,
                &json!({"name": name, "color": "ededed"}),
            )
        {
            // Another commenter can create the repository label at the same time.
            self.transport
                .command
                .read::<Value>(&["api", &label])
                .map_err(|_| error)?;
        }
        let endpoint = format!("repos/{}/issues/{number}/labels", self.repository());
        let _: Value = self
            .transport
            .command
            .api("POST", &endpoint, &json!({"labels": [name]}))?;
        Ok(())
    }

    fn remove_issue_label(&self, number: u32, label: &str) -> Result<(), PullRequestError> {
        let endpoint = format!("repos/{}/issues/{number}/labels/{label}", self.repository());
        self.transport
            .command
            .text(&["api", "--method", "DELETE", &endpoint])?;
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

    /// Publishes a pass on the exact mirrored base tested by Velocity.
    pub fn publish_integration_success(&self, commit: &str) -> Result<bool, PullRequestError> {
        if !self.rosen_commit_mirrored(commit)? {
            return Ok(false);
        }
        #[derive(Deserialize)]
        struct Status {
            context: String,
            state: String,
        }
        const CONTEXT: &str = "velocity/integration";
        let statuses = format!(
            "repos/{}/commits/{commit}/statuses?per_page=100",
            self.repository()
        );
        let existing: Vec<Status> = self.transport.command.read(&["api", &statuses])?;
        if existing
            .iter()
            .find(|status| status.context == CONTEXT)
            .is_some_and(|status| status.state == "success")
        {
            return Ok(true);
        }
        let endpoint = format!("repos/{}/statuses/{commit}", self.repository());
        let _: Value = self.transport.command.api(
            "POST",
            &endpoint,
            &json!({
                "context": CONTEXT,
                "state": "success",
                "description": "Velocity integration build passed"
            }),
        )?;
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
            self.remove_issue_label(number, READY_LABEL)?;
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
        Ok(self
            .issue_comment_bodies(number)?
            .iter()
            .any(|body| body.lines().last() == Some(marker)))
    }

    fn issue_comment_bodies(&self, number: u32) -> Result<Vec<String>, PullRequestError> {
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
        Ok(comments.into_iter().map(|comment| comment.body).collect())
    }
}

fn issue_has_no_change_label(issue: &GitHubIssue) -> bool {
    issue
        .labels
        .iter()
        .any(|label| label.eq_ignore_ascii_case(IssueClient::NO_CHANGE_LABEL))
}
