use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

use crate::commit_cache::{self, CommitStatusEntry};
use crate::issue_cache::{self, OPEN_ISSUES_REFRESH};
use crate::issue_types::IssueState;
use crate::repository_client::{Connection, RepositoryClient};
use crate::rosen_issue::{INTEGRATION_CONTEXT, unix_seconds};
use crate::{GitHubCommandError, GitHubIssue, IssueInventory};

pub(crate) const ISSUE_FIELDS: &str = "id number title body url state author { login }
    createdAt updatedAt closedAt milestone { number } labels(first:100) { nodes { name } totalCount }
    timelineItems(last:1,itemTypes:[REOPENED_EVENT]) { nodes { ... on ReopenedEvent { createdAt } } }";
pub(crate) const BLOCKER_FIELDS: &str = "number state repository { nameWithOwner }";
/// How many commits of the default branch history are bundled with the open-issues query
/// so integration status checks need no separate ref or commit-status polling (#556).
pub(crate) const BRANCH_HISTORY_COUNT: usize = 20;

/// Reads and writes one repository's issues through the signed-in GitHub transport, with
/// long-path repository discovery.
pub struct IssueClient {
    pub(crate) transport: RepositoryClient,
    pub(crate) directory: PathBuf,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct IssueNode {
    #[serde(flatten)]
    pub(crate) issue: GitHubIssue,
    pub(crate) labels: Labels,
    #[serde(default)]
    pub(crate) timeline_items: Option<ReopenTimeline>,
}

#[derive(Deserialize)]
pub(crate) struct ReopenTimeline {
    #[serde(default)]
    nodes: Vec<Option<ReopenNode>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReopenNode {
    #[serde(default)]
    created_at: Option<String>,
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
struct CombinedOpenIssues {
    issues: Connection<IssueWithDependencies>,
    #[serde(default)]
    default_branch_ref: Option<DefaultBranchRefNode>,
}

#[derive(Deserialize)]
pub(crate) struct DefaultBranchRefNode {
    pub(crate) name: String,
    pub(crate) target: Option<BranchTargetCommit>,
}

#[derive(Deserialize)]
pub(crate) struct BranchTargetCommit {
    #[serde(default)]
    pub(crate) oid: Option<String>,
    #[serde(default)]
    pub(crate) history: Option<CommitHistoryNodes>,
}

#[derive(Deserialize)]
pub(crate) struct CommitHistoryNodes {
    #[serde(default)]
    pub(crate) nodes: Vec<Option<HistoryCommitNode>>,
}

#[derive(Deserialize)]
pub(crate) struct HistoryCommitNode {
    pub(crate) oid: String,
    #[serde(default)]
    pub(crate) status: Option<CommitStatusNode>,
}

#[derive(Deserialize)]
pub(crate) struct CommitStatusNode {
    #[serde(default)]
    pub(crate) context: Option<StatusContextNode>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct StatusContextNode {
    pub(crate) state: String,
    #[serde(default)]
    pub(crate) created_at: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IssueDependenciesSummary {
    blocked_by: u32,
}

#[derive(Deserialize)]
pub(crate) struct Blocker {
    pub(crate) number: u32,
    pub(crate) state: IssueState,
    pub(crate) repository: BlockerRepository,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BlockerRepository {
    pub(crate) name_with_owner: String,
}

fn dependency_fields() -> String {
    format!(
        "issueDependenciesSummary {{ blockedBy }} blockedBy(first:25) {{
        nodes {{ {BLOCKER_FIELDS} }} pageInfo {{ hasNextPage endCursor }} }}"
    )
}

pub(crate) fn branch_history_selection() -> String {
    format!(
        "target {{ ... on Commit {{ oid history(first:{BRANCH_HISTORY_COUNT}) {{
          nodes {{ oid status {{ context(name:\"{INTEGRATION_CONTEXT}\") {{ state createdAt }} }} }}
        }} }} }}"
    )
}

pub(crate) fn record_branch_ref(repository: &str, branch_ref: DefaultBranchRefNode) {
    let Some(target) = branch_ref.target else {
        return;
    };
    let Some(tip) = target.oid.filter(|oid| !oid.is_empty()) else {
        return;
    };
    RepositoryClient::update_default_branch(repository, &branch_ref.name);
    let history = target
        .history
        .map(|history| {
            history
                .nodes
                .into_iter()
                .flatten()
                .map(|node| CommitStatusEntry {
                    sha: node.oid,
                    integration_green: node.status.and_then(|status| status.context).and_then(
                        |context| {
                            context
                                .state
                                .eq_ignore_ascii_case("SUCCESS")
                                .then(|| context.created_at.as_deref().and_then(unix_seconds))
                        },
                    ),
                })
                .collect()
        })
        .unwrap_or_default();
    commit_cache::record_branch_snapshot(repository, tip, history);
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Labels {
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

    /// Invalidates the shared open-issues cache for `repository` after a write.
    pub fn invalidate_open_issues(repository: &str) {
        issue_cache::invalidate(repository);
    }

    /// Monotonic version of the shared open-issues cache for `repository`, if loaded.
    pub fn open_issues_version(repository: &str) -> Option<u64> {
        issue_cache::version(repository)
    }

    /// Returns the repository's open issues from the shared single-flight cache, refreshing
    /// at most once per [`OPEN_ISSUES_REFRESH`] across dispatch, the issue panel and Work (#556).
    pub fn open_issues(&self) -> Result<Vec<GitHubIssue>, GitHubCommandError> {
        self.cached_open_issues(OPEN_ISSUES_REFRESH)
    }

    /// Returns the repository's open issues from the shared single-flight cache when younger
    /// than `max_age`, otherwise fetches them once and updates the shared cache.
    pub fn cached_open_issues(
        &self,
        max_age: Duration,
    ) -> Result<Vec<GitHubIssue>, GitHubCommandError> {
        if let Some(cached) = issue_cache::get_fresh(self.repository(), max_age) {
            return Ok(cached);
        }
        let lock = issue_cache::fetch_lock(self.repository());
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cached) = issue_cache::get_fresh(self.repository(), max_age) {
            return Ok(cached);
        }
        let issues = self.fetch_open_issues()?;
        issue_cache::store(self.repository(), &issues);
        Ok(issues)
    }

    /// Forces an immediate read of open issues and refreshes the shared cache (for example,
    /// right before duplicate-checking a newly filed issue).
    pub(crate) fn open_issues_fresh(&self) -> Result<Vec<GitHubIssue>, GitHubCommandError> {
        let lock = issue_cache::fetch_lock(self.repository());
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let issues = self.fetch_open_issues()?;
        issue_cache::store(self.repository(), &issues);
        Ok(issues)
    }

    fn fetch_open_issues(&self) -> Result<Vec<GitHubIssue>, GitHubCommandError> {
        let page_query = format!(
            "query($owner:String!,$name:String!,$cursor:String) {{ repository(owner:$owner,name:$name) {{ issues(first:100,after:$cursor,states:OPEN,orderBy:{{field:CREATED_AT,direction:ASC}}) {{ nodes {{ {ISSUE_FIELDS} {} }} pageInfo {{ hasNextPage endCursor }} }} }} }}",
            dependency_fields(),
        );
        let combined_query = format!(
            "query($owner:String!,$name:String!,$cursor:String) {{ repository(owner:$owner,name:$name) {{ defaultBranchRef {{ name {} }} issues(first:100,after:$cursor,states:OPEN,orderBy:{{field:CREATED_AT,direction:ASC}}) {{ nodes {{ {ISSUE_FIELDS} {} }} pageInfo {{ hasNextPage endCursor }} }} }} }}",
            branch_history_selection(),
            dependency_fields(),
        );
        let variables = self.transport.variables(0);
        let first_page = match self.transport.graphql_at::<CombinedOpenIssues>(
            &combined_query,
            variables.clone(),
            "/data/repository",
        ) {
            Ok(combined) => {
                if let Some(branch_ref) = combined.default_branch_ref {
                    record_branch_ref(self.repository(), branch_ref);
                }
                combined.issues
            }
            Err(GitHubCommandError::Invalid(message))
                if message.contains("GitHub GraphQL errors") =>
            {
                self.transport.graphql_at(
                    &page_query,
                    variables.clone(),
                    "/data/repository/issues",
                )?
            }
            Err(error) => return Err(error),
        };
        let issues = self.transport.complete_connection(
            &page_query,
            variables,
            "/data/repository/issues",
            first_page,
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
        let decoded = self.decode_issue_with_dependencies(issue)?;
        issue_cache::upsert_issue(self.repository(), &decoded);
        Ok(decoded)
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
        let created = self.decode_issue(issue)?;
        issue_cache::invalidate(self.repository());
        Ok(created)
    }

    pub(crate) fn decode_issue(&self, node: IssueNode) -> Result<GitHubIssue, GitHubCommandError> {
        self.decode_issue_for(self.transport.owner(), self.transport.name(), node)
    }

    pub(crate) fn decode_issue_for(
        &self,
        owner: &str,
        name: &str,
        mut node: IssueNode,
    ) -> Result<GitHubIssue, GitHubCommandError> {
        if node.labels.total_count != node.labels.nodes.len() {
            let mut variables = self.transport.variables_for(owner, name, node.issue.number);
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
        node.issue.reopened_at = node.timeline_items.and_then(|timeline| {
            timeline
                .nodes
                .into_iter()
                .flatten()
                .rev()
                .find_map(|event| event.created_at)
        });
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
        let mut open_blockers = Vec::new();
        let mut closed_blockers = Vec::new();
        for blocker in blockers.into_iter().flatten() {
            // `DependencyGraph` reads this form back.
            let name = relative_blocker_name(
                self.repository(),
                &blocker.repository.name_with_owner,
                blocker.number,
            );
            match blocker.state {
                IssueState::Open => open_blockers.push(name),
                IssueState::Closed => closed_blockers.push(name),
            }
        }
        issue.open_blockers = open_blockers;
        issue.closed_blockers = closed_blockers;
        Ok(issue)
    }
}

pub(crate) fn relative_blocker_name(home: &str, repository: &str, number: u32) -> String {
    if repository.eq_ignore_ascii_case(home) {
        format!("#{number}")
    } else {
        format!("{repository}#{number}")
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
