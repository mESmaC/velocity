use serde_json::{Value, json};

use crate::approval::mirror_body;
use crate::pull_request_reviews::missing_record;
use crate::{
    CreatePullRequest, MergeMethod, NewReviewComment, PullRequest, PullRequestClient,
    PullRequestError, PullRequestOperation, PullRequestReceipt, PullRequestState, ReviewAction,
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
            } => (number, self.merge(number, &expected_head, method)?),
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
        let base_branch = request
            .base_branch
            .or_else(|| self.default_branch.clone())
            .ok_or_else(|| {
                PullRequestError::Invalid(
                    "Repository has no default branch; supply a base branch".into(),
                )
            })?;
        require_text(&base_branch, "PR base branch")?;
        let body = json!({"title":request.title,"body":request.body,"head":request.head_branch,
            "base":base_branch,"draft":request.draft});
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
        let pull_request = self.get(number)?;
        if pull_request.head_ref_oid != head {
            return Err(PullRequestError::Invalid(
                "PR head changed; review the new commit".into(),
            ));
        }
        let approve = match action {
            ReviewAction::Comment => {
                let review =
                    json!({"event":action,"body":body,"commit_id":head,"comments":comments});
                return self.write(number, "reviews", &review);
            }
            ReviewAction::Approve => true,
            ReviewAction::RequestChanges => false,
        };
        let record = self.review_record.as_ref().ok_or_else(missing_record)?;
        require_text(body, "review reason")?;
        if !approve {
            // Recorded first: the merge is blocked before anything else can fail.
            record.request_changes(number, head)?;
        }
        // GitHub refuses a review that approves or blocks the login's own PR,
        // which is every PR when all agents share one login. The mirror is
        // then a comment review; GitHub's review state is advisory (F7).
        let viewer = self.viewer_login()?;
        let own = pull_request
            .author
            .as_ref()
            .is_some_and(|author| author.login.eq_ignore_ascii_case(&viewer));
        let event = if own { ReviewAction::Comment } else { action };
        let mirror = mirror_body(approve, head, record.nick(), body);
        self.write(
            number,
            "reviews",
            &json!({"event":event,"body":mirror,"commit_id":head,"comments":comments}),
        )?;
        if approve {
            record.approve(number, head).map_err(|error| {
                PullRequestError::Invalid(format!(
                    "The approval comment is on #{number}, but the approval was not recorded, \
                     so the merge stays blocked: {error}"
                ))
            })?;
        }
        Ok(())
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

    /// Merges only the head Velocity's record approved, then deletes the
    /// branch (F7 "Delete the branch after merge"). Returns the receipt name.
    fn merge(
        &self,
        number: u32,
        head: &str,
        method: MergeMethod,
    ) -> Result<&'static str, PullRequestError> {
        let snapshot = self.snapshot(number)?;
        self.require_merge(&snapshot, head)?;
        let record = self.review_record.as_ref().ok_or_else(missing_record)?;
        let response = match self.merge_on_github(&snapshot.pull_request.id, head, method) {
            Ok(response) => response,
            // F7: a refused merge (for example branch protection demanding a
            // GitHub approval) is a `failed` event; it is never bypassed.
            Err(error) => {
                return Err(
                    match record.merge_failed(number, head, &error.to_string()) {
                        Ok(()) => error,
                        Err(record_error) => PullRequestError::Invalid(format!(
                            "{error}; the failed event was not recorded: {record_error}"
                        )),
                    },
                );
            }
        };
        let head_reference = response.pointer("/headRef/id").and_then(Value::as_str);
        Ok(match head_reference {
            Some(reference) if self.owns_head_branch(&snapshot.pull_request) => {
                if self.delete_reference(reference).is_ok() {
                    "merged"
                } else {
                    "merged_branch_kept"
                }
            }
            // Already deleted (GitHub's automatic head-branch deletion).
            None => "merged",
            Some(_) => "merged_branch_kept",
        })
    }

    fn merge_on_github(
        &self,
        identifier: &str,
        head: &str,
        method: MergeMethod,
    ) -> Result<Value, PullRequestError> {
        let query = "mutation($input:MergePullRequestInput!) {
          mergePullRequest(input:$input) { pullRequest { state headRef { id } } } }";
        let response: Value = self.graphql_at(
            query,
            json!({"input":{"pullRequestId":identifier,"expectedHeadOid":head,"mergeMethod":method}}),
            "/data/mergePullRequest/pullRequest",
        )?;
        if response.get("state").and_then(Value::as_str) != Some("MERGED") {
            return Err(PullRequestError::Invalid(
                "GitHub did not merge the PR".into(),
            ));
        }
        Ok(response)
    }

    /// Only a same-repository topic branch is deleted: never a fork's branch,
    /// the repository's default branch, or the branch the PR merged into.
    fn owns_head_branch(&self, pull_request: &PullRequest) -> bool {
        let same_repository = pull_request
            .head_repository
            .as_ref()
            .is_some_and(|head| head.name_with_owner.eq_ignore_ascii_case(&self.repository));
        same_repository
            && pull_request.head_ref_name != pull_request.base_ref_name
            && self.default_branch.as_deref() != Some(pull_request.head_ref_name.as_str())
    }

    fn delete_reference(&self, reference: &str) -> Result<(), PullRequestError> {
        let query =
            "mutation($input:DeleteRefInput!) { deleteRef(input:$input) { clientMutationId } }";
        self.graphql_at::<Value>(
            query,
            json!({"input":{"refId":reference}}),
            "/data/deleteRef",
        )?;
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
