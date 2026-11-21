//! GitHub issue dependencies as the one way an issue waits on another (#511). Filing and
//! commenting tools link blockers here, in any repository, and the retired
//! `Held for dependencies` note with its `needs-owner` label is converted into links.

use serde::Deserialize;
use serde_json::{Value, json};
use velocity_core::issue_dependencies::{DependencyHold, IssueReference};

use crate::repository_client::Connection;
use crate::{GitHubCommandError, GitHubIssue, IssueClient, RepositoryName};

/// Retired label stripped by `convert_dependency_hold` when migrating legacy hold notes (#511).
const NEEDS_OWNER_LABEL: &str = "needs-owner";

const LINKED_FIELDS: &str = "nodes { number repository { nameWithOwner } }
    pageInfo { hasNextPage endCursor }";

#[derive(Deserialize)]
struct IssueId {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Dependent {
    id: String,
    blocked_by: Connection<Option<LinkedBlocker>>,
}

#[derive(Deserialize)]
struct LinkedBlocker {
    number: u32,
    repository: LinkedRepository,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LinkedRepository {
    name_with_owner: String,
}

/// A blocker GitHub confirmed, ready to link.
pub(crate) struct ResolvedBlocker {
    /// `#12` in this repository and `owner/name#12` elsewhere.
    pub(crate) name: String,
    id: String,
}

/// `Waits for #12, owner/name#40 …`, the line a comment gains for the links it made.
pub(crate) fn waits_line(linked: &[String]) -> String {
    if linked.is_empty() {
        return String::new();
    }
    format!(
        "\n\nWaits for {}, recorded as GitHub issue dependencies.",
        linked.join(", ")
    )
}

impl IssueClient {
    /// Makes `issue` wait on each blocker through a GitHub issue dependency. A blocker
    /// can live in another repository. Links that already exist are kept, so a retry is
    /// safe. Returns every blocker as `GitHubIssue::open_blockers` names it.
    pub fn link_blockers(
        &self,
        issue: u32,
        blockers: &[IssueReference],
    ) -> Result<Vec<String>, GitHubCommandError> {
        if blockers.is_empty() {
            return Ok(Vec::new());
        }
        let home = self.repository();
        if issue == 0
            || blockers
                .iter()
                .any(|blocker| blocker.is_issue(home, home, issue))
        {
            return Err(GitHubCommandError::Invalid(
                "Use a positive issue number and blockers other than the issue itself".into(),
            ));
        }
        let (subject, linked) = self.linked_blockers(issue)?;
        let missing: Vec<_> = blockers
            .iter()
            .filter(|blocker| {
                !linked
                    .iter()
                    .any(|(repository, number)| blocker.is_issue(home, repository, *number))
            })
            .cloned()
            .collect();
        self.add_blocked_by(&subject, &self.resolve_blockers(&missing)?)?;
        if !missing.is_empty() {
            crate::issue_cache::invalidate(self.repository());
        }
        Ok(blockers
            .iter()
            .map(|blocker| blocker.relative_to(home))
            .collect())
    }

    /// The issue's GitHub ID and every issue it already waits on, open or closed.
    fn linked_blockers(
        &self,
        issue: u32,
    ) -> Result<(String, Vec<(String, u32)>), GitHubCommandError> {
        let dependent: Dependent = self.transport.graphql_at(
            &format!(
                "query($owner:String!,$name:String!,$number:Int!) {{
                  repository(owner:$owner,name:$name) {{ issue(number:$number) {{
                    id blockedBy(first:100) {{ {LINKED_FIELDS} }} }} }} }}"
            ),
            self.transport.variables(issue),
            "/data/repository/issue",
        )?;
        let linked = self.transport.complete_connection(
            &format!(
                "query($owner:String!,$name:String!,$number:Int!,$cursor:String) {{
                  repository(owner:$owner,name:$name) {{ issue(number:$number) {{
                    blockedBy(first:100,after:$cursor) {{ {LINKED_FIELDS} }} }} }} }}"
            ),
            self.transport.variables(issue),
            "/data/repository/issue/blockedBy",
            dependent.blocked_by,
        )?;
        let linked = linked
            .into_iter()
            .flatten()
            .map(|blocker| (blocker.repository.name_with_owner, blocker.number))
            .collect();
        Ok((dependent.id, linked))
    }

    /// Reads each blocker's GitHub ID, so a blocker that does not exist is refused
    /// before anything is written.
    pub(crate) fn resolve_blockers(
        &self,
        blockers: &[IssueReference],
    ) -> Result<Vec<ResolvedBlocker>, GitHubCommandError> {
        blockers
            .iter()
            .map(|blocker| {
                let home = self.repository();
                let name = blocker.relative_to(home);
                let unreadable = |error: String| {
                    GitHubCommandError::Invalid(format!("Could not read blocker {name}: {error}"))
                };
                let repository = RepositoryName::parse(blocker.repository_or(home))
                    .map_err(|error| unreadable(error.to_string()))?;
                let found: IssueId = self
                    .transport
                    .graphql_at(
                        "query($owner:String!,$name:String!,$number:Int!) {
                          repository(owner:$owner,name:$name) { issue(number:$number) { id } } }",
                        json!({"owner": repository.owner(), "name": repository.name(),
                            "number": blocker.number()}),
                        "/data/repository/issue",
                    )
                    .map_err(|error| unreadable(error.to_string()))?;
                Ok(ResolvedBlocker { name, id: found.id })
            })
            .collect()
    }

    /// Links resolved blockers to the issue with GitHub ID `subject`.
    pub(crate) fn add_blocked_by(
        &self,
        subject: &str,
        blockers: &[ResolvedBlocker],
    ) -> Result<(), GitHubCommandError> {
        for blocker in blockers {
            let _: IssueId = self.transport.graphql_at(
                "mutation($input:AddBlockedByInput!) { addBlockedBy(input:$input) { issue { id } } }",
                json!({"input": {"issueId": subject, "blockingIssueId": blocker.id}}),
                "/data/addBlockedBy/issue",
            )?;
        }
        Ok(())
    }

    /// Whether an open issue still carries the retired hold: a `Held for dependencies`
    /// body line that names its blockers. Reads nothing.
    pub fn has_dependency_hold(issue: &GitHubIssue) -> bool {
        DependencyHold::find(&issue.body).is_some()
    }

    /// Converts the retired hold on `number` into GitHub links (#511) and returns the
    /// blockers, or `None` when the issue no longer carries the note. Links go on first,
    /// the `needs-owner` label comes off second and the note is rewritten last: a failure
    /// in between leaves the issue held, and the next call finishes the rest.
    pub fn convert_dependency_hold(
        &self,
        number: u32,
    ) -> Result<Option<Vec<String>>, GitHubCommandError> {
        // Read again: the caller's copy may predate an edit, and the body is replaced whole.
        let issue = self.read_issue(number)?;
        let Some(hold) = DependencyHold::find(&issue.body) else {
            return Ok(None);
        };
        let home = self.repository();
        // A note that names its own issue cannot wait on it.
        let blockers: Vec<_> = hold
            .blockers
            .iter()
            .filter(|blocker| !blocker.is_issue(home, home, number))
            .cloned()
            .collect();
        if blockers.is_empty() {
            return Err(GitHubCommandError::Invalid(
                "The hold note names only its own issue".into(),
            ));
        }
        let linked = self.link_blockers(number, &blockers)?;
        if issue
            .labels
            .iter()
            .any(|label| label.eq_ignore_ascii_case(NEEDS_OWNER_LABEL))
        {
            self.remove_issue_label(number, NEEDS_OWNER_LABEL)?;
        }
        let _: Value = self.transport.graphql_at(
            "mutation($input:UpdateIssueInput!) { updateIssue(input:$input) { issue { id } } }",
            json!({"input": {"id": issue.id, "body": hold.rewritten(&issue.body)}}),
            "/data/updateIssue/issue",
        )?;
        crate::issue_cache::invalidate(self.repository());
        Ok(Some(linked))
    }
}
