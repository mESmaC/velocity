use serde_json::{Value, json};

use crate::pull_request_queries::{COMMENT_FIELDS, REVIEWS, THREADS};
use crate::pull_requests::Connection;
use crate::{
    PullRequestClient, PullRequestComment, PullRequestError, PullRequestReview,
    PullRequestReviewThread, PullRequestSnapshot, PullRequestState,
};

#[derive(serde::Deserialize)]
struct ThreadPage {
    #[serde(flatten)]
    thread: PullRequestReviewThread,
    comments: Connection<PullRequestComment>,
}

impl PullRequestClient {
    pub(crate) fn reviews(&self, number: u32) -> Result<Vec<PullRequestReview>, PullRequestError> {
        self.pull_request_connection(number, "reviews", REVIEWS)
    }

    pub(crate) fn comments(
        &self,
        number: u32,
    ) -> Result<Vec<PullRequestComment>, PullRequestError> {
        self.pull_request_connection(number, "comments", COMMENT_FIELDS)
    }

    pub(crate) fn review_threads(
        &self,
        number: u32,
    ) -> Result<Vec<PullRequestReviewThread>, PullRequestError> {
        let fields = format!(
            "{THREADS} comments(first:100) {{ nodes {{ {COMMENT_FIELDS} }}
            pageInfo {{ hasNextPage endCursor }} }}"
        );
        let pages: Vec<ThreadPage> =
            self.pull_request_connection(number, "reviewThreads", &fields)?;
        let query = format!(
            "query($thread:ID!,$cursor:String) {{ node(id:$thread) {{
              ... on PullRequestReviewThread {{ comments(first:100,after:$cursor) {{
                nodes {{ {COMMENT_FIELDS} }} pageInfo {{ hasNextPage endCursor }} }} }} }} }}"
        );
        let mut threads = Vec::with_capacity(pages.len());
        for mut page in pages {
            // Only threads with more than one page need another gh process.
            page.thread.comments = self.complete_connection(
                &query,
                json!({"thread":page.thread.id}),
                "/data/node/comments",
                page.comments,
            )?;
            threads.push(page.thread);
        }
        Ok(threads)
    }

    pub(crate) fn pull_request_connection<T: serde::de::DeserializeOwned>(
        &self,
        number: u32,
        field: &str,
        fields: &str,
    ) -> Result<Vec<T>, PullRequestError> {
        let query = format!(
            "query($owner:String!,$name:String!,$number:Int!,$cursor:String) {{
          repository(owner:$owner,name:$name) {{ pullRequest(number:$number) {{
            {field}(first:100,after:$cursor) {{ nodes {{ {fields} }}
              pageInfo {{ hasNextPage endCursor }} }} }} }} }}"
        );
        self.connection(
            &query,
            self.variables(number),
            &format!("/data/repository/pullRequest/{field}"),
        )
    }

    pub(crate) fn require_thread(
        &self,
        number: u32,
        identifier: &str,
    ) -> Result<(), PullRequestError> {
        let threads: Vec<PullRequestReviewThread> =
            self.pull_request_connection(number, "reviewThreads", THREADS)?;
        if threads.iter().any(|thread| thread.id == identifier) {
            return Ok(());
        }
        Err(PullRequestError::Invalid(
            "Review thread does not belong to this PR".into(),
        ))
    }

    pub(crate) fn require_merge(
        &self,
        snapshot: &PullRequestSnapshot,
        head: &str,
    ) -> Result<String, PullRequestError> {
        let pull_request = &snapshot.pull_request;
        if pull_request.state != PullRequestState::Open
            || pull_request.is_draft
            || pull_request.head_ref_oid != head
            || pull_request.merge_state_status == "DIRTY"
        {
            return Err(PullRequestError::Invalid(
                "Merge requires an open, ready, conflict-free PR at the reviewed head".into(),
            ));
        }
        // GitHub's checks gate a merge where Velocity lands the head (merge_gate.rs), not
        // here: a reviewer that ran the local checks merges past a red or pending run (F7).
        if snapshot
            .review_threads
            .iter()
            .any(|thread| !thread.is_resolved)
        {
            return Err(PullRequestError::Invalid(
                "Resolve all review threads before merging".into(),
            ));
        }
        let record = self.review_record.as_ref().ok_or_else(missing_record)?;
        record.approver(pull_request.number, head)
    }

    pub(crate) fn viewer_login(&self) -> Result<String, PullRequestError> {
        let viewer: Value = self.command.read(&["api", "user"])?;
        viewer
            .get("login")
            .and_then(Value::as_str)
            .map(Into::into)
            .ok_or_else(|| PullRequestError::Missing("signed-in GitHub login".into()))
    }
}

pub(crate) fn missing_record() -> PullRequestError {
    PullRequestError::Invalid(
        "Approve, request changes and merge need Velocity's approval record (F7)".into(),
    )
}
