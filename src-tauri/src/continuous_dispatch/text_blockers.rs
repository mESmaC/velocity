//! Explicit text-blocker resolution, owner-comment caching, sibling-project backlog
//! refresh, and same-host native dependency linking for continuous dispatch (#623).

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};
use velocity_core::collapse::IssueHost;
use velocity_core::issue_dependencies::{IssueReference, explicit_text_blockers, is_owner_comment};
use velocity_core::issue_holds::{
    BLOCKED_UPSTREAM_LABEL, HOLD_LABEL, TRACKING_LABEL, has_label, is_person_hold_label,
};
use velocity_github::{GitHubCommandError, GitHubIssue, IssueClient, IssueState};
use velocity_store::ProjectItem;
use velocity_threads::continuous_dispatch::{
    TextBlockerContext, referenced_blocker_project_ids, same_host_text_blockers_to_link,
    unsatisfied_text_blockers_with_context,
};

use super::{
    Backlog, Backlogs, COLLAPSE_CURSOR_REFRESH, GITHUB_BACKLOG_REFRESH, Reader, record_dependencies,
};
use crate::state::ManagedStateStore;
use crate::threads::{lock, report};

/// Maximum ready-looking open issues whose comments are prefetched per pass so owner-comment
/// blockers surface in Duty and Project Health without flooding the issue host (#623).
const COMMENT_PREFETCH_PER_PASS: usize = 5;
/// Maximum issues whose same-host text blockers are linked per pass (#623).
const TEXT_LINKS_PER_PASS: usize = 5;
/// Backoff after a failed comment read or native dependency link attempt.
const ERROR_RETRY_INTERVAL: Duration = Duration::from_secs(60 * 60);

#[derive(Default)]
pub(super) struct TextBlockers {
    /// Cached comment bodies keyed by issue number: `(updated_at, comments)`.
    comments: BTreeMap<u32, (String, Vec<String>)>,
    /// Failed comment fetches keyed by issue number: `(updated_at, failed_at)`.
    comment_retry: BTreeMap<u32, (String, Instant)>,
    /// Failed native link attempts keyed by `(issue_number, blocker_key)`.
    link_retry: BTreeMap<(u32, String), Instant>,
    /// Same-host text blockers already linked in this session, avoiding duplicate API calls
    /// before the next backlog refresh reflects them.
    linked: HashSet<(u32, String)>,
}

impl TextBlockers {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn cached_comments(&self, issue: &GitHubIssue) -> Option<Vec<String>> {
        self.comments
            .get(&issue.number)
            .filter(|(updated_at, _)| updated_at == &issue.updated_at)
            .map(|(_, bodies)| bodies.clone())
    }

    pub(super) fn store_comments(&mut self, issue: &GitHubIssue, bodies: Vec<String>) {
        self.comment_retry.remove(&issue.number);
        self.comments
            .insert(issue.number, (issue.updated_at.clone(), bodies));
    }

    fn comment_retry_active(&self, issue: &GitHubIssue) -> bool {
        self.comment_retry
            .get(&issue.number)
            .is_some_and(|(updated_at, failed_at)| {
                updated_at == &issue.updated_at && failed_at.elapsed() < ERROR_RETRY_INTERVAL
            })
    }

    fn prune(&mut self, issues: &[GitHubIssue]) {
        let current: BTreeMap<u32, &str> = issues
            .iter()
            .map(|issue| (issue.number, issue.updated_at.as_str()))
            .collect();
        self.comments.retain(|number, (updated_at, _)| {
            current.get(number).is_some_and(|now| *now == updated_at)
        });
        self.comment_retry.retain(|number, (updated_at, _)| {
            current.get(number).is_some_and(|now| *now == updated_at)
        });
        self.link_retry
            .retain(|(number, _), _| current.contains_key(number));
        self.linked
            .retain(|(number, _)| current.contains_key(number));
    }
}

/// Builds a [`TextBlockerContext`] from every loaded project backlog plus an optional
/// fresher local `backlog` for `identifier` (#623).
pub(super) fn context_for_project(
    application: &AppHandle,
    identifier: u32,
    fresher_backlog: Option<&Backlog>,
) -> TextBlockerContext {
    let mut context = TextBlockerContext::default();
    if let Some(backlogs) = application.try_state::<Backlogs>()
        && let Ok(guard) = lock(backlogs.inner(), "dispatch backlog")
    {
        for (&project_id, backlog) in guard.iter() {
            if backlog.error.is_none() && !backlog.repository.is_empty() {
                context.insert_open_issues(&backlog.repository, &backlog.issues);
                if project_id == identifier {
                    for (&number, bodies) in &backlog.comments {
                        context.insert_comments(number, bodies.clone());
                    }
                }
            }
        }
    }
    if let Some(backlog) = fresher_backlog
        && backlog.error.is_none()
        && !backlog.repository.is_empty()
    {
        context.insert_open_issues(&backlog.repository, &backlog.issues);
        for (&number, bodies) in &backlog.comments {
            context.insert_comments(number, bodies.clone());
        }
    }
    context
}

/// Reads and caches comment bodies for `issue`, returning an error when the host cannot be
/// queried so dispatch never skips an owner-comment blocker blindly (#623).
pub(super) fn load_issue_comments(
    reader: &mut Reader,
    issue: &GitHubIssue,
) -> Result<Vec<String>, GitHubCommandError> {
    if let Some(cached) = reader.text_blockers.cached_comments(issue) {
        reader.backlog.comments.insert(issue.number, cached.clone());
        return Ok(cached);
    }
    let Some(client) = reader.client.as_ref() else {
        return Ok(Vec::new());
    };
    let bodies = client.read_comment_bodies(issue.number)?;
    reader.text_blockers.store_comments(issue, bodies.clone());
    reader.backlog.comments.insert(issue.number, bodies.clone());
    Ok(bodies)
}

/// Refreshes cached owner comments, fetches any referenced sibling projects' backlogs on
/// demand, and links same-host explicit text blockers natively (#623).
pub(super) fn refresh(
    application: &AppHandle,
    project: &ProjectItem,
    reader: &mut Reader,
) -> Result<(), String> {
    if reader.backlog.error.is_some() || reader.backlog.repository.is_empty() {
        return Ok(());
    }
    reader.text_blockers.prune(&reader.backlog.issues);
    sync_cached_comments_into_backlog(reader);
    prefetch_ready_comments(project, reader);

    let all_projects = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .state()
    .projects
    .clone();

    let sibling_ids = referenced_blocker_project_ids(
        &all_projects,
        project,
        &reader.backlog.repository,
        &reader.backlog.issues,
        &reader.backlog.comments,
    );
    refresh_sibling_backlogs(application, &all_projects, &sibling_ids)?;

    let context = context_for_project(application, project.id, Some(&reader.backlog));
    link_same_host_text_blockers(&all_projects, project, reader, &context);

    let updated_context = context_for_project(application, project.id, Some(&reader.backlog));
    lock(application.state::<Backlogs>().inner(), "dispatch backlog")?
        .insert(project.id, reader.backlog.clone());
    let extra = extra_text_blockers_map(&all_projects, project, &reader.backlog, &updated_context);
    record_dependencies(application, project.id, &reader.backlog, &extra)
}

/// Refreshes any sibling project backlogs referenced by `issue` (body or `comments`) and
/// links any same-host explicit text blockers right before `start_issue` (#623).
pub(super) fn refresh_for_issue(
    application: &AppHandle,
    project: &ProjectItem,
    reader: &mut Reader,
    issue: &GitHubIssue,
    comments: &[String],
) -> Result<(), String> {
    reader
        .backlog
        .comments
        .insert(issue.number, comments.to_vec());
    let all_projects = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .state()
    .projects
    .clone();
    let comments_map = BTreeMap::from([(issue.number, comments.to_vec())]);
    let sibling_ids = referenced_blocker_project_ids(
        &all_projects,
        project,
        &reader.backlog.repository,
        std::slice::from_ref(issue),
        &comments_map,
    );
    refresh_sibling_backlogs(application, &all_projects, &sibling_ids)?;

    let context = context_for_project(application, project.id, Some(&reader.backlog));
    link_same_host_blockers_for_issue(&all_projects, project, reader, issue.number, &context);

    lock(application.state::<Backlogs>().inner(), "dispatch backlog")?
        .insert(project.id, reader.backlog.clone());
    Ok(())
}

/// Collects unsatisfied text blockers per issue for `DependencyGraph` unblock counts (#623).
pub(super) fn extra_text_blockers_map(
    projects: &[ProjectItem],
    project: &ProjectItem,
    backlog: &Backlog,
    context: &TextBlockerContext,
) -> BTreeMap<u32, Vec<String>> {
    let mut extra = BTreeMap::new();
    for issue in &backlog.issues {
        let blockers = unsatisfied_text_blockers_with_context(
            projects,
            project,
            &backlog.repository,
            issue,
            context,
        );
        if !blockers.is_empty() {
            extra.insert(issue.number, blockers);
        }
    }
    extra
}

fn sync_cached_comments_into_backlog(reader: &mut Reader) {
    reader.backlog.comments.clear();
    for issue in &reader.backlog.issues {
        if let Some(bodies) = reader.text_blockers.cached_comments(issue) {
            reader.backlog.comments.insert(issue.number, bodies);
        }
    }
}

fn prefetch_ready_comments(project: &ProjectItem, reader: &mut Reader) {
    let Some(client) = reader.client.as_ref() else {
        return;
    };
    let candidates: Vec<GitHubIssue> = reader
        .backlog
        .issues
        .iter()
        .filter(|issue| {
            needs_comment_prefetch(
                project,
                &reader.backlog.repository,
                issue,
                &reader.text_blockers,
            )
        })
        .take(COMMENT_PREFETCH_PER_PASS)
        .cloned()
        .collect();
    for issue in candidates {
        match client.read_comment_bodies(issue.number) {
            Ok(bodies) => {
                reader.text_blockers.store_comments(&issue, bodies.clone());
                reader.backlog.comments.insert(issue.number, bodies);
            }
            Err(_) => {
                reader
                    .text_blockers
                    .comment_retry
                    .insert(issue.number, (issue.updated_at.clone(), Instant::now()));
            }
        }
    }
}

fn needs_comment_prefetch(
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    state: &TextBlockers,
) -> bool {
    if issue.state != IssueState::Open
        || issue.has_open_blockers()
        || state.cached_comments(issue).is_some()
        || state.comment_retry_active(issue)
    {
        return false;
    }
    if issue.is_spec_question()
        || has_label(&issue.labels, TRACKING_LABEL)
        || has_label(&issue.labels, BLOCKED_UPSTREAM_LABEL)
        || has_label(&issue.labels, HOLD_LABEL)
        || issue.labels.iter().any(|label| is_person_hold_label(label))
    {
        return false;
    }
    !project
        .continuous_dispatch
        .attempted(repository, issue.number, &issue.updated_at)
}

fn refresh_sibling_backlogs(
    application: &AppHandle,
    all_projects: &[ProjectItem],
    sibling_ids: &[u32],
) -> Result<(), String> {
    for &sibling_id in sibling_ids {
        let Some(sibling) = all_projects.iter().find(|item| item.id == sibling_id) else {
            continue;
        };
        let host = sibling.effective_issue_host();
        let interval = match host {
            IssueHost::Collapse => COLLAPSE_CURSOR_REFRESH,
            IssueHost::GitHub => GITHUB_BACKLOG_REFRESH,
        };
        let fresh = lock(application.state::<Backlogs>().inner(), "dispatch backlog")?
            .get(&sibling_id)
            .is_some_and(|existing| {
                existing.error.is_none()
                    && !existing.repository.is_empty()
                    && existing
                        .refreshed_at
                        .is_some_and(|at| at.elapsed() < interval)
            });
        if fresh {
            continue;
        }
        let Ok(client) = open_project_client(sibling) else {
            continue;
        };
        let Some(client) = client else {
            continue;
        };
        if let Ok(issues) = client.open_issues() {
            let mut updated = Backlog {
                repository: client.repository().to_owned(),
                issues,
                comments: BTreeMap::new(),
                error: None,
                refreshed_at: Some(Instant::now()),
            };
            let mut backlogs = lock(application.state::<Backlogs>().inner(), "dispatch backlog")?;
            if let Some(existing) = backlogs.get(&sibling_id) {
                updated.comments = existing.comments.clone();
            }
            backlogs.insert(sibling_id, updated);
        }
    }
    Ok(())
}

fn open_project_client(project: &ProjectItem) -> Result<Option<IssueClient>, GitHubCommandError> {
    let directory = Path::new(&project.repository_path);
    match project.issue_host_repository() {
        Some((host, repository)) => Ok(Some(IssueClient::for_host(directory, host, &repository)?)),
        None if project.effective_issue_host() == IssueHost::GitHub
            && directory.join(".git").exists()
            && IssueClient::has_origin(directory)? =>
        {
            Ok(Some(IssueClient::discover(directory)?))
        }
        None => Ok(None),
    }
}

fn link_same_host_text_blockers(
    all_projects: &[ProjectItem],
    project: &ProjectItem,
    reader: &mut Reader,
    context: &TextBlockerContext,
) {
    let candidate_numbers: Vec<u32> = reader
        .backlog
        .issues
        .iter()
        .filter(|issue| has_any_explicit_text_blocker(issue, context.comments_for(issue.number)))
        .map(|issue| issue.number)
        .collect();
    let mut linked_issues = 0usize;
    for number in candidate_numbers {
        if linked_issues >= TEXT_LINKS_PER_PASS {
            break;
        }
        if link_same_host_blockers_for_issue(all_projects, project, reader, number, context) {
            linked_issues += 1;
        }
    }
}

fn has_any_explicit_text_blocker(issue: &GitHubIssue, comments: &[String]) -> bool {
    !explicit_text_blockers(&issue.body).is_empty()
        || comments
            .iter()
            .any(|comment| is_owner_comment(comment) && !explicit_text_blockers(comment).is_empty())
}

fn link_same_host_blockers_for_issue(
    all_projects: &[ProjectItem],
    project: &ProjectItem,
    reader: &mut Reader,
    number: u32,
    context: &TextBlockerContext,
) -> bool {
    let Some(issue) = reader
        .backlog
        .issues
        .iter()
        .find(|entry| entry.number == number)
        .cloned()
    else {
        return false;
    };
    let repository = reader.backlog.repository.clone();
    let pending: Vec<IssueReference> =
        same_host_text_blockers_to_link(all_projects, project, &repository, &issue, context)
            .into_iter()
            .filter(|reference| {
                let key = (
                    number,
                    reference.relative_to(&repository).to_ascii_lowercase(),
                );
                !reader.text_blockers.linked.contains(&key)
                    && reader
                        .text_blockers
                        .link_retry
                        .get(&key)
                        .is_none_or(|failed_at| failed_at.elapsed() >= ERROR_RETRY_INTERVAL)
            })
            .collect();
    if pending.is_empty() {
        return false;
    }
    let Some(client) = reader.client.as_ref() else {
        return false;
    };
    match client.link_blockers(number, &pending) {
        Ok(linked_names) => {
            record_linked_blockers(reader, &repository, number, &pending, linked_names);
            true
        }
        Err(error) => {
            let now = Instant::now();
            for reference in &pending {
                let key = (
                    number,
                    reference.relative_to(&repository).to_ascii_lowercase(),
                );
                reader.text_blockers.link_retry.insert(key, now);
            }
            report::<()>(Err(format!(
                "Could not link same-host text blockers on {repository}#{number}: {error}"
            )));
            true
        }
    }
}

fn record_linked_blockers(
    reader: &mut Reader,
    repository: &str,
    number: u32,
    pending: &[IssueReference],
    linked_names: Vec<String>,
) {
    for reference in pending {
        let key = (
            number,
            reference.relative_to(repository).to_ascii_lowercase(),
        );
        reader.text_blockers.link_retry.remove(&key);
        reader.text_blockers.linked.insert(key);
    }
    if let Some(slot) = reader
        .backlog
        .issues
        .iter_mut()
        .find(|entry| entry.number == number)
    {
        for name in linked_names {
            if !slot
                .open_blockers
                .iter()
                .any(|existing| existing.eq_ignore_ascii_case(&name))
            {
                slot.open_blockers.push(name);
            }
        }
        slot.open_blocker_count = slot
            .open_blocker_count
            .max(u32::try_from(slot.open_blockers.len()).unwrap_or(u32::MAX));
    }
}
