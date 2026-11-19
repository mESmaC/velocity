//! Keep overlapping issues out of continuous dispatch while an earlier issue is worked.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use tauri::{AppHandle, Manager};
use velocity_airc::Claim;
use velocity_github::{GitHubIssue, IssueClient, IssueState, velocity_reports::IssueProblem};
use velocity_rosen::{ProjectServer, review_swarm};
use velocity_store::{
    ProjectItem, ThreadItem, ThreadState, continuous_dispatch::IssueOverlap, issues::ThreadIssue,
};
use velocity_threads::continuous_dispatch::actionable;

use super::{Reader, dispatch_claims};
use crate::{graph_access::issue_paths, state::ManagedStateStore, threads::lock};

pub(super) fn refresh(
    application: &AppHandle,
    identifier: u32,
    reader: &mut Reader,
) -> Result<(), String> {
    let Some(client) = reader
        .client
        .as_ref()
        .filter(|_| reader.backlog.error.is_none())
    else {
        return Ok(());
    };
    let state = application.state::<ManagedStateStore>();
    let project = lock(state.inner(), "persisted state")?
        .project(identifier)
        .cloned()
        .ok_or("The project was removed")?;
    let mut waits = project.continuous_dispatch.issue_overlaps.clone();
    if project.continuous_dispatch.slots == 0 && waits.is_empty() {
        return Ok(());
    }
    let claims = dispatch_claims(application, identifier)?;
    let repository = client.repository();
    let issues = &reader.backlog.issues;
    let working = active_issues(&project, repository, issues, &claims);
    if working.is_empty() && waits.is_empty() {
        return Ok(());
    }
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    OverlapContext::new(client, &project, &data).recheck(issues, working, &mut waits)?;
    lock(state.inner(), "persisted state")?
        .save_issue_overlaps(identifier, waits.clone())
        .map_err(|error| error.to_string())?;
    let reviewed = publish_reviews(application, identifier, client, &mut waits)?;
    if !reviewed.is_empty() {
        reader.refreshed = None;
        reader
            .backlog
            .issues
            .retain(|issue| !reviewed.contains(&issue.number));
        lock(state.inner(), "persisted state")?
            .save_issue_overlaps(identifier, waits)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

struct ActiveIssue {
    thread: Option<u32>,
    problem: IssueProblem,
}

struct WorkingIssue<'a> {
    issue: &'a GitHubIssue,
    thread: Option<u32>,
    files: BTreeSet<String>,
}

fn active_issues<'a>(
    project: &ProjectItem,
    repository: &str,
    issues: &'a [GitHubIssue],
    claims: &[Claim],
) -> BTreeMap<u32, WorkingIssue<'a>> {
    issues
        .iter()
        .filter(|issue| issue.state == IssueState::Open)
        .filter_map(|issue| {
            let thread = project.threads.iter().find(|thread| {
                thread.issue.as_ref().is_some_and(|key| {
                    key.number == issue.number && key.repository.eq_ignore_ascii_case(repository)
                })
            });
            if thread.is_some_and(|thread| thread.rosen_issue.landed_commit().is_some()) {
                return None;
            }
            let thread = thread.filter(|thread| {
                !thread.supervision.released
                    && matches!(thread.state, ThreadState::Working | ThreadState::Review)
            });
            let piece = format!("issue:{}", issue.number);
            let claim = claims.iter().find(|claim| claim.piece == piece);
            if thread.is_none() && claim.is_none() {
                return None;
            }
            let files = claim
                .into_iter()
                .flat_map(|claim| &claim.files)
                .map(|path| path.replace('\\', "/"))
                .collect();
            Some((
                issue.number,
                WorkingIssue {
                    issue,
                    thread: thread.map(|thread| thread.id),
                    files,
                },
            ))
        })
        .collect()
}

struct OverlapContext<'a> {
    client: &'a IssueClient,
    project: &'a ProjectItem,
    data: &'a Path,
    server: ProjectServer,
    landed: BTreeMap<String, BTreeSet<String>>,
}

impl<'a> OverlapContext<'a> {
    fn new(client: &'a IssueClient, project: &'a ProjectItem, data: &'a Path) -> Self {
        Self {
            client,
            project,
            data,
            server: ProjectServer::new(data, project.id),
            landed: BTreeMap::new(),
        }
    }

    fn recheck(
        &mut self,
        issues: &[GitHubIssue],
        working: BTreeMap<u32, WorkingIssue<'_>>,
        waits: &mut BTreeMap<u32, IssueOverlap>,
    ) -> Result<(), String> {
        let candidates: Vec<_> = issues
            .iter()
            .filter(|issue| working.contains_key(&issue.number) || actionable(issue))
            .collect();
        let descriptions: Vec<_> = candidates.iter().map(|issue| issue_text(issue)).collect();
        let mut paths: BTreeMap<_, _> = candidates
            .into_iter()
            .map(|issue| issue.number)
            .zip(
                issue_paths(self.data, self.project, &descriptions)
                    .into_iter()
                    .map(code_paths),
            )
            .collect();
        let active: BTreeMap<_, _> = working
            .into_iter()
            .map(|(number, working)| {
                let mut problem = problem(working.issue, paths.remove(&number).unwrap_or_default());
                problem.paths.extend(code_paths(working.files));
                (
                    number,
                    ActiveIssue {
                        thread: working.thread,
                        problem,
                    },
                )
            })
            .collect();
        waits.retain(|number, wait| {
            wait.issue
                .repository
                .eq_ignore_ascii_case(self.client.repository())
                && issues.iter().any(|issue| issue.number == *number)
        });
        for issue in issues {
            // A landed issue waits for its integration build, not for another issue, and
            // reviewing it against someone else's land only parks finished work (#505).
            if active.contains_key(&issue.number) || has_landed(self.project, issue.number) {
                waits.remove(&issue.number);
            } else {
                self.update(
                    issue,
                    paths.remove(&issue.number).unwrap_or_default(),
                    &active,
                    waits,
                )?;
            }
        }
        Ok(())
    }

    fn update(
        &mut self,
        issue: &GitHubIssue,
        paths: BTreeSet<String>,
        active: &BTreeMap<u32, ActiveIssue>,
        waits: &mut BTreeMap<u32, IssueOverlap>,
    ) -> Result<(), String> {
        if let Some(wait) = waits.get_mut(&issue.number) {
            if wait.review_commit.is_some() {
                return Ok(());
            }
            if let Some(commit) = self.overlapping_land(wait, issue.number)? {
                wait.review_commit = Some(commit);
                return Ok(());
            }
        }
        let blocker = actionable(issue)
            .then(|| problem(issue, paths))
            .and_then(|wanted| {
                active
                    .iter()
                    .find(|(_, peer)| wanted.overlaps(&peer.problem))
            });
        if let Some((&blocker, peer)) = blocker {
            waits.insert(
                issue.number,
                IssueOverlap {
                    issue: ThreadIssue::new(self.client.repository().into(), issue.number),
                    blocker,
                    blocker_thread: peer.thread,
                    review_commit: None,
                },
            );
        } else {
            waits.remove(&issue.number);
        }
        Ok(())
    }

    fn overlapping_land(
        &mut self,
        wait: &IssueOverlap,
        number: u32,
    ) -> Result<Option<String>, String> {
        let Some(commit) = blocker_thread(self.project, wait)
            .and_then(|thread| thread.rosen_issue.landed_commit())
            .map(str::to_owned)
        else {
            return Ok(None);
        };
        let issue = self.client.get(number).map_err(|error| error.to_string())?;
        if !actionable(&issue) {
            return Ok(None);
        }
        let source = self
            .client
            .get(wait.blocker)
            .map_err(|error| error.to_string())?;
        let mut paths = issue_paths(
            self.data,
            self.project,
            &[issue_text(&issue), issue_text(&source)],
        )
        .into_iter()
        .map(code_paths);
        let wanted = problem(&issue, paths.next().unwrap_or_default());
        if wanted.matches(&problem(&source, paths.next().unwrap_or_default())) {
            return Ok(Some(commit));
        }
        if wanted.paths.is_empty() {
            return Ok(None);
        }
        if !self.landed.contains_key(&commit) {
            let paths = tauri::async_runtime::block_on(review_swarm::landed_commit_paths(
                &self.server,
                &commit,
            ))
            .map_err(|error| error.to_string())?;
            self.landed
                .insert(commit.clone(), code_paths(paths.into_iter().collect()));
        }
        Ok(self
            .landed
            .get(&commit)
            .is_some_and(|paths| !wanted.paths.is_disjoint(paths))
            .then_some(commit))
    }
}

fn issue_text(issue: &GitHubIssue) -> (&str, &str) {
    (&issue.title, &issue.body)
}

fn problem(issue: &GitHubIssue, paths: BTreeSet<String>) -> IssueProblem {
    IssueProblem::new(&issue.title, &issue.body, paths)
}

/// Code paths only. Specs, design references and other documents are cited by most
/// issues, so sharing one says nothing about a conflict; Rosen's claims and the merge
/// settle edits there. Counting them held every collapse.sh issue behind one thread and
/// turned each of its lands into false overlap reviews (#505).
fn code_paths(paths: BTreeSet<String>) -> BTreeSet<String> {
    paths
        .into_iter()
        .filter(|path| !is_reference(path))
        .collect()
}

fn is_reference(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    let in_folder = |folder: &str| path.starts_with(folder) || path.contains(&format!("/{folder}"));
    [".md", ".png", ".svg", ".jpg"]
        .iter()
        .any(|extension| path.ends_with(extension))
        || ["spec/", "design/", "docs/"]
            .iter()
            .any(|folder| in_folder(folder))
}

/// Whether the project's thread for `number` has landed its change.
fn has_landed(project: &ProjectItem, number: u32) -> bool {
    project.threads.iter().any(|thread| {
        thread
            .issue
            .as_ref()
            .is_some_and(|issue| issue.number == number)
            && thread.rosen_issue.landed_commit().is_some()
    })
}

fn blocker_thread<'a>(project: &'a ProjectItem, wait: &IssueOverlap) -> Option<&'a ThreadItem> {
    project.threads.iter().find(|thread| {
        thread.issue.as_ref().is_some_and(|issue| {
            issue.number == wait.blocker
                && issue
                    .repository
                    .eq_ignore_ascii_case(&wait.issue.repository)
        }) && wait
            .blocker_thread
            .is_none_or(|identifier| thread.id == identifier)
    })
}

fn publish_reviews(
    application: &AppHandle,
    identifier: u32,
    client: &IssueClient,
    waits: &mut BTreeMap<u32, IssueOverlap>,
) -> Result<Vec<u32>, String> {
    let mut published = false;
    let mut completed = Vec::new();
    for wait in waits.values() {
        let Some(commit) = wait.review_commit.as_deref() else {
            continue;
        };
        let still_landed = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .project(identifier)
        .and_then(|project| blocker_thread(project, wait))
        .and_then(|thread| thread.rosen_issue.landed_commit())
            == Some(commit);
        if still_landed
            && client
                .get(wait.issue.number)
                .map_err(|error| error.to_string())?
                .state
                == IssueState::Open
        {
            client
                .post_overlap_review(wait.issue.number, wait.blocker, commit)
                .map_err(|error| error.to_string())?;
            published = true;
        }
        completed.push(wait.issue.number);
    }
    for number in &completed {
        waits.remove(number);
    }
    if published && let Some(sweeps) = application.try_state::<velocity_threads::ScoutSweeps>() {
        sweeps.request_no_change_review(identifier);
    }
    Ok(completed)
}
