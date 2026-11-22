//! Keep an issue out of continuous dispatch while another issue's open Rosen change edits
//! files it names. An issue is held this way once: when the hold ends it is dispatched.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use tauri::{AppHandle, Manager};
use velocity_airc::Claim;
use velocity_github::{GitHubIssue, IssueClient, IssueState, velocity_reports::IssueProblem};
use velocity_rosen::policy::{self, MergedPaths};
use velocity_rosen::{ProjectServer, change_details, review_swarm};
use velocity_store::{
    ProjectItem, ThreadItem, ThreadState, VersionControl, continuous_dispatch::IssueOverlap,
    issues::ThreadIssue, rosen_work::IssueVersion,
};
use velocity_threads::continuous_dispatch::actionable_in_projects;
use velocity_threads::overlap_paths::{is_reference, widely_shared};

use super::{Reader, dispatch_claims, unix_seconds};
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
    let projects = lock(state.inner(), "persisted state")?
        .state()
        .projects
        .clone();
    let project = projects
        .iter()
        .find(|item| item.id == identifier)
        .ok_or("The project was removed")?;
    let mut waits = project.continuous_dispatch.issue_overlaps.clone();
    let mut released = project.continuous_dispatch.overlap_released.clone();
    if project.continuous_dispatch.slots == 0 && waits.is_empty() {
        return Ok(());
    }
    let claims = dispatch_claims(application, identifier)?;
    let repository = client.repository();
    let issues = &reader.backlog.issues;
    let now = unix_seconds();
    let working = active_issues(project, repository, issues, &claims, now);
    if working.is_empty() && waits.is_empty() {
        return Ok(());
    }
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    OverlapContext::new(client, project, &projects, &data).recheck(
        issues,
        working,
        &mut waits,
        &mut released,
        now,
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
    /// The files the change has edited against its parent, as Rosen reports them, less
    /// documents and the lists Rosen merges without a conflict.
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

/// Edited files by project and open change, with the time they were read.
type EditedFiles = BTreeMap<(u32, String), (Instant, BTreeSet<String>)>;

static EDITED_FILES: Mutex<EditedFiles> = Mutex::new(BTreeMap::new());

/// Each project's generated and registration lists, with the time they were read. They
/// change only when a `rosen.toml` edit lands, so the edited files' refresh covers them.
static MERGED_PATHS: Mutex<BTreeMap<u32, (Instant, MergedPaths)>> = Mutex::new(BTreeMap::new());

/// Cached base commit per project, refreshed on `Landed`/`Reverted` events or every 30 s (#555).
static BASE_COMMITS: Mutex<BTreeMap<u32, (Instant, String)>> = Mutex::new(BTreeMap::new());

struct CachedIssuePaths {
    base_commit: String,
    issues: BTreeMap<u32, (u64, BTreeSet<String>)>,
}

/// Cached graph-resolved paths per `(project, issue, base_commit, content_hash)` (#555).
static ISSUE_PATHS_CACHE: Mutex<BTreeMap<u32, CachedIssuePaths>> = Mutex::new(BTreeMap::new());

/// Drops the cached base commit for `project` when Rosen's base moves on land or revert (#555).
pub(crate) fn invalidate_base_commit(project: u32) {
    if let Ok(mut commits) = BASE_COMMITS.lock() {
        commits.remove(&project);
    }
}

fn issue_content_hash(issue: &GitHubIssue) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in issue
        .title
        .bytes()
        .chain(std::iter::once(0))
        .chain(issue.body.bytes())
    {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3);
    }
    hash
}

struct IssueCacheUpdate<'a> {
    project: u32,
    base_commit: String,
    issues: &'a [&'a GitHubIssue],
    hashes: &'a [u64],
    misses: &'a [usize],
    prune_to_backlog: bool,
}

fn store_cached_issue_paths(
    update: IssueCacheUpdate<'_>,
    resolved: Vec<BTreeSet<String>>,
    results: &mut [Option<BTreeSet<String>>],
) {
    if let Ok(mut cache) = lock(&ISSUE_PATHS_CACHE, "issue overlap path cache") {
        let entry = cache
            .entry(update.project)
            .or_insert_with(|| CachedIssuePaths {
                base_commit: update.base_commit.clone(),
                issues: BTreeMap::new(),
            });
        if entry.base_commit != update.base_commit {
            entry.base_commit = update.base_commit;
            entry.issues.clear();
        }
        for (&index, paths) in update.misses.iter().zip(resolved) {
            entry.issues.insert(
                update.issues[index].number,
                (update.hashes[index], paths.clone()),
            );
            results[index] = Some(paths);
        }
        if update.prune_to_backlog {
            let active: BTreeSet<u32> = update.issues.iter().map(|issue| issue.number).collect();
            entry.issues.retain(|number, _| active.contains(number));
        }
    } else {
        for (&index, paths) in update.misses.iter().zip(resolved) {
            results[index] = Some(paths);
        }
    }
}

/// The lists in force at the project's base. A failed read keeps the last lists read,
/// and lists never read exclude nothing: the hold is then what it was without them.
fn merged_paths(server: &ProjectServer, project: &ProjectItem) -> MergedPaths {
    if project.version_control != VersionControl::Rosen {
        return MergedPaths::default();
    }
    let Ok(mut read) = lock(&MERGED_PATHS, "the merged path lists") else {
        return MergedPaths::default();
    };
    let last = match read.get(&project.id) {
        Some((at, paths)) if at.elapsed() < EDITED_FILES_REFRESH => return paths.clone(),
        known => known.map(|(_, paths)| paths.clone()),
    };
    let paths = tauri::async_runtime::block_on(policy::merged_paths(server))
        .ok()
        .or(last)
        .unwrap_or_default();
    read.insert(project.id, (Instant::now(), paths.clone()));
    paths
}

fn active_issues(
    project: &ProjectItem,
    repository: &str,
    issues: &[GitHubIssue],
    claims: &[Claim],
    now: u64,
) -> BTreeMap<u32, WorkingIssue> {
    issues
        .iter()
        .filter(|issue| issue.state == IssueState::Open)
        .filter_map(|issue| {
            // A skipped external issue (already landed or in progress on another machine)
            // never acts as a local overlap blocker (#519).
            if is_skipped_external(project, repository, issue, now) {
                return None;
            }
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
    let mut context = OverlapContext::new(client, project, &[], data);
    let clear = context.clear_of_land(issue, source, commit)?.is_some();
    Ok(LandEvidence {
        files: context.land_files(commit)?,
        clear,
    })
}

struct OverlapContext<'a> {
    client: &'a IssueClient,
    project: &'a ProjectItem,
    projects: &'a [ProjectItem],
    data: &'a Path,
    server: ProjectServer,
    /// The generated artifacts and registration lists in the project's `rosen.toml`.
    merged: MergedPaths,
    /// The paths more than a few issues of the ready backlog name (#505). Empty until a
    /// recheck has read the backlog's paths, and for a land checked on its own.
    shared: BTreeSet<String>,
    /// Every file of each landed commit read so far, documents included.
    landed: BTreeMap<String, Vec<String>>,
}

impl<'a> OverlapContext<'a> {
    fn new(
        client: &'a IssueClient,
        project: &'a ProjectItem,
        projects: &'a [ProjectItem],
        data: &'a Path,
    ) -> Self {
        let server = ProjectServer::new(data, project.id);
        Self {
            client,
            project,
            projects,
            data,
            merged: merged_paths(&server, project),
            server,
            shared: BTreeSet::new(),
            landed: BTreeMap::new(),
        }
    }

    /// The paths that can hold an issue. Documents, the generated artifacts and
    /// registration lists Rosen merges without a conflict, and files most of the ready
    /// backlog names hold nothing: Rosen's claims and the merge settle edits there (#505).
    fn holding(&self, paths: impl IntoIterator<Item = String>) -> BTreeSet<String> {
        paths
            .into_iter()
            .filter(|path| {
                !is_reference(path) && !self.merged.contains(path) && !self.shared.contains(path)
            })
            .collect()
    }

    fn recheck(
        &mut self,
        issues: &[GitHubIssue],
        working: BTreeMap<u32, WorkingIssue>,
        waits: &mut BTreeMap<u32, IssueOverlap>,
        released: &mut BTreeMap<u32, ThreadIssue>,
        now: u64,
    ) -> Result<(), String> {
        let client = self.client;
        let repository = client.repository();
        let open = |issue: &ThreadIssue| {
            issue.repository.eq_ignore_ascii_case(repository)
                && issues.iter().any(|open| open.number == issue.number)
        };
        waits.retain(|_, wait| {
            open(&wait.issue)
                && !blocker_skipped_external(self.project, repository, issues, wait.blocker, now)
        });
        released.retain(|_, issue| open(issue));
        let blockers = self.blockers(&working)?;
        let (mut holdable, mut backlog) = (Vec::new(), Vec::new());
        for issue in issues {
            let number = issue.number;
            let worked = working.contains_key(&number);
            // A landed or remotely worked issue waits elsewhere, not for another issue,
            // and reviewing it against someone else's land only parks finished work (#505, #519).
            let skipped = !worked && is_skipped_external(self.project, repository, issue, now);
            if worked {
                end_hold(number, waits, released);
            } else if skipped {
                waits.remove(&number);
            } else if waits.contains_key(&number)
                || (!blockers.is_empty()
                    && actionable_in_projects(self.projects, self.project, repository, issue)
                    && !released.contains_key(&number))
            {
                holdable.push(issue);
            }
            // The ready backlog: every issue being worked, held or free for dispatch.
            if worked
                || waits.contains_key(&number)
                || (!skipped
                    && actionable_in_projects(self.projects, self.project, repository, issue))
            {
                backlog.push(issue);
            }
        }
        let mut named = self.named_paths(&backlog, &blockers);
        for issue in holdable {
            let paths = named.remove(&issue.number).unwrap_or_default();
            self.update(issue, paths, &blockers, waits, released)?;
        }
        Ok(())
    }

    /// The paths each issue of the ready backlog names that can hold it. The whole
    /// backlog is read, not only the issues that can be held, because a path's share of
    /// it decides whether the path counts. Only a hold needs the paths, so nothing is
    /// read without a blocker.
    fn named_paths(
        &mut self,
        backlog: &[&GitHubIssue],
        blockers: &BTreeMap<u32, Blocker>,
    ) -> BTreeMap<u32, BTreeSet<String>> {
        if blockers.is_empty() {
            return BTreeMap::new();
        }
        let named = self.cached_issue_paths(backlog, true);
        self.shared = widely_shared(&named);
        backlog
            .iter()
            .zip(named)
            .map(|(issue, paths)| (issue.number, self.holding(paths)))
            .collect()
    }

    /// Resolves `issues` against the graph service, reusing cached results for each
    /// `(issue, base_commit)` whose title and body are unchanged (#555).
    fn cached_issue_paths(
        &self,
        issues: &[&GitHubIssue],
        prune_to_backlog: bool,
    ) -> Vec<BTreeSet<String>> {
        if issues.is_empty() {
            return Vec::new();
        }
        let Some(base_commit) = self.base_commit() else {
            let descriptions: Vec<_> = issues.iter().map(|issue| issue_text(issue)).collect();
            return issue_paths(self.data, self.project, &descriptions);
        };
        let hashes: Vec<u64> = issues
            .iter()
            .map(|issue| issue_content_hash(issue))
            .collect();
        let mut results: Vec<Option<BTreeSet<String>>> = vec![None; issues.len()];
        let mut misses = Vec::new();
        if let Ok(cache) = lock(&ISSUE_PATHS_CACHE, "issue overlap path cache")
            && let Some(project_cache) = cache.get(&self.project.id)
            && project_cache.base_commit == base_commit
        {
            for (index, (issue, &hash)) in issues.iter().zip(&hashes).enumerate() {
                if let Some((cached_hash, paths)) = project_cache.issues.get(&issue.number)
                    && *cached_hash == hash
                {
                    results[index] = Some(paths.clone());
                } else {
                    misses.push(index);
                }
            }
        } else {
            misses.extend(0..issues.len());
        }
        if !misses.is_empty() {
            let descriptions: Vec<_> = misses
                .iter()
                .map(|&index| issue_text(issues[index]))
                .collect();
            let resolved = issue_paths(self.data, self.project, &descriptions);
            store_cached_issue_paths(
                IssueCacheUpdate {
                    project: self.project.id,
                    base_commit,
                    issues,
                    hashes: &hashes,
                    misses: &misses,
                    prune_to_backlog,
                },
                resolved,
                &mut results,
            );
        }
        results.into_iter().map(Option::unwrap_or_default).collect()
    }

    fn base_commit(&self) -> Option<String> {
        let last = if let Ok(commits) = lock(&BASE_COMMITS, "base commit cache") {
            match commits.get(&self.project.id) {
                Some((at, commit)) if at.elapsed() < EDITED_FILES_REFRESH => {
                    return Some(commit.clone());
                }
                known => known.map(|(_, commit)| commit.clone()),
            }
        } else {
            None
        };
        let resolved = if self.project.version_control == VersionControl::Rosen {
            tauri::async_runtime::block_on(velocity_rosen::post_land::BaseSnapshot::read(
                &self.server,
            ))
            .ok()
            .map(|base| base.commit().to_owned())
        } else {
            let root = Path::new(&self.project.repository_path);
            let branch = if self.project.default_branch.trim().is_empty() {
                "master"
            } else {
                self.project.default_branch.as_str()
            };
            velocity_git::resolve_commit(root, &format!("refs/remotes/origin/{branch}"))
                .or_else(|| velocity_git::resolve_commit(root, &format!("refs/heads/{branch}")))
        };
        let commit = resolved.or(last)?;
        if let Ok(mut commits) = lock(&BASE_COMMITS, "base commit cache") {
            commits.insert(self.project.id, (Instant::now(), commit.clone()));
        }
        Some(commit)
    }

    /// The working issues that can hold another: each has an open Rosen change that has
    /// edited a file that can hold. Predicted paths and claimed paths never hold an
    /// issue (#525).
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
                .map(|paths| paths.into_iter().collect())
                .unwrap_or_else(|_| last.unwrap_or_default());
                read.insert(key, (Instant::now(), edited.clone()));
                edited
            });
            let edited = self.holding(edited);
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
        let ready =
            actionable_in_projects(self.projects, self.project, self.client.repository(), issue);
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
        let Some(commit) = blocker_landed_commit(self.project, wait).map(str::to_owned) else {
            return Ok(LandCheck::Waiting);
        };
        let issue = self.client.get(number).map_err(|error| error.to_string())?;
        if !actionable_in_projects(
            self.projects,
            self.project,
            self.client.repository(),
            &issue,
        ) {
            return Ok(LandCheck::Waiting);
        }
        Ok(match self.clear_of_land(&issue, wait.blocker, &commit)? {
            Some(files) => LandCheck::Clear { commit, files },
            None => LandCheck::Review(commit),
        })
    }

    /// The land's files when `commit`, landed for issue `source`, changed none of `issue`'s
    /// predicted paths that can hold it and the two issues describe different problems. `None` when
    /// the land shares code with the issue or may have fixed it, so a review has to decide.
    fn clear_of_land(
        &mut self,
        issue: &GitHubIssue,
        source: u32,
        commit: &str,
    ) -> Result<Option<Vec<String>>, String> {
        let source = self.client.get(source).map_err(|error| error.to_string())?;
        let named = self.cached_issue_paths(&[issue, &source], false);
        let mut paths = named.into_iter().map(|paths| self.holding(paths));
        let wanted = problem(issue, paths.next().unwrap_or_default());
        if wanted.matches(&problem(&source, paths.next().unwrap_or_default())) {
            return Ok(None);
        }
        let files = self.land_files(commit)?;
        let landed_code = self.holding(files.iter().cloned());
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

/// Whether `issue` is already landed or actively worked on another machine (#504, #519).
fn is_skipped_external(
    project: &ProjectItem,
    repository: &str,
    issue: &GitHubIssue,
    now: u64,
) -> bool {
    let identity = ThreadIssue::new(repository.into(), issue.number);
    has_landed(
        project,
        repository,
        issue.number,
        IssueVersion::from_github(issue),
    ) || project
        .remote_work_hold(&identity, Some(&issue.updated_at), now)
        .is_some()
}

fn blocker_skipped_external(
    project: &ProjectItem,
    repository: &str,
    issues: &[GitHubIssue],
    blocker: u32,
    now: u64,
) -> bool {
    let identity = ThreadIssue::new(repository.into(), blocker);
    let issue = issues.iter().find(|issue| issue.number == blocker);
    let updated_at = issue.map(|issue| issue.updated_at.as_str());
    let version = issue.map(IssueVersion::from_github).unwrap_or_default();
    has_landed(project, repository, blocker, version)
        || project
            .remote_work_hold(&identity, updated_at, now)
            .is_some()
}

/// Whether `number` has a landed change of its own, from a thread, the work projection
/// or the GitHub land record dispatch saved (#504, #602).
fn has_landed(
    project: &ProjectItem,
    repository: &str,
    number: u32,
    version: IssueVersion<'_>,
) -> bool {
    let identity = ThreadIssue::new(repository.into(), number);
    let key = velocity_core::work_pool::WorkKey::issue(repository, number);
    if project.rosen_work_item(&key).is_some() {
        return project.rosen_issue_completed_for_version(&identity, version)
            || project
                .landed_awaiting_integration_for_version(&identity, version)
                .is_some();
    }
    project.rosen_issue_completed_for_version(&identity, version)
        || project
            .landed_awaiting_integration_for_version(&identity, version)
            .is_some()
        || project.threads.iter().any(|thread| {
            thread
                .issue
                .as_ref()
                .is_some_and(|issue| issue.number == number)
                && thread.rosen_issue.landed_commit().is_some()
        })
        || project.retained_rosen_issues.values().any(|retained| {
            retained.issue.number == number
                && retained.issue.repository.eq_ignore_ascii_case(repository)
                && retained.progress.landed_commit().is_some()
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

fn blocker_landed_commit<'a>(project: &'a ProjectItem, wait: &IssueOverlap) -> Option<&'a str> {
    blocker_thread(project, wait)
        .and_then(|thread| thread.rosen_issue.landed_commit())
        .or_else(|| {
            project
                .retained_rosen_issues
                .values()
                .find(|retained| {
                    retained.issue.number == wait.blocker
                        && retained
                            .issue
                            .repository
                            .eq_ignore_ascii_case(&wait.issue.repository)
                })
                .and_then(|retained| retained.progress.landed_commit())
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
        .and_then(|project| blocker_landed_commit(project, wait))
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
