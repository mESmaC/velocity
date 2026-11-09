use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::path::Path;

use crate::pull_request_command::PullRequestCommand;
use crate::pull_request_queries::{CHECKS, FIELDS};
use crate::{PullRequest, PullRequestCheck, PullRequestError, PullRequestSnapshot};

pub struct PullRequestClient {
    pub(crate) command: PullRequestCommand,
    pub(crate) repository: String,
    owner: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepositoryIdentity {
    name_with_owner: String,
}

#[derive(Deserialize)]
struct Connection<T> {
    nodes: Vec<T>,
    #[serde(rename = "pageInfo")]
    page: PageInfo,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

impl PullRequestClient {
    pub fn discover(directory: &Path) -> Result<Self, PullRequestError> {
        let (command, repository) = PullRequestCommand::new(directory)?;
        let identity: RepositoryIdentity =
            command.read(&["repo", "view", "--json", "nameWithOwner", "--", &repository])?;
        let (owner, name) = identity
            .name_with_owner
            .split_once('/')
            .ok_or_else(|| PullRequestError::Invalid("Invalid GitHub repository name".into()))?;
        if [owner, name].iter().any(|part| {
            part.is_empty()
                || !part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
        }) {
            return Err(PullRequestError::Invalid(
                "Invalid GitHub repository name".into(),
            ));
        }
        Ok(Self {
            owner: owner.into(),
            name: name.into(),
            repository: identity.name_with_owner,
            command,
        })
    }

    pub fn list(&self) -> Result<Vec<PullRequest>, PullRequestError> {
        let query = format!(
            "query($owner:String!,$name:String!,$cursor:String) {{
          repository(owner:$owner,name:$name) {{ pullRequests(states:OPEN,first:100,after:$cursor,
            orderBy:{{field:UPDATED_AT,direction:DESC}}) {{ nodes {{ {FIELDS} }}
              pageInfo {{ hasNextPage endCursor }} }} }} }}"
        );
        self.connection(&query, self.variables(0), "/data/repository/pullRequests")
    }

    pub fn get(&self, number: u32) -> Result<PullRequest, PullRequestError> {
        let query = format!(
            "query($owner:String!,$name:String!,$number:Int!) {{
          repository(owner:$owner,name:$name) {{ pullRequest(number:$number) {{ {FIELDS} }} }} }}"
        );
        self.graphql_at(
            &query,
            self.variables(number),
            "/data/repository/pullRequest",
        )
    }

    pub fn snapshot(&self, number: u32) -> Result<PullRequestSnapshot, PullRequestError> {
        let pull_request = self.get(number)?;
        let snapshot = PullRequestSnapshot {
            checks: self.checks(number)?,
            reviews: self.reviews(number)?,
            comments: self.comments(number)?,
            review_threads: self.review_threads(number)?,
            pull_request,
        };
        // Never combine reviews/checks for a new push with an older head identity.
        if self.get(number)?.head_ref_oid != snapshot.pull_request.head_ref_oid {
            return Err(PullRequestError::Invalid(
                "PR head changed while reading; refresh it".into(),
            ));
        }
        Ok(snapshot)
    }

    pub(crate) fn variables(&self, number: u32) -> Value {
        json!({"owner":self.owner,"name":self.name,"number":number})
    }

    pub(crate) fn graphql_at<T: DeserializeOwned>(
        &self,
        query: &str,
        variables: Value,
        path: &str,
    ) -> Result<T, PullRequestError> {
        let response: Value = self.command.api(
            "POST",
            "graphql",
            &json!({"query":query,"variables":variables}),
        )?;
        if let Some(errors) = response.get("errors") {
            return Err(PullRequestError::Invalid(format!(
                "GitHub GraphQL errors: {errors}"
            )));
        }
        let value = response
            .pointer(path)
            .filter(|value| !value.is_null())
            .ok_or_else(|| PullRequestError::Missing(path.into()))?;
        Ok(serde_json::from_value(value.clone())?)
    }

    pub(crate) fn connection<T: DeserializeOwned>(
        &self,
        query: &str,
        mut variables: Value,
        path: &str,
    ) -> Result<Vec<T>, PullRequestError> {
        let mut nodes = Vec::new();
        loop {
            let page: Connection<T> = self.graphql_at(query, variables.clone(), path)?;
            nodes.extend(page.nodes);
            if !page.page.has_next_page {
                return Ok(nodes);
            }
            let cursor = page
                .page
                .end_cursor
                .filter(|cursor| !cursor.is_empty())
                .ok_or_else(|| PullRequestError::Missing("pagination cursor".into()))?;
            if variables.get("cursor").and_then(Value::as_str) == Some(&cursor) {
                return Err(PullRequestError::Invalid(
                    "GitHub repeated a pagination cursor".into(),
                ));
            }
            variables["cursor"] = json!(cursor);
        }
    }

    fn checks(&self, number: u32) -> Result<Vec<PullRequestCheck>, PullRequestError> {
        let path = "/data/repository/pullRequest/commits/nodes/0/commit/statusCheckRollup";
        let rollup: Option<Value> = self
            .graphql_at::<Value>(
                CHECKS,
                self.variables(number),
                "/data/repository/pullRequest/commits/nodes/0/commit",
            )?
            .get("statusCheckRollup")
            .cloned()
            .filter(|value| !value.is_null());
        if rollup.is_none() {
            return Ok(Vec::new());
        }
        self.connection(CHECKS, self.variables(number), &format!("{path}/contexts"))
    }
}
