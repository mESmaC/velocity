use serde_json::json;

use crate::pull_request_queries::{COMMENT_FIELDS, REVIEWS, THREADS};
use crate::pull_requests::Connection;
use crate::{
    PullRequestClient, PullRequestComment, PullRequestError, PullRequestReview,
    PullRequestReviewThread,
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
}
