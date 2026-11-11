use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::json;

use crate::{GitHubIssue, IssueInventory, PullRequestClient, PullRequestError};

const ISSUE_FIELDS: &str = "id number title body url state author { login }
    createdAt updatedAt closedAt labels(first:100) { nodes { name } totalCount }";

/// Uses the same signed-in gh transport and long-path repository discovery as
/// PRs. No credential, shell, or provider-specific path is introduced here.
pub struct IssueClient {
    pub(crate) transport: PullRequestClient,
    pub(crate) directory: PathBuf,
}

#[derive(Deserialize)]
struct IssueNode {
    #[serde(flatten)]
    issue: GitHubIssue,
    labels: Labels,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Labels {
    nodes: Vec<Label>,
    total_count: usize,
}

#[derive(Deserialize)]
struct Label {
    name: String,
}

impl IssueClient {
    pub fn has_origin(directory: &Path) -> Result<bool, PullRequestError> {
        let repository = gix::discover(directory)
            .map_err(|error| PullRequestError::Invalid(error.to_string()))?;
        Ok(repository.find_remote("origin").is_ok())
    }
    /// See `PullRequestClient::is_current`.
    pub fn is_current(&self, directory: &Path) -> bool {
        self.transport.is_current(directory)
    }

    pub fn repository(&self) -> &str {
        &self.transport.repository
    }

    pub(crate) fn open_issues(&self) -> Result<Vec<GitHubIssue>, PullRequestError> {
        let query = format!(
            "query($owner:String!,$name:String!,$cursor:String) {{ repository(owner:$owner,name:$name) {{ issues(first:100,after:$cursor,states:OPEN,orderBy:{{field:CREATED_AT,direction:ASC}}) {{ nodes {{ {ISSUE_FIELDS} }} pageInfo {{ hasNextPage endCursor }} }} }} }}"
        );
        let issues: Vec<IssueNode> = self.transport.connection(
            &query,
            self.transport.variables(0),
            "/data/repository/issues",
        )?;
        issues
            .into_iter()
            .map(|issue| self.decode_issue(issue))
            .collect()
    }

    pub(crate) fn read_issue(&self, number: u32) -> Result<GitHubIssue, PullRequestError> {
        let query = format!(
            "query($owner:String!,$name:String!,$number:Int!) {{ repository(owner:$owner,name:$name) {{ issue(number:$number) {{ {ISSUE_FIELDS} }} }} }}"
        );
        let issue = self.transport.graphql_at(
            &query,
            self.transport.variables(number),
            "/data/repository/issue",
        )?;
        self.decode_issue(issue)
    }

    #[track_caller]
    pub fn discover(directory: &Path) -> Result<Self, PullRequestError> {
        Ok(Self {
            transport: PullRequestClient::discover(directory)?,
            directory: std::fs::canonicalize(directory)?,
        })
    }

    /// GraphQL's issues connection excludes PRs and has no search-index delay or
    /// 1,000-result search cap. Every open and closed page is included.
    pub fn inventory(&self) -> Result<IssueInventory, PullRequestError> {
        let query = format!(
            "query($owner:String!,$name:String!,$cursor:String) {{
              repository(owner:$owner,name:$name) {{ issues(first:100,after:$cursor,
                states:[OPEN,CLOSED],orderBy:{{field:CREATED_AT,direction:ASC}}) {{
                  nodes {{ {ISSUE_FIELDS} }} pageInfo {{ hasNextPage endCursor }} }} }} }}"
        );
        let issues: Vec<IssueNode> = self.transport.connection(
            &query,
            self.transport.variables(0),
            "/data/repository/issues",
        )?;
        let issues = issues
            .into_iter()
            .map(|issue| self.decode_issue(issue))
            .collect::<Result<_, _>>()?;
        IssueInventory::new(self.transport.repository.clone(), issues)
    }

    pub(crate) fn label_identifiers(&self) -> Result<BTreeMap<String, String>, PullRequestError> {
        #[derive(Deserialize)]
        struct RepositoryLabel {
            name: String,
            id: String,
        }
        let labels: Vec<RepositoryLabel> = self.transport.connection(
            "query($owner:String!,$name:String!,$cursor:String) {
              repository(owner:$owner,name:$name) { labels(first:100,after:$cursor) {
                nodes { name id } pageInfo { hasNextPage endCursor } } } }",
            self.transport.variables(0),
            "/data/repository/labels",
        )?;
        Ok(labels
            .into_iter()
            .map(|label| (label.name, label.id))
            .collect())
    }

    pub(crate) fn create(
        &self,
        title: &str,
        body: &str,
        labels: &[String],
    ) -> Result<GitHubIssue, PullRequestError> {
        let repository: serde_json::Value = self.transport.graphql_at(
            "query($owner:String!,$name:String!) { repository(owner:$owner,name:$name) { id } }",
            self.transport.variables(0),
            "/data/repository/id",
        )?;
        let issue: IssueNode = self.transport.graphql_at(
            &format!("mutation($input:CreateIssueInput!) {{ createIssue(input:$input) {{ issue {{ {ISSUE_FIELDS} }} }} }}"),
            json!({"input":{"repositoryId":repository,"title":title,"body":body,"labelIds":labels}}),
            "/data/createIssue/issue",
        )?;
        self.decode_issue(issue)
    }

    fn decode_issue(&self, mut node: IssueNode) -> Result<GitHubIssue, PullRequestError> {
        if node.labels.total_count != node.labels.nodes.len() {
            let mut variables = self.transport.variables(node.issue.number);
            variables["cursor"] = serde_json::Value::Null;
            node.labels.nodes = self.transport.connection(
                "query($owner:String!,$name:String!,$number:Int!,$cursor:String) {
                  repository(owner:$owner,name:$name) { issue(number:$number) {
                    labels(first:100,after:$cursor) { nodes { name }
                      pageInfo { hasNextPage endCursor } } } } }",
                variables,
                "/data/repository/issue/labels",
            )?;
        }
        node.issue.labels = node
            .labels
            .nodes
            .into_iter()
            .map(|label| label.name)
            .collect();
        node.issue.labels.sort();
        node.issue.scout = scout_attribution(&node.issue.body);
        Ok(node.issue)
    }
}

pub(crate) fn scout_attribution(body: &str) -> Option<String> {
    let nick = body
        .trim_end()
        .lines()
        .next_back()?
        .strip_prefix("<!-- velocity-scout:")?
        .strip_suffix(" -->")?;
    valid_scout_nick(nick).then(|| nick.to_owned())
}

pub(crate) fn valid_scout_nick(nick: &str) -> bool {
    nick.strip_prefix("scout-").is_some_and(|number| {
        !number.is_empty()
            && !number.starts_with('0')
            && number.bytes().all(|byte| byte.is_ascii_digit())
    }) && nick.len() <= 16
}
