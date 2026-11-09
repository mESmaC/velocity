use serde_json::{Value, json};
use std::collections::BTreeMap;

use crate::pull_request_queries::{COMMENT_FIELDS, REVIEWS, THREADS};
use crate::{
    PullRequestClient, PullRequestComment, PullRequestError, PullRequestReview,
    PullRequestReviewThread, PullRequestSnapshot, PullRequestState,
};

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
        let mut threads: Vec<PullRequestReviewThread> =
            self.pull_request_connection(number, "reviewThreads", THREADS)?;
        for thread in &mut threads {
            let query = format!(
                "query($thread:ID!,$cursor:String) {{ node(id:$thread) {{
              ... on PullRequestReviewThread {{ comments(first:100,after:$cursor) {{
                nodes {{ {COMMENT_FIELDS} }} pageInfo {{ hasNextPage endCursor }} }} }} }} }}"
            );
            thread.comments =
                self.connection(&query, json!({"thread":thread.id}), "/data/node/comments")?;
        }
        Ok(threads)
    }

    fn pull_request_connection<T: serde::de::DeserializeOwned>(
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
    ) -> Result<(), PullRequestError> {
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
        // F7 makes GitHub CI advisory. The duty runner validates the local
        // merge result; UNSTABLE must not turn a red GitHub run into a gate.
        if snapshot
            .review_threads
            .iter()
            .any(|thread| !thread.is_resolved)
        {
            return Err(PullRequestError::Invalid(
                "Resolve all review threads before merging".into(),
            ));
        }
        let viewer: Value = self.command.read(&["api", "user"])?;
        let login = viewer
            .get("login")
            .and_then(Value::as_str)
            .ok_or_else(|| PullRequestError::Missing("signed-in GitHub login".into()))?;
        require_approval(snapshot, login)
    }
}

fn require_approval(snapshot: &PullRequestSnapshot, login: &str) -> Result<(), PullRequestError> {
    let mut decisions = BTreeMap::new();
    for review in &snapshot.reviews {
        if matches!(
            review.state.as_str(),
            "APPROVED" | "CHANGES_REQUESTED" | "DISMISSED"
        ) && let Some(author) = &review.author
        {
            decisions.insert(author.login.to_ascii_lowercase(), review);
        }
    }
    // A clean master-update merge preserves the approving review (F7). The
    // duty runner re-reviews author changes and conflict resolutions; this
    // transport checks the recorded decision without demanding a duplicate
    // approval merely because master was merged into the branch.
    let approved = decisions
        .get(&login.to_ascii_lowercase())
        .is_some_and(|review| review.state == "APPROVED");
    let author = snapshot
        .pull_request
        .author
        .as_ref()
        .is_some_and(|author| author.login.eq_ignore_ascii_case(login));
    if !approved
        || author
        || decisions
            .values()
            .any(|review| review.state == "CHANGES_REQUESTED")
        || snapshot.pull_request.review_decision.as_deref() == Some("CHANGES_REQUESTED")
    {
        return Err(PullRequestError::Invalid(
            "Only a reviewer who approved this PR may merge; requested changes must be cleared"
                .into(),
        ));
    }
    Ok(())
}
