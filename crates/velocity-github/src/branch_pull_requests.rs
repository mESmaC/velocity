use crate::pull_request_queries::FIELDS;
use crate::{PullRequest, PullRequestClient, PullRequestError};
use serde_json::json;

impl PullRequestClient {
    pub fn owns_branch(&self, request: &PullRequest, branch: &str) -> bool {
        request.head_ref_name == branch
            && request.head_repository.as_ref().is_some_and(|repository| {
                repository
                    .name_with_owner
                    .eq_ignore_ascii_case(&self.repository)
            })
    }
    /// Includes closed and merged PRs: an agent can create and close its PR
    /// between observations. Forks with the same branch name cannot own it.
    pub fn for_branch(&self, branch: &str) -> Result<Vec<PullRequest>, PullRequestError> {
        let query = format!(
            "query($owner:String!,$name:String!,$branch:String!,$cursor:String) {{
              repository(owner:$owner,name:$name) {{ pullRequests(headRefName:$branch,
                first:100,after:$cursor,orderBy:{{field:CREATED_AT,direction:DESC}}) {{
                  nodes {{ {FIELDS} }} pageInfo {{ hasNextPage endCursor }} }} }} }}"
        );
        let mut variables = self.variables(0);
        variables["branch"] = json!(branch);
        let requests: Vec<PullRequest> =
            self.connection(&query, variables, "/data/repository/pullRequests")?;
        Ok(requests
            .into_iter()
            .filter(|request| self.owns_branch(request, branch))
            .collect())
    }
}
