use serde_json::{Value, json};

use crate::{
    CreatePullRequest, MergeMethod, NewReviewComment, PullRequestClient, PullRequestError,
    PullRequestOperation, PullRequestReceipt, PullRequestState, ReviewAction,
};

impl PullRequestClient {
    pub fn execute(
        &self,
        operation: PullRequestOperation,
    ) -> Result<PullRequestReceipt, PullRequestError> {
        let (number, name) = match operation {
            PullRequestOperation::Create(request) => (self.create(request)?, "created"),
            PullRequestOperation::Review {
                number,
                expected_head,
                action,
                body,
                comments,
            } => {
                self.review(number, &expected_head, action, &body, &comments)?;
                (number, "reviewed")
            }
            PullRequestOperation::Merge {
                number,
                expected_head,
                method,
            } => {
                self.merge(number, &expected_head, method)?;
                (number, "merged")
            }
            discussion => return self.execute_discussion(discussion),
        };
        Ok(PullRequestReceipt {
            number,
            operation: name.into(),
        })
    }

    fn execute_discussion(
        &self,
        operation: PullRequestOperation,
    ) -> Result<PullRequestReceipt, PullRequestError> {
        let (number, name) = match operation {
            PullRequestOperation::Comment { number, body } => {
                self.comment(number, &body)?;
                (number, "commented")
            }
            PullRequestOperation::Reply {
                number,
                thread,
                body,
            } => {
                self.thread_mutation(number, &thread, Some(body))?;
                (number, "replied")
            }
            PullRequestOperation::Resolve { number, thread } => {
                self.thread_mutation(number, &thread, None)?;
                (number, "resolved")
            }
            PullRequestOperation::RequestReview { number, reviewers } => {
                self.request_review(number, &reviewers)?;
                (number, "review_requested")
            }
            PullRequestOperation::Close { number } => {
                self.close(number)?;
                (number, "closed")
            }
            _ => {
                return Err(PullRequestError::Invalid(
                    "Expected a discussion operation".into(),
                ));
            }
        };
        Ok(PullRequestReceipt {
            number,
            operation: name.into(),
        })
    }

    fn create(&self, request: CreatePullRequest) -> Result<u32, PullRequestError> {
        require_text(&request.title, "PR title")?;
        require_text(&request.head_branch, "PR head branch")?;
        let body = json!({"title":request.title,"body":request.body,"head":request.head_branch,
            "base":"master","draft":request.draft});
        let response: Value =
            self.command
                .api("POST", &format!("repos/{}/pulls", self.repository), &body)?;
        response
            .get("number")
            .and_then(Value::as_u64)
            .and_then(|number| u32::try_from(number).ok())
            .ok_or_else(|| PullRequestError::Missing("created PR number".into()))
    }

    fn write(&self, number: u32, suffix: &str, body: &Value) -> Result<(), PullRequestError> {
        self.command.api::<Value>(
            "POST",
            &format!("repos/{}/pulls/{number}/{suffix}", self.repository),
            body,
        )?;
        Ok(())
    }

    fn comment(&self, number: u32, body: &str) -> Result<(), PullRequestError> {
        self.get(number)?;
        require_text(body, "comment")?;
        self.command.api::<Value>(
            "POST",
            &format!("repos/{}/issues/{number}/comments", self.repository),
            &json!({"body":body}),
        )?;
        Ok(())
    }

    fn request_review(&self, number: u32, reviewers: &[String]) -> Result<(), PullRequestError> {
        if reviewers.is_empty() {
            return Err(PullRequestError::Invalid(
                "Choose at least one reviewer".into(),
            ));
        }
        self.write(
            number,
            "requested_reviewers",
            &json!({"reviewers":reviewers}),
        )
    }

    fn close(&self, number: u32) -> Result<(), PullRequestError> {
        if self.get(number)?.state != PullRequestState::Open {
            return Err(PullRequestError::Invalid(
                "Only an open PR can be closed".into(),
            ));
        }
        let response: Value = self.command.api(
            "PATCH",
            &format!("repos/{}/pulls/{number}", self.repository),
            &json!({"state":"closed"}),
        )?;
        if response.get("merged").and_then(Value::as_bool) == Some(true) {
            return Err(PullRequestError::Invalid(
                "PR merged before it could be closed".into(),
            ));
        }
        Ok(())
    }

    fn review(
        &self,
        number: u32,
        head: &str,
        action: ReviewAction,
        body: &str,
        comments: &[NewReviewComment],
    ) -> Result<(), PullRequestError> {
        if self.get(number)?.head_ref_oid != head {
            return Err(PullRequestError::Invalid(
                "PR head changed; review the new commit".into(),
            ));
        }
        self.write(
            number,
            "reviews",
            &json!({"event":action,"body":body,"commit_id":head,"comments":comments}),
        )
    }

    fn thread_mutation(
        &self,
        number: u32,
        thread: &str,
        body: Option<String>,
    ) -> Result<(), PullRequestError> {
        self.require_thread(number, thread)?;
        let (mutation, input_type, input) = if let Some(body) = body {
            require_text(&body, "reply")?;
            (
                "addPullRequestReviewThreadReply",
                "AddPullRequestReviewThreadReplyInput",
                json!({"pullRequestReviewThreadId":thread,"body":body}),
            )
        } else {
            (
                "resolveReviewThread",
                "ResolveReviewThreadInput",
                json!({"threadId":thread}),
            )
        };
        let query = format!(
            "mutation($input:{input_type}!) {{ {mutation}(input:$input) {{ clientMutationId }} }}"
        );
        self.graphql_at::<Value>(&query, json!({"input":input}), &format!("/data/{mutation}"))?;
        Ok(())
    }

    fn merge(&self, number: u32, head: &str, method: MergeMethod) -> Result<(), PullRequestError> {
        let snapshot = self.snapshot(number)?;
        self.require_merge(&snapshot, head)?;
        let query = "mutation($input:MergePullRequestInput!) {
          mergePullRequest(input:$input) { pullRequest { state } } }";
        let response: Value = self.graphql_at(
            query,
            json!({"input":{
                "pullRequestId":snapshot.pull_request.id,"expectedHeadOid":head,"mergeMethod":method
            }}),
            "/data/mergePullRequest/pullRequest",
        )?;
        if response.get("state").and_then(Value::as_str) != Some("MERGED") {
            return Err(PullRequestError::Invalid(
                "GitHub did not merge the PR".into(),
            ));
        }
        Ok(())
    }
}

fn require_text(text: &str, field: &str) -> Result<(), PullRequestError> {
    if text.trim().is_empty() {
        return Err(PullRequestError::Invalid(format!(
            "{field} cannot be empty"
        )));
    }
    Ok(())
}
