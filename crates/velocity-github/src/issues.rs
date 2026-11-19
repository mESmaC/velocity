use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::json;

use crate::issue_types::IssueState;
use crate::repository_client::{Connection, RepositoryClient};
use crate::{GitHubCommandError, GitHubIssue, IssueInventory};

const ISSUE_FIELDS: &str = "id number title body url state author { login }
    createdAt updatedAt closedAt milestone { number } labels(first:100) { nodes { name } totalCount }";
const BLOCKER_FIELDS: &str = "number state repository { nameWithOwner }";

/// Reads and writes one repository's issues through the signed-in gh transport, with
/// long-path repository discovery. No credential, shell, or provider-specific path is
/// introduced here.
pub struct IssueClient {
    pub(crate) transport: RepositoryClient,
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
struct IssueWithDependencies {
    #[serde(flatten)]
    node: IssueNode,
    issue_dependencies_summary: IssueDependenciesSummary,
    blocked_by: Connection<Option<Blocker>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IssueDependenciesSummary {
    blocked_by: u32,
}

#[derive(Deserialize)]
struct Blocker {
    number: u32,
    state: IssueState,
    repository: BlockerRepository,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BlockerRepository {
    name_with_owner: String,
}

fn dependency_fields() -> String {
    format!(
        "issueDependenciesSummary {{ blockedBy }} blockedBy(first:10) {{
        nodes {{ {BLOCKER_FIELDS} }} pageInfo {{ hasNextPage endCursor }} }}"
    )
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
    pub fn has_origin(directory: &Path) -> Result<bool, GitHubCommandError> {
        let repository = gix::discover(directory)
            .map_err(|error| GitHubCommandError::Invalid(error.to_string()))?;
        Ok(repository.find_remote("origin").is_ok())
    }
    /// See `RepositoryClient::is_current`.
    pub fn is_current(&self, directory: &Path) -> bool {
        self.transport.is_current(directory)
    }

    pub fn repository(&self) -> &str {
        &self.transport.repository
    }

    pub fn open_issues(&self) -> Result<Vec<GitHubIssue>, GitHubCommandError> {
        let query = format!(
            "query($owner:String!,$name:String!,$cursor:String) {{ repository(owner:$owner,name:$name) {{ issues(first:100,after:$cursor,states:OPEN,orderBy:{{field:CREATED_AT,direction:ASC}}) {{ nodes {{ {ISSUE_FIELDS} {} }} pageInfo {{ hasNextPage endCursor }} }} }} }}",
            dependency_fields(),
        );
        let issues: Vec<IssueWithDependencies> = self.transport.connection(
            &query,
            self.transport.variables(0),
            "/data/repository/issues",
        )?;
        issues
            .into_iter()
            .map(|issue| self.decode_issue_with_dependencies(issue))
            .collect()
    }

    pub(crate) fn read_issue(&self, number: u32) -> Result<GitHubIssue, GitHubCommandError> {
        let query = format!(
            "query($owner:String!,$name:String!,$number:Int!) {{ repository(owner:$owner,name:$name) {{ issue(number:$number) {{ {ISSUE_FIELDS} {} }} }} }}",
            dependency_fields(),
        );
        let issue = self.transport.graphql_at(
            &query,
            self.transport.variables(number),
            "/data/repository/issue",
        )?;
        self.decode_issue_with_dependencies(issue)
    }

    pub(crate) fn add_blocker(&self, issue: u32, blocker: u32) -> Result<(), GitHubCommandError> {
        if issue == 0 || blocker == 0 || issue == blocker {
            return Err(GitHubCommandError::Invalid(
                "Use two distinct, positive issue numbers".into(),
            ));
        }
        #[derive(Deserialize)]
        struct IssueId {
            id: String,
        }
        #[derive(Deserialize)]
        struct DependencyIds {
            issue: Option<IssueId>,
            blocker: Option<IssueId>,
        }
        let mut variables = self.transport.variables(issue);
        variables["blocker"] = json!(blocker);
        let ids: DependencyIds = self.transport.graphql_at(
            "query($owner:String!,$name:String!,$number:Int!,$blocker:Int!) {
                repository(owner:$owner,name:$name) {
                    issue(number:$number) { id } blocker: issue(number:$blocker) { id }
                }
            }",
            variables,
            "/data/repository",
        )?;
        let issue_id = ids
            .issue
            .ok_or_else(|| GitHubCommandError::Missing(format!("issue #{issue}")))?;
        let blocker_id = ids
            .blocker
            .ok_or_else(|| GitHubCommandError::Missing(format!("issue #{blocker}")))?;
        let _: IssueId = self.transport.graphql_at(
            "mutation($input:AddBlockedByInput!) { addBlockedBy(input:$input) { issue { id } } }",
            json!({"input": {"issueId": issue_id.id, "blockingIssueId": blocker_id.id}}),
            "/data/addBlockedBy/issue",
        )?;
        Ok(())
    }

    #[track_caller]
    pub fn discover(directory: &Path) -> Result<Self, GitHubCommandError> {
        Ok(Self {
            transport: RepositoryClient::discover(directory)?,
            directory: std::fs::canonicalize(directory)?,
        })
    }

    /// A Rosen project can keep its GitHub identity without a Git checkout.
    #[track_caller]
    pub fn for_repository(directory: &Path, repository: &str) -> Result<Self, GitHubCommandError> {
        Ok(Self {
            transport: RepositoryClient::for_repository(repository)?,
            directory: std::fs::canonicalize(directory)?,
        })
    }

    /// GraphQL's issues connection excludes PRs and has no search-index delay or
    /// 1,000-result search cap. Every open and closed page is included.
    pub fn inventory(&self) -> Result<IssueInventory, GitHubCommandError> {
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

    pub(crate) fn label_identifiers(&self) -> Result<BTreeMap<String, String>, GitHubCommandError> {
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
    ) -> Result<GitHubIssue, GitHubCommandError> {
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

    fn decode_issue(&self, mut node: IssueNode) -> Result<GitHubIssue, GitHubCommandError> {
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

    fn decode_issue_with_dependencies(
        &self,
        node: IssueWithDependencies,
    ) -> Result<GitHubIssue, GitHubCommandError> {
        let mut issue = self.decode_issue(node.node)?;
        issue.open_blocker_count = node.issue_dependencies_summary.blocked_by;
        // The connection includes closed issues. Skip extra pages when the summary says none are open.
        let blockers = if issue.open_blocker_count == 0 {
            node.blocked_by.into_nodes()
        } else {
            self.transport.complete_connection(
                &format!(
                    "query($owner:String!,$name:String!,$number:Int!,$cursor:String) {{
                    repository(owner:$owner,name:$name) {{ issue(number:$number) {{
                      blockedBy(first:100,after:$cursor) {{ nodes {{ {BLOCKER_FIELDS} }}
                        pageInfo {{ hasNextPage endCursor }} }} }} }} }}"
                ),
                self.transport.variables(issue.number),
                "/data/repository/issue/blockedBy",
                node.blocked_by,
            )?
        };
        issue.open_blockers = blockers
            .into_iter()
            .flatten()
            .filter(|blocker| blocker.state == IssueState::Open)
            .map(|blocker| {
                let repository = blocker.repository.name_with_owner;
                if repository.eq_ignore_ascii_case(self.repository()) {
                    format!("#{}", blocker.number)
                } else {
                    format!("{repository}#{}", blocker.number)
                }
            })
            .collect();
        Ok(issue)
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
