//! Keep an issue out of continuous dispatch while another issue's open Rosen change edits
//! files it names. An issue is held this way once: when the hold ends it is dispatched.
use std::collections::{BTreeMap, BTreeSet, HashSet};
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
use velocity_threads::continuous_dispatch::{
    TextBlockerContext, actionable_in_projects_with_blockers,
};
use velocity_threads::overlap_paths::{explicit_file_paths, is_reference, widely_shared};

use super::{Reader, dispatch_claims, text_blockers, unix_seconds};
use crate::{
    graph_access::{issue_files, issue_paths},
    state::ManagedStateStore,
    threads::{lock, report},
};

/// How long a failed per-issue overlap host request waits before retrying independently (#651).
const ISSUE_ERROR_RETRY: Duration = Duration::from_secs(60);

#[derive(Clone, Debug)]
struct FailedLookup {
    updated_at: String,
    failed_at: Instant,
}

/// Per-project in-memory retry state for overlap checks so a host failure on one issue
/// never stalls unrelated continuous dispatch candidates (#651).
#[derive(Default)]
pub(super) struct Overlap {
    /// Issues whose overlap-history lookup failed while overlapping an active blocker.
    /// Held out of dispatch until retry succeeds or the overlap ends.
    review_lookup_retry: BTreeMap<u32, FailedLookup>,
    /// Already-waiting issues whose review-history or landed-blocker check failed.
    wait_retry: BTreeMap<u32, Instant>,
    /// Already-waiting issues whose overlap review publication failed.
    publish_retry: BTreeMap<u32, Instant>,
}

impl Overlap {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn clear(&mut self) {
        self.review_lookup_retry.clear();
        self.wait_retry.clear();
        self.publish_retry.clear();
    }

    pub(super) fn include_deferred(&self, claimed: &mut HashSet<String>) {
        claimed.extend(
            self.review_lookup_retry
                .keys()
                .map(|number| format!("issue:{number}")),
        );
    }

    pub(super) fn is_deferred(&self, number: u32) -> bool {
        self.review_lookup_retry.contains_key(&number)
    }

    fn lookup_retry_active_at(&self, number: u32, updated_at: &str, now: Instant) -> bool {
        self.review_lookup_retry.get(&number).is_some_and(|failed| {
            failed.updated_at == updated_at
                && now.saturating_duration_since(failed.failed_at) < ISSUE_ERROR_RETRY
        })
    }

    fn record_lookup_failure_at(&mut self, number: u32, updated_at: String, now: Instant) {
        self.review_lookup_retry.insert(
            number,
            FailedLookup {
                updated_at,
                failed_at: now,
            },
        );
    }

    fn clear_lookup_failure(&mut self, number: u32) {
        self.review_lookup_retry.remove(&number);
    }

    fn retain_review_lookups(&mut self, deferred: &BTreeSet<u32>) {
        self.review_lookup_retry
            .retain(|number, _| deferred.contains(number));
    }

    fn wait_retry_active_at(&self, number: u32, now: Instant) -> bool {
        self.wait_retry
            .get(&number)
            .is_some_and(|&failed_at| now.saturating_duration_since(failed_at) < ISSUE_ERROR_RETRY)
    }

    fn record_wait_failure_at(&mut self, number: u32, now: Instant) {
        self.wait_retry.insert(number, now);
    }

    fn clear_wait_failure(&mut self, number: u32) {
        self.wait_retry.remove(&number);
    }

    fn publish_retry_active_at(&self, number: u32, now: Instant) -> bool {
        self.publish_retry
            .get(&number)
            .is_some_and(|&failed_at| now.saturating_duration_since(failed_at) < ISSUE_ERROR_RETRY)
    }

    fn record_publish_failure_at(&mut self, number: u32, now: Instant) {
        self.publish_retry.insert(number, now);
    }

    fn clear_publish_failure(&mut self, number: u32) {
        self.publish_retry.remove(&number);
    }

    fn retain_waits(&mut self, waits: &BTreeMap<u32, IssueOverlap>) {
        self.wait_retry
            .retain(|number, _| waits.contains_key(number));
        self.publish_retry
            .retain(|number, _| waits.contains_key(number));
    }
}

pub(super) fn refresh(
    application: &AppHandle,
    identifier: u32,
    reader: &mut Reader,
) -> Result<(), String> {
    if reader.client.is_none() || reader.backlog.error.is_some() {
        return Ok(());
    }
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
        reader.overlap.clear();
        return Ok(());
    }
    let claims = dispatch_claims(application, identifier)?;
    let Some(client) = reader.client.as_ref() else {
        return Ok(());
    };
    let repository = client.repository().to_owned();
    let now = unix_seconds();
    let working = active_issues(project, &repository, &reader.backlog.issues, &claims, now);
    if working.is_empty() && waits.is_empty() {
        reader.overlap.clear();
        return Ok(());
    }
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let blockers_context =
        text_blockers::context_for_project(application, identifier, Some(&reader.backlog));
    OverlapContext::new(
        client,
        project,
        &projects,
        &data,
        blockers_context,
        &mut reader.overlap,
    )
    .recheck(
        &reader.backlog.issues,
        working,
        &mut waits,
        &mut released,
        now,
    )?;
    lock(state.inner(), "persisted state")?
        .save_issue_overlaps(identifier, waits.clone(), released.clone())
        .map_err(|error| error.to_string())?;
    let reviewed = publish_reviews(
        application,
        identifier,
        client,
        &mut waits,
        &mut reader.overlap,
    )?;
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
                .map(|&number| (number, ThreadIssue::new(repository.clone(), number))),
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

/// Cached graph-predicted paths per `(project, issue, base_commit, content_hash)` for
/// landed-change reviews (#555, #649).
static ISSUE_PATHS_CACHE: Mutex<BTreeMap<u32, CachedIssuePaths>> = Mutex::new(BTreeMap::new());

/// Cached concrete file references per `(project, issue, base_commit, content_hash)` for
/// live overlap holds (#555, #649).
static ISSUE_FILES_CACHE: Mutex<BTreeMap<u32, CachedIssuePaths>> = Mutex::new(BTreeMap::new());

/// Issues whose comments were already checked for a prior returned overlap review at
/// `(repository, updated_at)`, per project (#650). Only negative results are cached:
/// a positive result immediately ends the hold and records the issue in `overlap_released`.
type ReviewChecked = BTreeMap<u32, BTreeMap<u32, (String, String)>>;

static REVIEW_CHECKED: Mutex<ReviewChecked> = Mutex::new(BTreeMap::new());

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
    cache: &Mutex<BTreeMap<u32, CachedIssuePaths>>,
    update: IssueCacheUpdate<'_>,
    resolved: Vec<BTreeSet<String>>,
    results: &mut [Option<BTreeSet<String>>],
) {
    if let Ok(mut cache) = lock(cache, "issue overlap path cache") {
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
    let mut overlap = Overlap::new();
    let mut context = OverlapContext::new(
        client,
        project,
        &[],
        data,
        TextBlockerContext::default(),
        &mut overlap,
    );
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
    blockers_context: TextBlockerContext,
    overlap: &'a mut Overlap,
    /// Candidates deferred during this pass because their overlap-history lookup failed (#651).
    deferred: BTreeSet<u32>,
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
        blockers_context: TextBlockerContext,
        overlap: &'a mut Overlap,
    ) -> Self {
        let server = ProjectServer::new(data, project.id);
        Self {
            client,
            project,
            projects,
            data,
            blockers_context,
            overlap,
            deferred: BTreeSet::new(),
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
                    && actionable_in_projects_with_blockers(
                        self.projects,
                        self.project,
                        repository,
                        issue,
                        &self.blockers_context,
                    )
                    && !released.contains_key(&number))
            {
                holdable.push(issue);
            }
            // The ready backlog: every issue being worked, held or free for dispatch.
            if worked
                || waits.contains_key(&number)
                || (!skipped
                    && actionable_in_projects_with_blockers(
                        self.projects,
                        self.project,
                        repository,
                        issue,
                        &self.blockers_context,
                    ))
            {
                backlog.push(issue);
            }
        }
        let mut named = self.named_paths(&backlog, &blockers);
        for issue in holdable {
            let paths = named.remove(&issue.number).unwrap_or_default();
            self.update(issue, paths, &blockers, waits, released);
        }
        self.overlap.retain_review_lookups(&self.deferred);
        self.overlap.retain_waits(waits);
        self.prune_review_checked(waits);
        Ok(())
    }

    /// The concrete code files each issue of the ready backlog names that can hold it
    /// (#649). The whole backlog is read, not only the issues that can be held, because a
    /// path's share of it decides whether the path counts. Only a hold needs the paths, so
    /// nothing is read without a blocker.
    fn named_paths(
        &mut self,
        backlog: &[&GitHubIssue],
        blockers: &BTreeMap<u32, Blocker>,
    ) -> BTreeMap<u32, BTreeSet<String>> {
        if blockers.is_empty() {
            return BTreeMap::new();
        }
        let named = self.cached_issue_files(backlog);
        self.shared = widely_shared(&named);
        backlog
            .iter()
            .zip(named)
            .map(|(issue, paths)| (issue.number, self.holding(paths)))
            .collect()
    }

    /// Resolves concrete file references named by `issues`, reusing cached results per
    /// `(issue, base_commit)` without inferring files from symbol names (#649).
    fn cached_issue_files(&self, issues: &[&GitHubIssue]) -> Vec<BTreeSet<String>> {
        self.cached_paths(&ISSUE_FILES_CACHE, issues, true, |descriptions| {
            let mut resolved = issue_files(self.data, self.project, descriptions);
            for (paths, &(title, body)) in resolved.iter_mut().zip(descriptions) {
                paths.extend(explicit_file_paths(title));
                paths.extend(explicit_file_paths(body));
            }
            resolved
        })
    }

    /// Resolves `issues` against the graph service with symbol-based prediction for
    /// landed-change review checks, reusing cached results per `(issue, base_commit)` (#555).
    fn cached_issue_paths(&self, issues: &[&GitHubIssue]) -> Vec<BTreeSet<String>> {
        self.cached_paths(&ISSUE_PATHS_CACHE, issues, false, |descriptions| {
            issue_paths(self.data, self.project, descriptions)
        })
    }

    fn cached_paths(
        &self,
        cache: &Mutex<BTreeMap<u32, CachedIssuePaths>>,
        issues: &[&GitHubIssue],
        prune_to_backlog: bool,
        resolve: impl FnOnce(&[(&str, &str)]) -> Vec<BTreeSet<String>>,
    ) -> Vec<BTreeSet<String>> {
        if issues.is_empty() {
            return Vec::new();
        }
        let Some(base_commit) = self.base_commit() else {
            let descriptions: Vec<_> = issues.iter().map(|issue| issue_text(issue)).collect();
            return resolve(&descriptions);
        };
        let hashes: Vec<u64> = issues
            .iter()
            .map(|issue| issue_content_hash(issue))
            .collect();
        let mut results: Vec<Option<BTreeSet<String>>> = vec![None; issues.len()];
        let mut misses = Vec::new();
        if let Ok(cache) = lock(cache, "issue overlap path cache")
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
            let resolved = resolve(&descriptions);
            store_cached_issue_paths(
                cache,
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
    ) {
        let number = issue.number;
        if self.reconcile_wait(issue, waits, released) {
            return;
        }
        let ready = actionable_in_projects_with_blockers(
            self.projects,
            self.project,
            self.client.repository(),
            issue,
            &self.blockers_context,
        );
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
            return;
        }
        if ready && !released.contains_key(&number) {
            self.hold(issue, &paths, blockers, waits, released);
        }
    }

    /// Reconciles an existing wait before updating its live overlap paths. Returns `true`
    /// when `update` should stop for this issue: the wait was released, a review is pending,
    /// or a per-issue host error kept the wait for an independent retry (#650, #651).
    fn reconcile_wait(
        &mut self,
        issue: &GitHubIssue,
        waits: &mut BTreeMap<u32, IssueOverlap>,
        released: &mut BTreeMap<u32, ThreadIssue>,
    ) -> bool {
        let number = issue.number;
        if !waits.contains_key(&number) {
            return false;
        }
        let now = Instant::now();
        if self.overlap.wait_retry_active_at(number, now) {
            return true;
        }
        let client = self.client;
        let project = self.project.id;
        // Reconcile restored waits against GitHub's durable review record before
        // retaining them, checking a blocker land, or publishing another review (#650).
        match release_returned_wait(number, waits, released, || {
            overlap_review_returned_for(client, project, issue)
        }) {
            Ok(true) => {
                self.overlap.clear_wait_failure(number);
                return true;
            }
            Ok(false) => {}
            Err(error) => {
                self.overlap.record_wait_failure_at(number, now);
                report::<()>(Err(format!(
                    "Could not check overlap review history for {}#{number}: {error}",
                    client.repository()
                )));
                return true;
            }
        }
        match self.check_landed_wait(number, waits) {
            Ok(pending_review) => {
                self.overlap.clear_wait_failure(number);
                pending_review
            }
            Err(error) => {
                self.overlap.record_wait_failure_at(number, now);
                report::<()>(Err(format!(
                    "Could not check overlapping land for {}#{number}: {error}",
                    client.repository()
                )));
                true
            }
        }
    }

    /// Checks whether an existing wait's blocker has landed. Returns `true` while a
    /// review is pending (already recorded or just scheduled) so `publish_reviews` posts it.
    fn check_landed_wait(
        &mut self,
        number: u32,
        waits: &mut BTreeMap<u32, IssueOverlap>,
    ) -> Result<bool, String> {
        let Some(wait) = waits.get_mut(&number) else {
            return Ok(false);
        };
        if wait.review_commit.is_some() {
            return Ok(true);
        }
        match self.overlapping_land(wait, number)? {
            LandCheck::Review(commit) => {
                wait.review_commit = Some(commit);
                Ok(true)
            }
            // The evidence is mechanical, so no review parks the issue. The comment
            // is posted before the wait goes; a failure keeps the wait for a retry.
            LandCheck::Clear { commit, files } => {
                self.client
                    .post_overlap_clear(number, wait.blocker, &commit, &files)
                    .map_err(|error| error.to_string())?;
                Ok(false)
            }
            LandCheck::Waiting => Ok(false),
        }
    }

    fn prune_review_checked(&self, waits: &BTreeMap<u32, IssueOverlap>) {
        prune_review_checked_in(&REVIEW_CHECKED, self.project.id, waits);
    }

    /// Holds `issue` behind the first working issue whose open change has edited a file
    /// the issue names. Without such a file the issue stays free for dispatch. A failed
    /// overlap-history lookup is isolated to `issue` and retried independently (#651).
    fn hold(
        &mut self,
        issue: &GitHubIssue,
        paths: &BTreeSet<String>,
        blockers: &BTreeMap<u32, Blocker>,
        waits: &mut BTreeMap<u32, IssueOverlap>,
        released: &mut BTreeMap<u32, ThreadIssue>,
    ) {
        let number = issue.number;
        let client = self.client;
        let project = self.project.id;
        let repository = client.repository().to_owned();
        let outcome = resolve_overlap_hold(
            OverlapHoldTarget {
                repository: &repository,
                number,
                updated_at: &issue.updated_at,
                paths,
                now: Instant::now(),
            },
            blockers,
            self.overlap,
            waits,
            released,
            || overlap_review_returned_for(client, project, issue),
        );
        match outcome {
            HoldOutcome::Deferred(Some(error)) => {
                self.deferred.insert(number);
                report::<()>(Err(format!(
                    "Could not check overlap review history for {repository}#{number}: {error}"
                )));
            }
            HoldOutcome::Deferred(None) => {
                self.deferred.insert(number);
            }
            HoldOutcome::Clear | HoldOutcome::Held | HoldOutcome::Released => {}
        }
    }

    fn overlapping_land(&mut self, wait: &IssueOverlap, number: u32) -> Result<LandCheck, String> {
        let Some(commit) = blocker_landed_commit(self.project, wait).map(str::to_owned) else {
            return Ok(LandCheck::Waiting);
        };
        let issue = self.client.get(number).map_err(|error| error.to_string())?;
        if !actionable_in_projects_with_blockers(
            self.projects,
            self.project,
            self.client.repository(),
            &issue,
            &self.blockers_context,
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
        let named = self.cached_issue_paths(&[issue, &source]);
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

/// Whether `issue`'s comments show a prior overlap review that returned it to
/// dispatch (#525, #650). Negative results are cached per `(repository, updated_at)`
/// so active waits do not re-fetch comments on every dispatch pass.
fn overlap_review_returned_for(
    client: &IssueClient,
    project: u32,
    issue: &GitHubIssue,
) -> Result<bool, String> {
    let repository = client.repository();
    if review_checked_hit(
        &REVIEW_CHECKED,
        project,
        issue.number,
        repository,
        &issue.updated_at,
    ) {
        return Ok(false);
    }
    if client
        .overlap_review_returned(issue.number)
        .map_err(|error| error.to_string())?
    {
        clear_review_checked(&REVIEW_CHECKED, project, issue.number);
        return Ok(true);
    }
    record_review_checked_in(
        &REVIEW_CHECKED,
        project,
        issue.number,
        repository,
        &issue.updated_at,
    );
    Ok(false)
}

struct OverlapHoldTarget<'a> {
    repository: &'a str,
    number: u32,
    updated_at: &'a str,
    paths: &'a BTreeSet<String>,
    now: Instant,
}

#[derive(Debug, PartialEq, Eq)]
enum HoldOutcome {
    Clear,
    Held,
    Released,
    Deferred(Option<String>),
}

/// Evaluates whether `target` should be held behind an active blocker, released because a
/// prior overlap review already returned it, or deferred independently after a failed
/// overlap-history lookup (#525, #651).
fn resolve_overlap_hold(
    target: OverlapHoldTarget<'_>,
    blockers: &BTreeMap<u32, Blocker>,
    overlap_state: &mut Overlap,
    waits: &mut BTreeMap<u32, IssueOverlap>,
    released: &mut BTreeMap<u32, ThreadIssue>,
    returned_review: impl FnOnce() -> Result<bool, String>,
) -> HoldOutcome {
    let overlap = blockers
        .iter()
        .map(|(&blocker, peer)| (blocker, peer.thread, shared_files(target.paths, peer)))
        .find(|(_, _, shared)| !shared.is_empty());
    let Some((blocker, thread, shared)) = overlap else {
        overlap_state.clear_lookup_failure(target.number);
        return HoldOutcome::Clear;
    };
    if overlap_state.lookup_retry_active_at(target.number, target.updated_at, target.now) {
        return HoldOutcome::Deferred(None);
    }
    let identity = ThreadIssue::new(target.repository.into(), target.number);
    // GitHub's record outlives local state: an issue a review already returned from
    // an overlapping land is dispatched, not held again (#525). Isolate lookup errors
    // to this issue so unrelated ready candidates still dispatch (#651).
    match returned_review() {
        Ok(true) => {
            overlap_state.clear_lookup_failure(target.number);
            released.insert(target.number, identity);
            HoldOutcome::Released
        }
        Ok(false) => {
            overlap_state.clear_lookup_failure(target.number);
            waits.insert(
                target.number,
                IssueOverlap::new(identity, blocker, Some(thread), shared),
            );
            HoldOutcome::Held
        }
        Err(error) => {
            overlap_state.record_lookup_failure_at(
                target.number,
                target.updated_at.to_owned(),
                target.now,
            );
            HoldOutcome::Deferred(Some(error))
        }
    }
}

/// Drops an existing wait before retaining it, checking a blocker land, or publishing a
/// review when the issue was already released or its GitHub comments show a prior
/// returned-to-dispatch overlap review (#650).
fn release_returned_wait(
    number: u32,
    waits: &mut BTreeMap<u32, IssueOverlap>,
    released: &mut BTreeMap<u32, ThreadIssue>,
    returned_review: impl FnOnce() -> Result<bool, String>,
) -> Result<bool, String> {
    if !waits.contains_key(&number) {
        return Ok(false);
    }
    if released.contains_key(&number) || returned_review()? {
        end_hold(number, waits, released);
        return Ok(true);
    }
    Ok(false)
}

fn review_checked_hit(
    cache: &Mutex<ReviewChecked>,
    project: u32,
    number: u32,
    repository: &str,
    updated_at: &str,
) -> bool {
    lock(cache, "overlap review cache").is_ok_and(|checked| {
        checked
            .get(&project)
            .and_then(|issues| issues.get(&number))
            .is_some_and(|(saved_repo, saved_updated)| {
                saved_repo.eq_ignore_ascii_case(repository) && saved_updated == updated_at
            })
    })
}

fn record_review_checked_in(
    cache: &Mutex<ReviewChecked>,
    project: u32,
    number: u32,
    repository: &str,
    updated_at: &str,
) {
    if let Ok(mut checked) = lock(cache, "overlap review cache") {
        checked
            .entry(project)
            .or_default()
            .insert(number, (repository.to_owned(), updated_at.to_owned()));
    }
}

fn clear_review_checked(cache: &Mutex<ReviewChecked>, project: u32, number: u32) {
    if let Ok(mut checked) = lock(cache, "overlap review cache")
        && let Some(issues) = checked.get_mut(&project)
    {
        issues.remove(&number);
    }
}

fn prune_review_checked_in(
    cache: &Mutex<ReviewChecked>,
    project: u32,
    waits: &BTreeMap<u32, IssueOverlap>,
) {
    if let Ok(mut checked) = lock(cache, "overlap review cache") {
        if waits.is_empty() {
            checked.remove(&project);
        } else if let Some(issues) = checked.get_mut(&project) {
            issues.retain(|number, _| waits.contains_key(number));
        }
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
    overlap: &mut Overlap,
) -> Result<Vec<u32>, String> {
    let mut completed = Vec::new();
    let now = Instant::now();
    for wait in waits.values() {
        let Some(commit) = wait.review_commit.as_deref() else {
            continue;
        };
        if overlap.publish_retry_active_at(wait.issue.number, now) {
            continue;
        }
        let still_landed = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .project(identifier)
        .and_then(|project| blocker_landed_commit(project, wait))
            == Some(commit);
        if still_landed && let Err(error) = publish_one_review(client, wait, commit) {
            overlap.record_publish_failure_at(wait.issue.number, now);
            report::<()>(Err(format!(
                "Could not publish overlap review for {}#{}: {error}",
                client.repository(),
                wait.issue.number
            )));
            continue;
        }
        overlap.clear_publish_failure(wait.issue.number);
        completed.push(wait.issue.number);
    }
    for number in &completed {
        waits.remove(number);
    }
    overlap.retain_waits(waits);
    // The parked issue gets its bounded review once the label shows in the backlog (#506).
    Ok(completed)
}

fn publish_one_review(
    client: &IssueClient,
    wait: &IssueOverlap,
    commit: &str,
) -> Result<(), String> {
    let open = client
        .get(wait.issue.number)
        .map_err(|error| error.to_string())?
        .state
        == IssueState::Open;
    if open
        && !client
            .overlap_review_returned(wait.issue.number)
            .map_err(|error| error.to_string())?
    {
        client
            .post_overlap_review(wait.issue.number, wait.blocker, commit)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn restored_wait(number: u32, blocker: u32, review_commit: Option<&str>) -> IssueOverlap {
        let mut wait = IssueOverlap::new(
            ThreadIssue::new("owner/repo".into(), number),
            blocker,
            Some(7),
            Vec::new(),
        );
        wait.review_commit = review_commit.map(str::to_owned);
        wait
    }

    // Risk (#650): a pre-upgrade repeat IssueOverlap persisted after an earlier returned
    // overlap review must be dropped and moved to `overlap_released` before it can be
    // retained behind a live blocker or published as another review.
    #[test]
    fn restored_wait_with_prior_returned_review_is_released() -> Result<(), String> {
        let mut waits = BTreeMap::from([(105, restored_wait(105, 106, None))]);
        let mut released = BTreeMap::new();
        let dropped = release_returned_wait(105, &mut waits, &mut released, || Ok(true))?;
        assert!(dropped);
        assert!(waits.is_empty());
        assert_eq!(
            released.get(&105),
            Some(&ThreadIssue::new("owner/repo".into(), 105))
        );
        Ok(())
    }

    // Risk (#650): a restored wait that already recorded a landed blocker commit must also
    // be dropped before `publish_reviews` can post a second overlap review.
    #[test]
    fn restored_pending_review_with_prior_returned_review_is_released() -> Result<(), String> {
        let mut waits = BTreeMap::from([(105, restored_wait(105, 106, Some("deadbeef")))]);
        let mut released = BTreeMap::new();
        let dropped = release_returned_wait(105, &mut waits, &mut released, || Ok(true))?;
        assert!(dropped);
        assert!(waits.is_empty());
        assert!(released.contains_key(&105));
        Ok(())
    }

    // Risk (#650): a valid wait with no prior returned review stays held and propagates
    // lookup errors without silently dropping the wait.
    #[test]
    fn valid_wait_is_retained_and_lookup_errors_preserve_wait() -> Result<(), String> {
        let mut waits = BTreeMap::from([(105, restored_wait(105, 106, None))]);
        let mut released = BTreeMap::new();
        assert!(!release_returned_wait(
            105,
            &mut waits,
            &mut released,
            || Ok(false),
        )?);
        assert!(waits.contains_key(&105));
        assert!(released.is_empty());

        let result = release_returned_wait(105, &mut waits, &mut released, || {
            Err("temporary API error".into())
        });
        assert!(result.is_err());
        assert!(waits.contains_key(&105));
        assert!(released.is_empty());
        Ok(())
    }

    // Risk (#650): negative overlap-review lookups are cached per `(repository, updated_at)`
    // so 1-second dispatch passes do not re-fetch comments, while updates or pruning
    // invalidate stale entries.
    #[test]
    fn review_checked_cache_tracks_updated_at_and_prunes() {
        let cache = Mutex::new(BTreeMap::new());
        assert!(!review_checked_hit(
            &cache,
            1,
            105,
            "owner/repo",
            "2026-01-01T00:00:00Z"
        ));
        record_review_checked_in(&cache, 1, 105, "owner/repo", "2026-01-01T00:00:00Z");
        assert!(review_checked_hit(
            &cache,
            1,
            105,
            "Owner/Repo",
            "2026-01-01T00:00:00Z"
        ));
        assert!(!review_checked_hit(
            &cache,
            1,
            105,
            "owner/repo",
            "2026-01-01T00:05:00Z"
        ));
        let empty = BTreeMap::new();
        prune_review_checked_in(&cache, 1, &empty);
        assert!(!review_checked_hit(
            &cache,
            1,
            105,
            "owner/repo",
            "2026-01-01T00:00:00Z"
        ));
    }

    // Risk (#651): a comment-history read failure for one overlapping candidate (#126) must
    // isolate only that candidate with independent backoff and retry so unrelated ready
    // candidates (#133) remain free for dispatch.
    #[test]
    fn failed_overlap_lookup_isolates_candidate_and_retries_independently() {
        let blockers = BTreeMap::from([(
            100,
            Blocker {
                thread: 1,
                edited: BTreeSet::from(["src/shared.rs".to_owned()]),
            },
        )]);
        let shared = BTreeSet::from(["src/shared.rs".to_owned()]);
        let unrelated = BTreeSet::from(["src/other.rs".to_owned()]);
        let mut overlap = Overlap::new();
        let mut waits = BTreeMap::new();
        let mut released = BTreeMap::new();
        let start = Instant::now();

        let first = resolve_overlap_hold(
            OverlapHoldTarget {
                repository: "owner/repo",
                number: 126,
                updated_at: "2026-11-22T00:00:00Z",
                paths: &shared,
                now: start,
            },
            &blockers,
            &mut overlap,
            &mut waits,
            &mut released,
            || Err("HTTP 500".into()),
        );
        assert_eq!(first, HoldOutcome::Deferred(Some("HTTP 500".into())));
        assert!(overlap.is_deferred(126));
        assert!(!waits.contains_key(&126));
        assert!(!released.contains_key(&126));

        let other = resolve_overlap_hold(
            OverlapHoldTarget {
                repository: "owner/repo",
                number: 133,
                updated_at: "2026-11-22T00:00:00Z",
                paths: &unrelated,
                now: start,
            },
            &blockers,
            &mut overlap,
            &mut waits,
            &mut released,
            || panic!("non-overlapping candidate should not fetch comments"),
        );
        assert_eq!(other, HoldOutcome::Clear);
        let mut claimed = HashSet::new();
        overlap.include_deferred(&mut claimed);
        assert!(claimed.contains("issue:126"));
        assert!(!claimed.contains("issue:133"));

        // Within the retry window at the same `updated_at`, #126 stays isolated without re-querying.
        let backed_off = resolve_overlap_hold(
            OverlapHoldTarget {
                repository: "owner/repo",
                number: 126,
                updated_at: "2026-11-22T00:00:00Z",
                paths: &shared,
                now: start + Duration::from_secs(30),
            },
            &blockers,
            &mut overlap,
            &mut waits,
            &mut released,
            || panic!("lookup should stay in retry backoff"),
        );
        assert_eq!(backed_off, HoldOutcome::Deferred(None));

        // After the retry interval, #126 retries independently and records the result.
        let retried = resolve_overlap_hold(
            OverlapHoldTarget {
                repository: "owner/repo",
                number: 126,
                updated_at: "2026-11-22T00:00:00Z",
                paths: &shared,
                now: start + ISSUE_ERROR_RETRY,
            },
            &blockers,
            &mut overlap,
            &mut waits,
            &mut released,
            || Ok(true),
        );
        assert_eq!(retried, HoldOutcome::Released);
        assert!(!overlap.is_deferred(126));
        assert!(released.contains_key(&126));
    }

    // Risk (#651): a deferred candidate clears immediately when its blocker no longer
    // overlaps or when a new issue version retries and confirms a hold.
    #[test]
    fn deferred_overlap_lookup_clears_on_update_or_ended_overlap() {
        let blockers = BTreeMap::from([(
            100,
            Blocker {
                thread: 1,
                edited: BTreeSet::from(["src/shared.rs".to_owned()]),
            },
        )]);
        let shared = BTreeSet::from(["src/shared.rs".to_owned()]);
        let mut overlap = Overlap::new();
        let mut waits = BTreeMap::new();
        let mut released = BTreeMap::new();
        let start = Instant::now();

        resolve_overlap_hold(
            OverlapHoldTarget {
                repository: "owner/repo",
                number: 126,
                updated_at: "2026-11-22T00:00:00Z",
                paths: &shared,
                now: start,
            },
            &blockers,
            &mut overlap,
            &mut waits,
            &mut released,
            || Err("timeout".into()),
        );
        assert!(overlap.is_deferred(126));

        // A newer `updated_at` retries immediately rather than waiting out the backoff.
        let held = resolve_overlap_hold(
            OverlapHoldTarget {
                repository: "owner/repo",
                number: 126,
                updated_at: "2026-11-22T00:00:10Z",
                paths: &shared,
                now: start + Duration::from_secs(5),
            },
            &blockers,
            &mut overlap,
            &mut waits,
            &mut released,
            || Ok(false),
        );
        assert_eq!(held, HoldOutcome::Held);
        assert!(!overlap.is_deferred(126));
        assert!(waits.contains_key(&126));

        // Another candidate whose overlap disappears clears its deferral immediately.
        resolve_overlap_hold(
            OverlapHoldTarget {
                repository: "owner/repo",
                number: 127,
                updated_at: "2026-11-22T00:00:00Z",
                paths: &shared,
                now: start,
            },
            &blockers,
            &mut overlap,
            &mut waits,
            &mut released,
            || Err("timeout".into()),
        );
        assert!(overlap.is_deferred(127));
        let cleared = resolve_overlap_hold(
            OverlapHoldTarget {
                repository: "owner/repo",
                number: 127,
                updated_at: "2026-11-22T00:00:00Z",
                paths: &shared,
                now: start + Duration::from_secs(5),
            },
            &BTreeMap::new(),
            &mut overlap,
            &mut waits,
            &mut released,
            || panic!("cleared overlap should not fetch comments"),
        );
        assert_eq!(cleared, HoldOutcome::Clear);
        assert!(!overlap.is_deferred(127));
    }
}
