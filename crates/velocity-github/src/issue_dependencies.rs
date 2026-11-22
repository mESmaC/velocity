//! GitHub issue dependencies as the one way an issue waits on another (#511). Filing and
//! commenting tools link blockers here, in any repository, and the retired
//! `Held for dependencies` note with its `needs-owner` label is converted into links.

use serde::Deserialize;
use serde_json::json;
use velocity_core::collapse::IssueHost;
use velocity_core::issue_dependencies::{DependencyHold, IssueReference, upsert_declared_blockers};

use crate::collapse_issues::CollapseIssueTransport;
use crate::issues::IssueBackend;
use crate::repository_client::{Connection, RepositoryClient};
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

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenIssueBlockerNode {
    blocked_by: BlockerRepositoryNodes,
}

#[derive(Deserialize)]
struct BlockerRepositoryNodes {
    #[serde(default)]
    nodes: Vec<Option<BlockerRepositoryEntry>>,
}

#[derive(Deserialize)]
struct BlockerRepositoryEntry {
    repository: LinkedRepository,
}

/// How a validated blocker is recorded on the subject issue (#624).
pub(crate) enum ResolvedBlockerKind {
    /// Lives on the subject's own host and can be linked as a native dependency.
    Native { id: String },
    /// Lives on another configured issue host and is recorded via a `Blocked by:` line.
    CrossHostText,
}

/// A blocker confirmed by its issue host, ready to link or record textually.
pub(crate) struct ResolvedBlocker {
    /// `#12` in this repository and `owner/name#12` elsewhere.
    pub(crate) name: String,
    pub(crate) reference: IssueReference,
    pub(crate) kind: ResolvedBlockerKind,
}

impl ResolvedBlocker {
    pub(crate) fn is_native(&self) -> bool {
        matches!(self.kind, ResolvedBlockerKind::Native { .. })
    }

    pub(crate) fn cross_host_reference(&self) -> Option<&IssueReference> {
        matches!(self.kind, ResolvedBlockerKind::CrossHostText).then_some(&self.reference)
    }
}

/// Blockers recorded on an existing issue, split between native host links and cross-host
/// `Blocked by:` lines (#624).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct LinkedBlockers {
    pub(crate) native: Vec<String>,
    pub(crate) textual: Vec<String>,
    pub(crate) textual_references: Vec<IssueReference>,
    pub(crate) ordered: Vec<String>,
}

struct CrossHostBlocker {
    host: IssueHost,
    canonical_repository: String,
    reference: IssueReference,
    name: String,
}

/// Comment footer describing blockers recorded on an issue: native links use `Waits for …`
/// and cross-host blockers use `Blocked by: owner/name#N` (#511, #624).
pub(crate) fn waits_line_for(host: IssueHost, linked: &LinkedBlockers) -> String {
    let mut suffix = String::new();
    if !linked.native.is_empty() {
        let line = match host {
            IssueHost::GitHub => format!(
                "\n\nWaits for {}, recorded as GitHub issue dependencies.",
                linked.native.join(", ")
            ),
            IssueHost::Collapse => format!(
                "\n\nWaits for {}, recorded as issue dependencies.",
                linked.native.join(", ")
            ),
        };
        suffix.push_str(&line);
    }
    if !linked.textual.is_empty() {
        suffix.push_str(&format!("\n\nBlocked by: {}", linked.textual.join(", ")));
    }
    suffix
}

impl IssueClient {
    /// Makes `issue` wait on each blocker through an issue dependency (or a validated
    /// `Blocked by:` line on the issue body when the blocker lives on another host, #624).
    /// Links that already exist are kept, so a retry is safe.
    pub fn link_blockers(
        &self,
        issue: u32,
        blockers: &[IssueReference],
    ) -> Result<Vec<String>, GitHubCommandError> {
        Ok(self.link_blockers_classified(issue, blockers)?.ordered)
    }

    /// Links same-host blockers natively and records validated cross-host blockers on the
    /// issue body as `Blocked by: owner/name#N` (#624).
    pub(crate) fn link_blockers_classified(
        &self,
        issue: u32,
        blockers: &[IssueReference],
    ) -> Result<LinkedBlockers, GitHubCommandError> {
        if blockers.is_empty() {
            return Ok(LinkedBlockers::default());
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
        let (same_host, cross_host, ordered) = self.partition_blockers(blockers);
        self.validate_cross_host_blockers(&cross_host)?;
        let textual: Vec<String> = cross_host.iter().map(|item| item.name.clone()).collect();
        let textual_references: Vec<IssueReference> = cross_host
            .iter()
            .map(|item| item.reference.clone())
            .collect();
        let native =
            self.link_same_host_blockers(issue, &same_host, textual_references.is_empty())?;
        if !textual_references.is_empty() {
            self.record_textual_blockers(issue, &textual_references)?;
        }
        Ok(LinkedBlockers {
            native,
            textual,
            textual_references,
            ordered,
        })
    }

    fn partition_blockers(
        &self,
        blockers: &[IssueReference],
    ) -> (Vec<IssueReference>, Vec<CrossHostBlocker>, Vec<String>) {
        let home = self.repository();
        let mut same_host = Vec::new();
        let mut cross_host = Vec::new();
        let mut ordered = Vec::new();
        for blocker in blockers {
            let repo = blocker.repository_or(home);
            let (target_host, canonical_repository) = self.blocker_host(repo);
            if target_host == self.host() {
                let name = blocker.relative_to(home);
                same_host.push(blocker.clone());
                ordered.push(name);
            } else {
                let canonical_ref =
                    IssueReference::new(Some(&canonical_repository), blocker.number())
                        .unwrap_or_else(|| blocker.clone());
                let name = canonical_ref.relative_to(home);
                ordered.push(name.clone());
                cross_host.push(CrossHostBlocker {
                    host: target_host,
                    canonical_repository,
                    reference: canonical_ref,
                    name,
                });
            }
        }
        (same_host, cross_host, ordered)
    }

    fn link_same_host_blockers(
        &self,
        issue: u32,
        same_host: &[IssueReference],
        refresh_collapse: bool,
    ) -> Result<Vec<String>, GitHubCommandError> {
        if same_host.is_empty() {
            return Ok(Vec::new());
        }
        let home = self.repository();
        if let IssueBackend::Collapse(collapse) = &self.backend {
            let names = collapse.validate_blockers(same_host)?;
            for blocker in same_host {
                collapse.add_dependency(issue, blocker)?;
            }
            if refresh_collapse {
                let _ = collapse.read_issue(issue);
            }
            return Ok(names);
        }
        let (subject, linked) = self.linked_blockers(issue)?;
        let missing: Vec<_> = same_host
            .iter()
            .filter(|blocker| {
                !linked
                    .iter()
                    .any(|(repository, number)| blocker.is_issue(home, repository, *number))
            })
            .cloned()
            .collect();
        let resolved = self.resolve_github_blockers(&missing)?;
        self.add_blocked_by(&subject, &resolved)?;
        if !missing.is_empty() {
            crate::issue_cache::invalidate(self.host(), self.repository());
        }
        Ok(same_host
            .iter()
            .map(|blocker| blocker.relative_to(home))
            .collect())
    }

    fn record_textual_blockers(
        &self,
        issue: u32,
        blockers: &[IssueReference],
    ) -> Result<(), GitHubCommandError> {
        let loaded = self.read_issue(issue)?;
        let updated = upsert_declared_blockers(&loaded.body, blockers);
        if updated != loaded.body {
            self.update_issue_body(issue, &loaded.id, &updated)?;
            crate::issue_cache::invalidate(self.host(), self.repository());
        }
        Ok(())
    }

    /// Every repository referenced in `issue`'s `blockedBy` dependencies,
    /// including closed blockers (#598, #589).
    pub fn linked_blocker_repositories(
        &self,
        issue: u32,
    ) -> Result<Vec<String>, GitHubCommandError> {
        if issue == 0 {
            return Ok(Vec::new());
        }
        if let IssueBackend::Collapse(collapse) = &self.backend {
            let loaded = collapse.read_issue(issue)?;
            return Ok(dedup_repositories(
                loaded
                    .open_blockers
                    .iter()
                    .chain(loaded.closed_blockers.iter())
                    .filter_map(|item| IssueReference::parse(item))
                    .map(|reference| reference.repository_or(self.repository()).to_owned()),
            ));
        }
        let (_, linked) = self.linked_blockers(issue)?;
        Ok(dedup_repositories(
            linked.into_iter().map(|(repository, _)| repository),
        ))
    }

    /// Every repository referenced in `blockedBy` dependencies (open or closed)
    /// of this repository's open issues (#598, #589).
    pub fn open_issue_blocker_repositories(&self) -> Result<Vec<String>, GitHubCommandError> {
        if let IssueBackend::Collapse(collapse) = &self.backend {
            let issues = collapse.open_issues()?;
            let home = self.repository();
            let repositories = issues.iter().flat_map(|issue| {
                issue
                    .open_blockers
                    .iter()
                    .chain(issue.closed_blockers.iter())
                    .filter_map(|item| IssueReference::parse(item))
                    .map(|reference| reference.repository_or(home).to_owned())
            });
            return Ok(dedup_repositories(repositories));
        }
        let transport = self.github_transport()?;
        let issues: Vec<Option<OpenIssueBlockerNode>> = transport.connection(
            "query($owner:String!,$name:String!,$cursor:String) {
              repository(owner:$owner,name:$name) { issues(first:100,after:$cursor,states:OPEN) {
                nodes { blockedBy(first:25) { nodes { repository { nameWithOwner } } } }
                pageInfo { hasNextPage endCursor } } } }",
            transport.variables(0),
            "/data/repository/issues",
        )?;
        let repositories = issues
            .into_iter()
            .flatten()
            .flat_map(|issue| issue.blocked_by.nodes.into_iter().flatten())
            .map(|blocker| blocker.repository.name_with_owner);
        Ok(dedup_repositories(repositories))
    }

    /// The issue's GitHub ID and every issue it already waits on, open or closed.
    fn linked_blockers(
        &self,
        issue: u32,
    ) -> Result<(String, Vec<(String, u32)>), GitHubCommandError> {
        let transport = self.github_transport()?;
        let dependent: Dependent = transport.graphql_at(
            &format!(
                "query($owner:String!,$name:String!,$number:Int!) {{
                  repository(owner:$owner,name:$name) {{ issue(number:$number) {{
                    id blockedBy(first:100) {{ {LINKED_FIELDS} }} }} }} }}"
            ),
            transport.variables(issue),
            "/data/repository/issue",
        )?;
        let linked = transport.complete_connection(
            &format!(
                "query($owner:String!,$name:String!,$number:Int!,$cursor:String) {{
                  repository(owner:$owner,name:$name) {{ issue(number:$number) {{
                    blockedBy(first:100,after:$cursor) {{ {LINKED_FIELDS} }} }} }} }}"
            ),
            transport.variables(issue),
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

    /// Reads each blocker on its configured issue host, so a blocker that does not exist
    /// is refused before anything is written (#511, #624).
    pub(crate) fn resolve_blockers(
        &self,
        blockers: &[IssueReference],
    ) -> Result<Vec<ResolvedBlocker>, GitHubCommandError> {
        if blockers.is_empty() {
            return Ok(Vec::new());
        }
        let home = self.repository();
        let (same_host, cross_host, _) = self.partition_blockers(blockers);
        self.validate_cross_host_blockers(&cross_host)?;
        let mut native_iter = self.resolve_same_host_blockers(&same_host)?.into_iter();
        let mut cross_iter = cross_host.into_iter();
        let mut resolved = Vec::with_capacity(blockers.len());
        for blocker in blockers {
            let (target_host, _) = self.blocker_host(blocker.repository_or(home));
            if target_host == self.host() {
                if let Some(item) = native_iter.next() {
                    resolved.push(item);
                }
            } else if let Some(item) = cross_iter.next() {
                resolved.push(ResolvedBlocker {
                    name: item.name,
                    reference: item.reference,
                    kind: ResolvedBlockerKind::CrossHostText,
                });
            }
        }
        Ok(resolved)
    }

    fn resolve_same_host_blockers(
        &self,
        blockers: &[IssueReference],
    ) -> Result<Vec<ResolvedBlocker>, GitHubCommandError> {
        if blockers.is_empty() {
            return Ok(Vec::new());
        }
        if let IssueBackend::Collapse(collapse) = &self.backend {
            let names = collapse.validate_blockers(blockers)?;
            return Ok(blockers
                .iter()
                .zip(names)
                .map(|(blocker, name)| ResolvedBlocker {
                    name,
                    reference: blocker.clone(),
                    kind: ResolvedBlockerKind::Native { id: String::new() },
                })
                .collect());
        }
        self.resolve_github_blockers(blockers)
    }

    fn resolve_github_blockers(
        &self,
        blockers: &[IssueReference],
    ) -> Result<Vec<ResolvedBlocker>, GitHubCommandError> {
        if blockers.is_empty() {
            return Ok(Vec::new());
        }
        let transport = self.github_transport()?;
        let home = self.repository();
        blockers
            .iter()
            .map(|blocker| {
                let name = blocker.relative_to(home);
                let found = read_github_issue_id(
                    transport,
                    blocker.repository_or(home),
                    blocker.number(),
                    &name,
                )?;
                Ok(ResolvedBlocker {
                    name,
                    reference: blocker.clone(),
                    kind: ResolvedBlockerKind::Native { id: found.id },
                })
            })
            .collect()
    }

    fn validate_cross_host_blockers(
        &self,
        blockers: &[CrossHostBlocker],
    ) -> Result<(), GitHubCommandError> {
        for blocker in blockers {
            self.validate_cross_host_blocker(blocker)?;
        }
        Ok(())
    }

    fn validate_cross_host_blocker(
        &self,
        blocker: &CrossHostBlocker,
    ) -> Result<(), GitHubCommandError> {
        match blocker.host {
            IssueHost::Collapse => {
                let transport = self.collapse_transport_for(&blocker.canonical_repository)?;
                transport.validate_issue_exists(blocker.reference.number(), &blocker.name)
            }
            IssueHost::GitHub => {
                let unreadable = |error: String| {
                    GitHubCommandError::Invalid(format!(
                        "Could not read blocker {}: {error}",
                        blocker.name
                    ))
                };
                let transport = RepositoryClient::for_repository(&blocker.canonical_repository)
                    .map_err(|error| unreadable(error.to_string()))?;
                let _ = read_github_issue_id(
                    &transport,
                    &blocker.canonical_repository,
                    blocker.reference.number(),
                    &blocker.name,
                )?;
                Ok(())
            }
        }
    }

    fn collapse_transport_for(
        &self,
        repository: &str,
    ) -> Result<CollapseIssueTransport, GitHubCommandError> {
        #[cfg(test)]
        if let Some((base_url, token)) = &self.test_collapse_endpoint {
            return CollapseIssueTransport::with_endpoint(
                base_url.clone(),
                token.clone(),
                repository,
            );
        }
        CollapseIssueTransport::for_repository(repository)
    }

    /// Links resolved native blockers to the issue with host identifier `subject`.
    /// Cross-host textual blockers are skipped because they are written in the issue body (#624).
    pub(crate) fn add_blocked_by(
        &self,
        subject: &str,
        blockers: &[ResolvedBlocker],
    ) -> Result<(), GitHubCommandError> {
        let native: Vec<&ResolvedBlocker> = blockers
            .iter()
            .filter(|blocker| blocker.is_native())
            .collect();
        if native.is_empty() {
            return Ok(());
        }
        if let IssueBackend::Collapse(collapse) = &self.backend {
            let number = subject
                .rsplit_once('#')
                .and_then(|(_, value)| value.parse::<u32>().ok())
                .filter(|number| *number > 0)
                .ok_or_else(|| {
                    GitHubCommandError::Invalid(format!(
                        "Could not parse Collapse issue number from `{subject}`"
                    ))
                })?;
            for blocker in native {
                collapse.add_dependency(number, &blocker.reference)?;
            }
            let _ = collapse.read_issue(number);
            return Ok(());
        }
        let transport = self.github_transport()?;
        for blocker in native {
            let ResolvedBlockerKind::Native { id } = &blocker.kind else {
                continue;
            };
            let _: IssueId = transport.graphql_at(
                "mutation($input:AddBlockedByInput!) { addBlockedBy(input:$input) { issue { id } } }",
                json!({"input": {"issueId": subject, "blockingIssueId": id}}),
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

    /// Converts the retired hold on `number` into issue links (#511, #589) and returns the
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
        let linked = self.link_blockers_classified(number, &blockers)?;
        if issue
            .labels
            .iter()
            .any(|label| label.eq_ignore_ascii_case(NEEDS_OWNER_LABEL))
        {
            self.remove_issue_label(number, NEEDS_OWNER_LABEL)?;
        }
        let mut rewritten = hold.rewritten(&issue.body);
        if !linked.textual_references.is_empty() {
            rewritten = upsert_declared_blockers(&rewritten, &linked.textual_references);
        }
        self.update_issue_body(number, &issue.id, &rewritten)?;
        crate::issue_cache::invalidate(self.host(), self.repository());
        Ok(Some(linked.ordered))
    }
}

fn read_github_issue_id(
    transport: &RepositoryClient,
    repository_name: &str,
    number: u32,
    display_name: &str,
) -> Result<IssueId, GitHubCommandError> {
    let unreadable = |error: String| {
        GitHubCommandError::Invalid(format!("Could not read blocker {display_name}: {error}"))
    };
    let repository =
        RepositoryName::parse(repository_name).map_err(|error| unreadable(error.to_string()))?;
    transport
        .graphql_at(
            "query($owner:String!,$name:String!,$number:Int!) {
              repository(owner:$owner,name:$name) { issue(number:$number) { id } } }",
            json!({"owner": repository.owner(), "name": repository.name(), "number": number}),
            "/data/repository/issue",
        )
        .map_err(|error| unreadable(error.to_string()))
}

fn dedup_repositories(repositories: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut unique: Vec<String> = Vec::new();
    for repository in repositories {
        if !unique
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&repository))
        {
            unique.push(repository);
        }
    }
    unique
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::Path;
    use std::thread;

    use super::*;
    use crate::SiblingIssueHost;

    fn start_mock_collapse_server(
        expected_requests: usize,
    ) -> Result<(String, thread::JoinHandle<()>), Box<dyn std::error::Error>> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let handle = thread::spawn(move || {
            for _ in 0..expected_requests {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                let mut buffer = [0_u8; 4096];
                let read = stream.read(&mut buffer).unwrap_or(0);
                let request = String::from_utf8_lossy(&buffer[..read]);
                let path = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .unwrap_or("");
                let (status, body) = match path {
                    "/api/v1/repos/mESmaC/velocity/issues/619" => (
                        "200 OK",
                        r#"{"number":619,"title":"Token usage","body":"","state":"open","reason":"","author":{"name":"alice"},"labels":[],"blocked_by":[],"blocks":[],"comment_count":0,"comments":[],"created_at":"2026-11-22T00:00:00Z","updated_at":"2026-11-22T00:00:00Z","closed_at":null}"#,
                    ),
                    "/api/v1/repos/mESmaC/rosen/issues/449" => (
                        "200 OK",
                        r#"{"number":449,"title":"Rosen change","body":"","state":"open","reason":"","author":{"name":"alice"},"labels":[],"blocked_by":[],"blocks":[],"comment_count":0,"comments":[],"created_at":"2026-11-22T00:00:00Z","updated_at":"2026-11-22T00:00:00Z","closed_at":null}"#,
                    ),
                    _ => ("404 Not Found", r#"{"error":"issue not found"}"#),
                };
                let response = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(response.as_bytes());
                let _ = stream.flush();
            }
        });
        Ok((format!("http://{address}"), handle))
    }

    #[test]
    fn waits_line_formats_native_cross_host_and_mixed_blockers() {
        let native_only = LinkedBlockers {
            native: vec!["#12".into()],
            ..LinkedBlockers::default()
        };
        assert_eq!(
            waits_line_for(IssueHost::GitHub, &native_only),
            "\n\nWaits for #12, recorded as GitHub issue dependencies."
        );
        assert_eq!(
            waits_line_for(IssueHost::Collapse, &native_only),
            "\n\nWaits for #12, recorded as issue dependencies."
        );

        let cross_only = LinkedBlockers {
            textual: vec!["mESmaC/velocity#619".into(), "mESmaC/rosen#449".into()],
            ..LinkedBlockers::default()
        };
        assert_eq!(
            waits_line_for(IssueHost::GitHub, &cross_only),
            "\n\nBlocked by: mESmaC/velocity#619, mESmaC/rosen#449"
        );

        let mixed = LinkedBlockers {
            native: vec!["#12".into()],
            textual: vec!["mESmaC/velocity#619".into()],
            ..LinkedBlockers::default()
        };
        assert_eq!(
            waits_line_for(IssueHost::GitHub, &mixed),
            "\n\nWaits for #12, recorded as GitHub issue dependencies.\n\nBlocked by: mESmaC/velocity#619"
        );
    }

    // Risk (#624): cross-host blockers must validate against the sibling project's configured
    // host (Collapse) and refuse missing issues instead of failing against the subject's host.
    #[test]
    fn cross_host_collapse_blocker_validation_accepts_existing_and_rejects_missing()
    -> Result<(), Box<dyn std::error::Error>> {
        let (base_url, handle) = start_mock_collapse_server(3)?;
        let mut client = IssueClient::for_collapse_endpoint(
            Path::new("."),
            &base_url,
            Some("test-token".into()),
            "mESmaC/velocity",
        )?;
        client.test_collapse_endpoint = Some((base_url, Some("test-token".into())));

        let velocity_ref = IssueReference::parse("mESmaC/velocity#619").ok_or("velocity ref")?;
        let rosen_ref = IssueReference::parse("mESmaC/rosen#449").ok_or("rosen ref")?;
        let missing_ref = IssueReference::parse("mESmaC/velocity#999").ok_or("missing ref")?;

        let valid = vec![
            CrossHostBlocker {
                host: IssueHost::Collapse,
                canonical_repository: "mESmaC/velocity".into(),
                reference: velocity_ref,
                name: "mESmaC/velocity#619".into(),
            },
            CrossHostBlocker {
                host: IssueHost::Collapse,
                canonical_repository: "mESmaC/rosen".into(),
                reference: rosen_ref,
                name: "mESmaC/rosen#449".into(),
            },
        ];
        client.validate_cross_host_blockers(&valid)?;

        let missing = vec![CrossHostBlocker {
            host: IssueHost::Collapse,
            canonical_repository: "mESmaC/velocity".into(),
            reference: missing_ref,
            name: "mESmaC/velocity#999".into(),
        }];
        let error = client
            .validate_cross_host_blockers(&missing)
            .err()
            .ok_or("expected missing blocker error")?;
        assert!(
            error
                .to_string()
                .contains("Could not read blocker mESmaC/velocity#999"),
            "unexpected error: {error}"
        );

        let _ = handle.join();
        Ok(())
    }

    #[test]
    fn partition_blockers_separates_same_host_and_cross_host_siblings()
    -> Result<(), Box<dyn std::error::Error>> {
        let client = IssueClient::for_collapse_endpoint(
            Path::new("."),
            "http://127.0.0.1:1",
            None,
            "mESmaC/velocity",
        )?
        .with_sibling_hosts([
            SiblingIssueHost {
                repository: "mESmaC/rosen".into(),
                host_repository: "mESmaC/rosen".into(),
                host: IssueHost::Collapse,
            },
            SiblingIssueHost {
                repository: "mESmaC/collapse.sh".into(),
                host_repository: "mESmaC/collapse.sh".into(),
                host: IssueHost::GitHub,
            },
        ]);

        let blockers = vec![
            IssueReference::parse("#619").ok_or("same repo")?,
            IssueReference::parse("mESmaC/rosen#449").ok_or("same host sibling")?,
            IssueReference::parse("mESmaC/collapse.sh#225").ok_or("cross host sibling")?,
        ];
        let (same_host, cross_host, ordered) = client.partition_blockers(&blockers);
        assert_eq!(same_host.len(), 2);
        assert_eq!(cross_host.len(), 1);
        assert_eq!(cross_host[0].host, IssueHost::GitHub);
        assert_eq!(cross_host[0].name, "mESmaC/collapse.sh#225");
        assert_eq!(
            ordered,
            vec!["#619", "mESmaC/rosen#449", "mESmaC/collapse.sh#225"]
        );
        Ok(())
    }
}
