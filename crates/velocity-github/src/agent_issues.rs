//! Reading and filing project issues for agents through AIRC (`issue_read`, `issue_create`, #535).

use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, LazyLock, Mutex};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use velocity_core::issue_dependencies::{IssueReference, upsert_declared_blockers};
use velocity_core::issue_holds::{
    BLOCKED_UPSTREAM_LABEL, HOLD_LABEL, TRACKING_LABEL, parse_explicit_person_hold,
    parse_upstream_target, refuse_retired_label, validate_person_hold,
};

use crate::issues::{
    BLOCKER_FIELDS, Blocker, ISSUE_FIELDS, IssueBackend, IssueNode, relative_blocker_name,
};
use crate::repository_client::Connection;
use crate::velocity_reports::find_matching_open_issue;
use crate::{
    GitHubActor, GitHubCommandError, GitHubIssue, IssueClient, IssueState, RepositoryName,
};

const SPEC_QUESTION_LABEL: &str = "spec-question";
const MAX_TITLE_CHARS: usize = 256;
const MAX_BODY_BYTES: usize = 65_000;
const MAX_RECENT_COMMENTS: usize = 50;
const MAX_LISTED_LABELS: usize = 30;

type FilingLocks = Mutex<BTreeMap<String, Arc<Mutex<()>>>>;
static AGENT_FILING_LOCKS: LazyLock<FilingLocks> = LazyLock::new(Mutex::default);

/// One issue that another issue waits on, with its current GitHub state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueDependency {
    pub issue: String,
    pub state: IssueState,
}

/// One comment returned by `issue_read`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct IssueCommentEntry {
    pub author: String,
    pub created_at: String,
    pub body: String,
}

/// Full details of one project issue, including dependencies and recent comments.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IssueDetail {
    pub repository: String,
    pub number: u32,
    pub url: String,
    pub state: IssueState,
    pub title: String,
    pub body: String,
    pub author: Option<String>,
    pub labels: Vec<String>,
    pub blocked_by: Vec<IssueDependency>,
    pub total_comments: usize,
    pub comments: Vec<IssueCommentEntry>,
}

/// An agent's request to file a new issue on the project's mirror repository or a registered sibling project.
pub struct AgentIssueDraft<'a> {
    pub title: &'a str,
    pub body: &'a str,
    pub labels: &'a [String],
    pub blocked_by: &'a [IssueReference],
    pub byline: &'a str,
    /// When true (sibling-project filing, #608), labels must already exist in the target
    /// repository and no auto-labels are created there.
    pub existing_labels_only: bool,
}

/// Outcome of `issue_create`: either a new issue or an existing matching open issue (#471).
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum AgentIssueOutcome {
    Filed {
        issue: GitHubIssue,
        #[serde(skip_serializing_if = "Vec::is_empty")]
        blocked_by: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        unlinked: Option<String>,
    },
    Duplicate {
        issue: GitHubIssue,
        reason: String,
    },
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawIssueDetail {
    #[serde(flatten)]
    node: IssueNode,
    blocked_by: Connection<Option<Blocker>>,
    comments: RawComments,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawComments {
    total_count: usize,
    #[serde(default)]
    nodes: Vec<Option<RawComment>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawComment {
    author: Option<GitHubActor>,
    body: String,
    created_at: String,
}

impl IssueClient {
    /// Reads one issue's title, body, labels, state, dependencies and recent comments.
    pub fn read_issue_detail(
        &self,
        number: u32,
        recent_comments: usize,
    ) -> Result<IssueDetail, GitHubCommandError> {
        match &self.backend {
            IssueBackend::Collapse(collapse) => collapse.read_issue_detail_for(
                collapse.owner(),
                collapse.name(),
                collapse.repository(),
                number,
                recent_comments.min(MAX_RECENT_COMMENTS),
            ),
            IssueBackend::GitHub(transport) => self.read_issue_detail_for(
                transport.owner(),
                transport.name(),
                self.repository(),
                number,
                recent_comments,
            ),
        }
    }

    /// Reads one issue from another allowed repository through this client's transport (#598, #589).
    pub fn read_issue_detail_in(
        &self,
        repository: &RepositoryName,
        number: u32,
        recent_comments: usize,
    ) -> Result<IssueDetail, GitHubCommandError> {
        let full_name = repository.full_name();
        match &self.backend {
            IssueBackend::Collapse(collapse) => collapse.read_issue_detail_for(
                repository.owner(),
                repository.name(),
                &full_name,
                number,
                recent_comments.min(MAX_RECENT_COMMENTS),
            ),
            IssueBackend::GitHub(_) => self.read_issue_detail_for(
                repository.owner(),
                repository.name(),
                &full_name,
                number,
                recent_comments,
            ),
        }
    }

    fn read_issue_detail_for(
        &self,
        owner: &str,
        name: &str,
        repository: &str,
        number: u32,
        recent_comments: usize,
    ) -> Result<IssueDetail, GitHubCommandError> {
        let transport = self.github_transport()?;
        if number == 0 {
            return Err(GitHubCommandError::Invalid(
                "issue must be a positive issue number".into(),
            ));
        }
        let comment_limit = recent_comments.min(MAX_RECENT_COMMENTS);
        let (query, mut variables) =
            issue_detail_query(comment_limit, transport.variables_for(owner, name, number));
        if comment_limit > 0 {
            variables["comments"] = json!(comment_limit);
        }
        let raw: RawIssueDetail =
            transport.graphql_at(&query, variables, "/data/repository/issue")?;
        let issue = self.decode_issue_for(owner, name, raw.node)?;
        let blockers =
            self.complete_blockers_for(owner, name, repository, issue.number, raw.blocked_by)?;
        let comments = raw
            .comments
            .nodes
            .into_iter()
            .flatten()
            .map(|comment| IssueCommentEntry {
                author: comment
                    .author
                    .map_or_else(|| "ghost".into(), |actor| actor.login),
                created_at: comment.created_at,
                body: comment.body,
            })
            .collect();
        Ok(IssueDetail {
            repository: repository.to_owned(),
            number: issue.number,
            url: issue.url,
            state: issue.state,
            title: issue.title,
            body: issue.body,
            author: issue.author.map(|actor| actor.login),
            labels: issue.labels,
            blocked_by: blockers,
            total_comments: raw.comments.total_count,
            comments,
        })
    }

    /// Files an agent issue on the project's configured issue host after checking for
    /// duplicates (#471) and linking any `blocked_by` issues as dependencies (#511, #589, #608, #624).
    pub fn create_agent_issue(
        &self,
        draft: &AgentIssueDraft<'_>,
        mentioned_paths: impl FnOnce(&[(&str, &str)]) -> Vec<BTreeSet<String>>,
    ) -> Result<AgentIssueOutcome, GitHubCommandError> {
        let (title, body, full_body, labels) = validate_draft(draft)?;
        // Refuse invalid blockers before running the duplicate scan or creating the issue (#511, #624).
        let blockers = self.resolve_blockers(draft.blocked_by)?;
        let blocked_by: Vec<String> = blockers
            .iter()
            .map(|blocker| blocker.name.clone())
            .collect();
        let full_body = append_textual_blockers(&full_body, &blockers)?;
        if let IssueBackend::Collapse(collapse) = &self.backend {
            let native_blockers: Vec<String> = blockers
                .iter()
                .filter(|blocker| blocker.is_native())
                .map(|blocker| blocker.name.clone())
                .collect();
            return self.create_collapse_agent_issue(
                collapse,
                (title, body, &full_body, &labels),
                (&native_blockers, blocked_by),
                draft.existing_labels_only,
                mentioned_paths,
            );
        }
        let label_ids = self.resolve_agent_labels(&labels, !draft.existing_labels_only)?;
        let lock = agent_filing_lock(self.repository())?;
        let _guard = lock
            .lock()
            .map_err(|error| GitHubCommandError::Invalid(error.to_string()))?;
        let open = self.open_issues_fresh()?;
        if let Some(existing) = find_matching_open_issue(title, body, &open, mentioned_paths) {
            return Ok(AgentIssueOutcome::Duplicate {
                reason: duplicate_reason(
                    self.repository(),
                    existing.number,
                    draft.existing_labels_only,
                ),
                issue: existing.clone(),
            });
        }
        let issue = self.create(title, &full_body, &label_ids)?;
        // The issue exists now, so a partial link failure returns it instead of failing the call.
        let unlinked = self
            .add_blocked_by(&issue.id, &blockers)
            .err()
            .map(|error| {
                format!(
                    "Issue #{} was created, but its blockers were not all linked ({error}).",
                    issue.number
                )
            });
        Ok(AgentIssueOutcome::Filed {
            issue,
            blocked_by,
            unlinked,
        })
    }

    fn create_collapse_agent_issue(
        &self,
        collapse: &crate::collapse_issues::CollapseIssueTransport,
        (title, body, full_body, labels): (&str, &str, &str, &[String]),
        (native_blockers, all_blockers): (&[String], Vec<String>),
        existing_labels_only: bool,
        mentioned_paths: impl FnOnce(&[(&str, &str)]) -> Vec<BTreeSet<String>>,
    ) -> Result<AgentIssueOutcome, GitHubCommandError> {
        let resolved_labels = resolve_collapse_labels(collapse, labels, !existing_labels_only)?;
        let lock = agent_filing_lock(&format!("collapse:{}", self.repository()))?;
        let _guard = lock
            .lock()
            .map_err(|error| GitHubCommandError::Invalid(error.to_string()))?;
        let open = collapse.open_issues_fresh()?;
        if let Some(existing) = find_matching_open_issue(title, body, &open, mentioned_paths) {
            return Ok(AgentIssueOutcome::Duplicate {
                reason: duplicate_reason(self.repository(), existing.number, existing_labels_only),
                issue: existing.clone(),
            });
        }
        let issue = collapse.create_issue(title, full_body, &resolved_labels, native_blockers)?;
        Ok(AgentIssueOutcome::Filed {
            issue,
            blocked_by: all_blockers,
            unlinked: None,
        })
    }

    fn complete_blockers_for(
        &self,
        owner: &str,
        name: &str,
        repository: &str,
        number: u32,
        first_page: Connection<Option<Blocker>>,
    ) -> Result<Vec<IssueDependency>, GitHubCommandError> {
        let transport = self.github_transport()?;
        let blockers = transport.complete_connection(
            &format!(
                "query($owner:String!,$name:String!,$number:Int!,$cursor:String) {{
                  repository(owner:$owner,name:$name) {{ issue(number:$number) {{
                    blockedBy(first:100,after:$cursor) {{ nodes {{ {BLOCKER_FIELDS} }}
                      pageInfo {{ hasNextPage endCursor }} }} }} }} }}"
            ),
            transport.variables_for(owner, name, number),
            "/data/repository/issue/blockedBy",
            first_page,
        )?;
        Ok(blockers
            .into_iter()
            .flatten()
            .map(|blocker| IssueDependency {
                issue: relative_blocker_name(
                    repository,
                    &blocker.repository.name_with_owner,
                    blocker.number,
                ),
                state: blocker.state,
            })
            .collect())
    }

    fn resolve_agent_labels(
        &self,
        names: &[String],
        auto_create: bool,
    ) -> Result<Vec<String>, GitHubCommandError> {
        if names.is_empty() {
            return Ok(Vec::new());
        }
        let mut available = self.label_identifiers()?;
        if auto_create {
            let auto_labels = [
                SPEC_QUESTION_LABEL,
                TRACKING_LABEL,
                BLOCKED_UPSTREAM_LABEL,
                HOLD_LABEL,
            ];
            let mut ensured = false;
            for auto in auto_labels {
                if names.iter().any(|name| name.eq_ignore_ascii_case(auto))
                    && !available
                        .keys()
                        .any(|label| label.eq_ignore_ascii_case(auto))
                {
                    self.ensure_repository_label(auto)?;
                    ensured = true;
                }
            }
            if ensured {
                available = self.label_identifiers()?;
            }
        }
        let mut identifiers = Vec::new();
        for name in names {
            let Some((_, identifier)) = available
                .iter()
                .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
            else {
                return Err(unknown_label_error(name, &available));
            };
            if !identifiers.contains(identifier) {
                identifiers.push(identifier.clone());
            }
        }
        Ok(identifiers)
    }

    fn ensure_repository_label(&self, name: &str) -> Result<(), GitHubCommandError> {
        let transport = self.github_transport()?;
        let labels = format!("repos/{}/labels", self.repository());
        let label = format!("{labels}/{name}");
        if transport.command.read::<Value>(&["api", &label]).is_err()
            && let Err(error) = transport.command.api::<Value>(
                "POST",
                &labels,
                &json!({"name": name, "color": "ededed"}),
            )
        {
            transport
                .command
                .read::<Value>(&["api", &label])
                .map_err(|_| error)?;
        }
        Ok(())
    }
}

fn append_textual_blockers(
    full_body: &str,
    blockers: &[crate::issue_dependencies::ResolvedBlocker],
) -> Result<String, GitHubCommandError> {
    let textual_blockers: Vec<IssueReference> = blockers
        .iter()
        .filter_map(crate::issue_dependencies::ResolvedBlocker::cross_host_reference)
        .cloned()
        .collect();
    if textual_blockers.is_empty() {
        return Ok(full_body.to_owned());
    }
    let updated = upsert_declared_blockers(full_body, &textual_blockers);
    if updated.len() > MAX_BODY_BYTES {
        return Err(GitHubCommandError::Invalid(
            "Issue body exceeds 65000 UTF-8 bytes; shorten the description".into(),
        ));
    }
    Ok(updated)
}

fn resolve_collapse_labels(
    collapse: &crate::collapse_issues::CollapseIssueTransport,
    names: &[String],
    auto_create: bool,
) -> Result<Vec<String>, GitHubCommandError> {
    if names.is_empty() {
        return Ok(Vec::new());
    }
    if auto_create {
        let auto_labels = [
            SPEC_QUESTION_LABEL,
            TRACKING_LABEL,
            BLOCKED_UPSTREAM_LABEL,
            HOLD_LABEL,
        ];
        for auto in auto_labels {
            if names.iter().any(|name| name.eq_ignore_ascii_case(auto)) {
                collapse.ensure_label(auto)?;
            }
        }
    }
    let available: BTreeMap<String, String> = collapse
        .label_names()?
        .into_iter()
        .map(|label| (label.clone(), label))
        .collect();
    let mut resolved = Vec::new();
    for name in names {
        let Some((existing, _)) = available
            .iter()
            .find(|(existing, _)| existing.eq_ignore_ascii_case(name))
        else {
            return Err(unknown_label_error(name, &available));
        };
        if !resolved.contains(existing) {
            resolved.push(existing.clone());
        }
    }
    Ok(resolved)
}

fn duplicate_reason(repository: &str, number: u32, cross_repository: bool) -> String {
    if cross_repository {
        format!(
            "Open issue {repository}#{number} already matches this report. Read it with issue_read (repository: {repository:?}) instead of opening a duplicate."
        )
    } else {
        format!(
            "Open issue #{number} already matches this report. Read it with issue_read instead of opening a duplicate."
        )
    }
}

fn issue_detail_query(comment_limit: usize, variables: Value) -> (String, Value) {
    let comments = if comment_limit > 0 {
        "comments(last:$comments) { totalCount nodes { author { login } body createdAt } }"
    } else {
        "comments { totalCount }"
    };
    let signature = if comment_limit > 0 {
        "query($owner:String!,$name:String!,$number:Int!,$comments:Int!)"
    } else {
        "query($owner:String!,$name:String!,$number:Int!)"
    };
    let query = format!(
        "{signature} {{ repository(owner:$owner,name:$name) {{ issue(number:$number) {{ \
         {ISSUE_FIELDS} blockedBy(first:100) {{ nodes {{ {BLOCKER_FIELDS} }} \
         pageInfo {{ hasNextPage endCursor }} }} {comments} }} }} }}"
    );
    (query, variables)
}

fn validate_draft<'a>(
    draft: &'a AgentIssueDraft<'a>,
) -> Result<(&'a str, &'a str, String, Vec<String>), GitHubCommandError> {
    let title = draft.title.trim();
    if title.is_empty() || title.chars().count() > MAX_TITLE_CHARS || title.contains(['\r', '\n']) {
        return Err(GitHubCommandError::Invalid(
            "Title must be one nonempty line, at most 256 characters".into(),
        ));
    }
    let body = draft.body.trim();
    if body.is_empty() {
        return Err(GitHubCommandError::Invalid(
            "An issue needs a nonempty body".into(),
        ));
    }
    let mut labels = Vec::with_capacity(draft.labels.len());
    for label in draft.labels {
        let trimmed = label.trim();
        if trimmed.is_empty() {
            return Err(GitHubCommandError::Invalid(
                "Labels must be nonempty strings".into(),
            ));
        }
        refuse_retired_label(trimmed).map_err(GitHubCommandError::Invalid)?;
        if trimmed.eq_ignore_ascii_case(BLOCKED_UPSTREAM_LABEL)
            && parse_upstream_target(body, None).is_none()
        {
            return Err(GitHubCommandError::Invalid(
                "Label `blocked-upstream` requires an upstream PR or release reference in the body \
                 (for example `Blocked upstream: owner/repo#123` or `owner/repo@v1.0.0`)."
                    .into(),
            ));
        }
        if trimmed.eq_ignore_ascii_case(HOLD_LABEL) {
            let Some(hold) = parse_explicit_person_hold(body) else {
                return Err(GitHubCommandError::Invalid(
                    "Label `hold` requires `Hold: <reason>` and `Release when: <action>` lines in the body."
                        .into(),
                ));
            };
            validate_person_hold(&hold.reason, &hold.release_action)
                .map_err(GitHubCommandError::Invalid)?;
        }
        labels.push(trimmed.to_owned());
    }
    let byline = draft.byline.trim();
    let full_body = if byline.is_empty() {
        body.to_owned()
    } else if byline.starts_with("Reported by ") {
        format!("{body}\n\n{byline}")
    } else {
        format!("{body}\n\nFiled by {byline}.")
    };
    if full_body.len() > MAX_BODY_BYTES {
        return Err(GitHubCommandError::Invalid(
            "Issue body exceeds 65000 UTF-8 bytes; shorten the description".into(),
        ));
    }
    Ok((title, body, full_body, labels))
}

fn unknown_label_error(name: &str, available: &BTreeMap<String, String>) -> GitHubCommandError {
    if available.is_empty() {
        return GitHubCommandError::Invalid(format!(
            "Unknown label {name:?}; this repository has no labels defined. Omit labels or pass []."
        ));
    }
    let listed: Vec<&str> = available
        .keys()
        .take(MAX_LISTED_LABELS)
        .map(String::as_str)
        .collect();
    let suffix = if available.len() > MAX_LISTED_LABELS {
        format!(" (and {} more)", available.len() - MAX_LISTED_LABELS)
    } else {
        String::new()
    };
    GitHubCommandError::Invalid(format!(
        "Unknown label {name:?}. Available labels: {}{suffix}",
        listed.join(", ")
    ))
}

fn agent_filing_lock(repository: &str) -> Result<Arc<Mutex<()>>, GitHubCommandError> {
    let mut locks = AGENT_FILING_LOCKS
        .lock()
        .map_err(|error| GitHubCommandError::Invalid(error.to_string()))?;
    Ok(Arc::clone(locks.entry(repository.to_owned()).or_default()))
}
