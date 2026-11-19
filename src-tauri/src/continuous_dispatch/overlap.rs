//! Keep an issue out of continuous dispatch while another issue's open Rosen change edits
//! files it names. An issue is held this way once: when the hold ends it is dispatched.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};
use velocity_airc::Claim;
use velocity_github::{GitHubIssue, IssueClient, IssueState, velocity_reports::IssueProblem};
use velocity_rosen::{ProjectServer, change_details, review_swarm};
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
    let mut released = project.continuous_dispatch.overlap_released.clone();
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
    OverlapContext::new(client, &project, &data).recheck(
        issues,
        working,
        &mut waits,
        &mut released,
    )?;
    lock(state.inner(), "persisted state")?
        .save_issue_overlaps(identifier, waits.clone(), released.clone())
        .map_err(|error| error.to_string())?;
    let reviewed = publish_reviews(application, identifier, client, &mut waits)?;
    if !reviewed.is_empty() {
        reader.refreshed = None;
        reader
            .backlog
            .issues
            .retain(|issue| !reviewed.contains(&issue.number));
        // The review ends the issue's one hold: whatever it returns is dispatched (#525).
        released.extend(
            reviewed
                .iter()
                .map(|&number| (number, ThreadIssue::new(repository.into(), number))),
        );
        lock(state.inner(), "persisted state")?
            .save_issue_overlaps(identifier, waits, released)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// An issue being worked whose open Rosen change has edited code.
struct Blocker {
    thread: u32,
    /// The code files the change has edited against its parent, as Rosen reports them.
    edited: BTreeSet<String>,
}

struct WorkingIssue {
    thread: Option<u32>,
    /// The thread's open Rosen change, once it has one.
    change: Option<String>,
}

/// How long one read of an open change's edited files is reused. Dispatch rechecks
/// overlaps every second, and a hold that starts this much later costs at most a merge.
const EDITED_FILES_REFRESH: Duration = Duration::from_secs(30);

/// Edited code files by project and open change, with the time they were read.
type EditedFiles = BTreeMap<(u32, String), (Instant, BTreeSet<String>)>;

static EDITED_FILES: Mutex<EditedFiles> = Mutex::new(BTreeMap::new());

fn active_issues(
    project: &ProjectItem,
    repository: &str,
    issues: &[GitHubIssue],
    claims: &[Claim],
) -> BTreeMap<u32, WorkingIssue> {
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
            // A claim alone marks the issue as taken. Its claimed paths are intent, not
            // edits, so they hold nothing (#525).
            if thread.is_none() && !claims.iter().any(|claim| claim.piece == piece) {
                return None;
            }
            Some((
                issue.number,
                WorkingIssue {
                    thread: thread.map(|thread| thread.id),
                    change: thread.and_then(|thread| thread.rosen_change.clone()),
                },
            ))
        })
        .collect()
}

/// What a blocker's land means for an issue that waited for it.
enum LandCheck {
    /// The blocker has not landed, or the issue is no longer ready for work.
    Waiting,
    /// The land may have fixed this issue or changed its code, so a review decides.
    Review(String),
    /// The land changed none of this issue's code (#506). `files` is the evidence.
    Clear { commit: String, files: Vec<String> },
}

/// A parked overlap review's land: its files, and whether they leave the issue's code alone.
pub(super) struct LandEvidence {
    pub files: Vec<String>,
    pub clear: bool,
}

/// Checks an overlap review that is already parked against its land (#506).
pub(super) fn land_evidence(
    client: &IssueClient,
    project: &ProjectItem,
    data: &Path,
    issue: &GitHubIssue,
    (source, commit): (u32, &str),
) -> Result<LandEvidence, String> {
    let mut context = OverlapContext::new(client, project, data);
    let clear = context.clear_of_land(issue, source, commit)?.is_some();
    Ok(LandEvidence {
        files: context.land_files(commit)?,
        clear,
    })
}

struct OverlapContext<'a> {
    client: &'a IssueClient,
    project: &'a ProjectItem,
    data: &'a Path,
    server: ProjectServer,
    /// Every file of each landed commit read so far, documents included.
    landed: BTreeMap<String, Vec<String>>,
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
        working: BTreeMap<u32, WorkingIssue>,
        waits: &mut BTreeMap<u32, IssueOverlap>,
        released: &mut BTreeMap<u32, ThreadIssue>,
    ) -> Result<(), String> {
        let client = self.client;
        let repository = client.repository();
        let open = |issue: &ThreadIssue| {
            issue.repository.eq_ignore_ascii_case(repository)
                && issues.iter().any(|open| open.number == issue.number)
        };
        waits.retain(|_, wait| open(&wait.issue));
        released.retain(|_, issue| open(issue));
        let blockers = self.blockers(&working)?;
        let mut holdable = Vec::new();
        for issue in issues {
            // A landed issue waits for its integration build, not for another issue, and
            // reviewing it against someone else's land only parks finished work (#505).
            if working.contains_key(&issue.number)
                || has_landed(self.project, repository, issue.number)
            {
                end_hold(issue.number, waits, released);
            } else if waits.contains_key(&issue.number)
                || (!blockers.is_empty()
                    && actionable(issue)
                    && !released.contains_key(&issue.number))
            {
                holdable.push(issue);
            }
        }
        // Only an issue that can be held needs the files its text names.
        let named = if blockers.is_empty() {
            vec![BTreeSet::new(); holdable.len()]
        } else {
            let descriptions: Vec<_> = holdable.iter().map(|issue| issue_text(issue)).collect();
            issue_paths(self.data, self.project, &descriptions)
        };
        for (issue, paths) in holdable.into_iter().zip(named) {
            self.update(issue, code_paths(paths), &blockers, waits, released)?;
        }
        Ok(())
    }

    /// The working issues that can hold another: each has an open Rosen change that has
    /// edited code. Predicted paths and claimed paths never hold an issue (#525).
    fn blockers(
        &self,
        working: &BTreeMap<u32, WorkingIssue>,
    ) -> Result<BTreeMap<u32, Blocker>, String> {
        let project = self.project.id;
        let mut read = lock(&EDITED_FILES, "the edited files of open changes")?;
        read.retain(|(owner, change), _| {
            *owner != project
                || working
                    .values()
                    .any(|issue| issue.change.as_ref() == Some(change))
        });
        let mut blockers = BTreeMap::new();
        for (&number, issue) in working {
            let (Some(thread), Some(change)) = (issue.thread, &issue.change) else {
                continue;
            };
            let key = (project, change.clone());
            let (current, last) = match read.get(&key) {
                Some((at, edited)) if at.elapsed() < EDITED_FILES_REFRESH => {
                    (Some(edited.clone()), None)
                }
                known => (None, known.map(|(_, edited)| edited.clone())),
            };
            let edited = current.unwrap_or_else(|| {
                // A failed read keeps the last files read. A change never read holds
                // nothing: Rosen merges and reports conflicts, so a missed hold costs
                // less than a stalled dispatch.
                let edited = tauri::async_runtime::block_on(change_details::changed_paths(
                    &self.server,
                    change,
                ))
                .map(|paths| code_paths(paths.into_iter().collect()))
                .unwrap_or_else(|_| last.unwrap_or_default());
                read.insert(key, (Instant::now(), edited.clone()));
                edited
            });
            if !edited.is_empty() {
                blockers.insert(number, Blocker { thread, edited });
            }
        }
        Ok(blockers)
    }

    fn update(
        &mut self,
        issue: &GitHubIssue,
        paths: BTreeSet<String>,
        blockers: &BTreeMap<u32, Blocker>,
        waits: &mut BTreeMap<u32, IssueOverlap>,
        released: &mut BTreeMap<u32, ThreadIssue>,
    ) -> Result<(), String> {
        if let Some(wait) = waits.get_mut(&issue.number) {
            if wait.review_commit.is_some() {
                return Ok(());
            }
            match self.overlapping_land(wait, issue.number)? {
                LandCheck::Review(commit) => {
                    wait.review_commit = Some(commit);
                    return Ok(());
                }
                // The evidence is mechanical, so no review parks the issue. The comment
                // is posted before the wait goes; a failure keeps the wait for a retry.
                LandCheck::Clear { commit, files } => self
                    .client
                    .post_overlap_clear(issue.number, wait.blocker, &commit, &files)
                    .map_err(|error| error.to_string())?,
                LandCheck::Waiting => {}
            }
        }
        let number = issue.number;
        let ready = actionable(issue);
        if let Some(wait) = waits.get(&number) {
            // One hold per issue (#525): it lasts only while the same blocker's open
            // change still edits a file this issue names, and never moves to another.
            let shared = blockers
                .get(&wait.blocker)
                .filter(|blocker| {
                    ready && wait.blocker_thread.is_none_or(|id| id == blocker.thread)
                })
                .map(|blocker| shared_files(&paths, blocker))
                .unwrap_or_default();
            if shared.is_empty() {
                end_hold(number, waits, released);
            } else if let Some(wait) = waits.get_mut(&number) {
                wait.paths = shared;
            }
            return Ok(());
        }
        if ready && !released.contains_key(&number) {
            self.hold(number, &paths, blockers, waits, released)?;
        }
        Ok(())
    }

    /// Holds `number` behind the first working issue whose open change has edited a file
    /// the issue names. Without such a file the issue stays free for dispatch.
    fn hold(
        &self,
        number: u32,
        paths: &BTreeSet<String>,
        blockers: &BTreeMap<u32, Blocker>,
        waits: &mut BTreeMap<u32, IssueOverlap>,
        released: &mut BTreeMap<u32, ThreadIssue>,
    ) -> Result<(), String> {
        let overlap = blockers
            .iter()
            .map(|(&blocker, peer)| (blocker, peer.thread, shared_files(paths, peer)))
            .find(|(_, _, shared)| !shared.is_empty());
        let Some((blocker, thread, shared)) = overlap else {
            return Ok(());
        };
        let identity = ThreadIssue::new(self.client.repository().into(), number);
        // GitHub's record outlives local state: an issue a review already returned from
        // an overlapping land is dispatched, not held again.
        if self
            .client
            .overlap_review_returned(number)
            .map_err(|error| error.to_string())?
        {
            released.insert(number, identity);
            return Ok(());
        }
        waits.insert(
            number,
            IssueOverlap::new(identity, blocker, Some(thread), shared),
        );
        Ok(())
    }

    fn overlapping_land(&mut self, wait: &IssueOverlap, number: u32) -> Result<LandCheck, String> {
        let Some(commit) = blocker_thread(self.project, wait)
            .and_then(|thread| thread.rosen_issue.landed_commit())
            .map(str::to_owned)
        else {
            return Ok(LandCheck::Waiting);
        };
        let issue = self.client.get(number).map_err(|error| error.to_string())?;
        if !actionable(&issue) {
            return Ok(LandCheck::Waiting);
        }
        Ok(match self.clear_of_land(&issue, wait.blocker, &commit)? {
            Some(files) => LandCheck::Clear { commit, files },
            None => LandCheck::Review(commit),
        })
    }

    /// The land's files when `commit`, landed for issue `source`, changed none of `issue`'s
    /// predicted code paths and the two issues describe different problems. `None` when
    /// the land shares code with the issue or may have fixed it, so a review has to decide.
    fn clear_of_land(
        &mut self,
        issue: &GitHubIssue,
        source: u32,
        commit: &str,
    ) -> Result<Option<Vec<String>>, String> {
        let source = self.client.get(source).map_err(|error| error.to_string())?;
        let mut paths = issue_paths(
            self.data,
            self.project,
            &[issue_text(issue), issue_text(&source)],
        )
        .into_iter()
        .map(code_paths);
        let wanted = problem(issue, paths.next().unwrap_or_default());
        if wanted.matches(&problem(&source, paths.next().unwrap_or_default())) {
            return Ok(None);
        }
        let files = self.land_files(commit)?;
        let landed_code = code_paths(files.iter().cloned().collect());
        Ok(wanted.paths.is_disjoint(&landed_code).then_some(files))
    }

    fn land_files(&mut self, commit: &str) -> Result<Vec<String>, String> {
        if let Some(files) = self.landed.get(commit) {
            return Ok(files.clone());
        }
        let files =
            tauri::async_runtime::block_on(review_swarm::landed_commit_paths(&self.server, commit))
                .map_err(|error| error.to_string())?;
        self.landed.insert(commit.to_owned(), files.clone());
        Ok(files)
    }
}

fn issue_text(issue: &GitHubIssue) -> (&str, &str) {
    (&issue.title, &issue.body)
}

fn problem(issue: &GitHubIssue, paths: BTreeSet<String>) -> IssueProblem {
    IssueProblem::new(&issue.title, &issue.body, paths)
}

/// The files `blocker`'s open change has edited that the waiting issue also names.
fn shared_files(named: &BTreeSet<String>, blocker: &Blocker) -> Vec<String> {
    named.intersection(&blocker.edited).cloned().collect()
}

/// Ends `number`'s overlap hold. An open issue is held on overlap once (#525): held
/// again behind each next running issue, it never starts, and every cycle costs a review.
fn end_hold(
    number: u32,
    waits: &mut BTreeMap<u32, IssueOverlap>,
    released: &mut BTreeMap<u32, ThreadIssue>,
) {
    if let Some(wait) = waits.remove(&number) {
        released.insert(number, wait.issue);
    }
}

/// Code paths only. Specs, design references and other documents are cited by most
/// issues, so sharing one says nothing about a conflict; Rosen's claims and the merge
/// settle edits there. Counting them held every collapse.sh issue behind one thread and
/// turned each of its lands into false overlap reviews (#505). Operations scripts under
/// `ops/` are shared the same way and hold nothing either (#525).
fn code_paths(paths: BTreeSet<String>) -> BTreeSet<String> {
    paths
        .into_iter()
        .filter(|path| !is_reference(path))
        .collect()
}

fn is_reference(path: &str) -> bool {
    let path = path.replace('\\', "/").to_ascii_lowercase();
    let in_folder = |folder: &str| path.starts_with(folder) || path.contains(&format!("/{folder}"));
    // `ops/` counts at the repository root only: a nested `ops` folder is a code module.
    [".md", ".png", ".svg", ".jpg"]
        .iter()
        .any(|extension| path.ends_with(extension))
        || path.starts_with("ops/")
        || ["spec/", "design/", "docs/"]
            .iter()
            .any(|folder| in_folder(folder))
}

/// Whether `number` has a landed change of its own, from a thread, the work projection
/// or the GitHub land record dispatch saved (#504).
fn has_landed(project: &ProjectItem, repository: &str, number: u32) -> bool {
    let identity = ThreadIssue::new(repository.into(), number);
    project.rosen_issue_completed(&identity)
        || project
            .landed_awaiting_integration(&identity, None)
            .is_some()
        || project.threads.iter().any(|thread| {
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
        }
        completed.push(wait.issue.number);
    }
    for number in &completed {
        waits.remove(number);
    }
    // The parked issue gets its bounded review once the label shows in the backlog (#506).
    Ok(completed)
}
