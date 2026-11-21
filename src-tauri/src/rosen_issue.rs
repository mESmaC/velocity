//! GitHub effects are run by the Rosen event follower, never by the checker (#386).
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};
use velocity_core::work_pool::WorkKey;
use velocity_github::{IssueClient, IssueState, RosenIssueEffect};
use velocity_rosen::{
    IssueEvent,
    review_swarm::{self, IssueCloser, IssueReviews},
};
use velocity_store::continuous_dispatch::LandedHold;
use velocity_store::issues::ThreadIssue;
use velocity_store::rosen_issue::{IssueAction, IssueObservation};

use crate::state::ManagedStateStore;
use crate::threads::{lock, report};

/// How often an orphaned landed hold without a local green build polls GitHub for
/// another machine's `velocity/integration` status (#519).
const ORPHANED_STATUS_REFRESH: Duration = Duration::from_secs(2 * 60);

static ORPHANED_STATUS_CHECKS: Mutex<BTreeMap<(u32, u32), Instant>> = Mutex::new(BTreeMap::new());

pub(crate) async fn record_source(
    application: &AppHandle,
    key: (u32, u32),
    server: &velocity_rosen::ProjectServer,
    agent: &str,
    change: &str,
) -> Result<(), String> {
    let items = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        store
            .project(key.0)
            .map(|project| project.rosen_source_items(key.1))
            .unwrap_or_default()
    };
    velocity_rosen::record_work_sources(server, agent, change, key.0, &items)
        .await
        .map_err(|error| error.to_string())
}

/// Short per-thread session card for a Rosen workspace (#534).
/// Static workflow, check, and issue-update rules live in AGENTS.md.
pub(crate) fn session_card(
    application: &AppHandle,
    key: (u32, u32),
    worktree: &Path,
    change: &str,
) -> velocity_acp::client::SessionCard {
    let nick = velocity_rosen::agent_name(worktree);
    let mut card = velocity_acp::client::SessionCard {
        identity: Some(format!("{nick} (thread-{})", key.1)),
        workspace: Some(worktree.to_path_buf()),
        change: Some(change.to_owned()),
        is_rosen: true,
        ..velocity_acp::client::SessionCard::default()
    };
    if let Ok(store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) && let Some(issue) = store
        .project(key.0)
        .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
        .and_then(|thread| thread.issue.as_ref())
    {
        card.work_item = Some(format!(
            "{}#{} (name snapshot with `(#{})`)",
            issue.repository, issue.number, issue.number
        ));
    }
    card
}

pub(crate) fn observe(
    application: &AppHandle,
    project: u32,
    change: &str,
    sequence: u64,
    event: IssueEvent<'_>,
) -> Result<(), String> {
    let observation = match event {
        IssueEvent::Landed(commit) => IssueObservation::Landed(commit),
        IssueEvent::BuildPassed(commit) => IssueObservation::BuildPassed(commit),
        IssueEvent::Reverted { commit, reason } => IssueObservation::Reverted { commit, reason },
    };
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .observe_rosen_issue(project, change, sequence, observation)
    .map_err(|error| error.to_string())
}

/// Called under the follower's follow-up lock. A mirror lag or failed GitHub call stays
/// pending; a revert cancels a close before the next pass and queues a reopen instead.
pub(crate) async fn follow_up(application: &AppHandle, project: u32) {
    let covered = match confirm_builds(application, project, 0).await {
        Ok(covered) => covered,
        Err(error) => {
            report::<()>(Err(error));
            Vec::new()
        }
    };
    let pending = match pending(application, project) {
        Ok(pending) => pending,
        Err(error) => return report::<()>(Err(error)),
    };
    let reviews = if pending
        .iter()
        .any(|thread| thread.actions.iter().any(IssueAction::is_close))
    {
        match issue_reviews(application, project).await {
            Ok(reviews) => Some(reviews),
            Err(error) => {
                report::<()>(Err(error));
                None
            }
        }
    } else {
        None
    };
    for thread in pending {
        if let Err(error) =
            follow_thread(application, project, thread, reviews.as_ref(), &covered).await
        {
            report::<()>(Err(error));
        }
    }
    follow_orphaned_holds(application, project, &covered).await;
}

async fn follow_thread(
    application: &AppHandle,
    project: u32,
    thread: Pending,
    reviews: Option<&IssueReviews>,
    covered: &[String],
) -> Result<(), String> {
    for action in &thread.actions {
        let closer = if action.is_close() {
            let Some(closer) = reviews.and_then(|reviews| {
                let watcher_due = thread.watcher_due && covered.contains(&action.commit);
                reviews.closer(&thread.change, &action.commit, watcher_due)
            }) else {
                break;
            };
            Some(closer)
        } else {
            None
        };
        let (directory, change, work) = (
            thread.directory.clone(),
            thread.change.clone(),
            action.clone(),
        );
        let done = tauri::async_runtime::spawn_blocking(move || {
            synchronize(directory, &change, work, closer)
        })
        .await
        .map_err(|error| error.to_string())??;
        if !done {
            break;
        }
        finish(application, project, &thread.change, action)?;
    }
    Ok(())
}

async fn issue_reviews(application: &AppHandle, project: u32) -> Result<IssueReviews, String> {
    let server = crate::rosen::server(application, project).await?;
    review_swarm::issue_reviews(&server)
        .await
        .map_err(|error| error.to_string())
}

type BuildTargets = (Vec<(String, String)>, Vec<LandedHold>, Vec<String>);

fn build_check_targets(application: &AppHandle, project: u32) -> Result<BuildTargets, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let candidates = store.rosen_issue_build_candidates(project);
    let orphaned = store
        .project(project)
        .map(velocity_store::ProjectItem::orphaned_landed_holds)
        .unwrap_or_default();
    let mut commits: Vec<_> = candidates
        .iter()
        .map(|(_, commit)| commit.clone())
        .collect();
    commits.extend(orphaned.iter().map(|hold| hold.commit.clone()));
    if let Some(item) = store.project(project) {
        commits.extend(item.rosen_work_items.iter().filter_map(|work| {
            (work.status == velocity_core::work_pool::WorkStatus::Landed)
                .then_some(work.landed_commit.as_ref())
                .flatten()
                .cloned()
        }));
    }
    commits.sort_unstable();
    commits.dedup();
    Ok((candidates, orphaned, commits))
}

pub(crate) async fn confirm_builds(
    application: &AppHandle,
    project: u32,
    sequence: u64,
) -> Result<Vec<String>, String> {
    let handle = application.clone();
    let (candidates, orphaned, commits) =
        tauri::async_runtime::spawn_blocking(move || build_check_targets(&handle, project))
            .await
            .map_err(|error| error.to_string())??;
    if commits.is_empty() && sequence == 0 {
        return Ok(Vec::new());
    }
    let covered = if commits.is_empty() {
        Vec::new()
    } else {
        let server = crate::rosen::server(application, project).await?;
        velocity_rosen::issue_build_coverage(&server, &commits)
            .await
            .map_err(|error| error.to_string())?
    };
    let now = unix_seconds();
    let covered_for_store = covered.clone();
    let progress = crate::state::try_with_store(application, move |store| {
        let work_changed = store
            .confirm_rosen_work_build(project, sequence, &covered_for_store)
            .map_err(|error| error.to_string())?;
        for (change, commit) in candidates {
            if covered_for_store.contains(&commit) {
                store
                    .confirm_rosen_issue_build(project, &change, &commit)
                    .map_err(|error| error.to_string())?;
            }
        }
        for hold in orphaned {
            if covered_for_store.contains(&hold.commit) {
                store
                    .confirm_landed_hold_build(project, hold.issue.number, &hold.commit, now)
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(work_changed.then(|| store.layout_refresh()))
    })
    .await?;
    if let Some(progress) = progress {
        let handle = application.clone();
        tauri::async_runtime::spawn_blocking(move || {
            crate::rosen_events::publish_progress(&handle, progress)
        })
        .await
        .map_err(|error| error.to_string())??;
    }
    Ok(covered)
}

struct Pending {
    directory: PathBuf,
    change: String,
    watcher_due: bool,
    actions: Vec<IssueAction>,
}

fn pending(application: &AppHandle, project: u32) -> Result<Vec<Pending>, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let Some(project) = store.project(project) else {
        return Ok(Vec::new());
    };
    let directory = PathBuf::from(&project.repository_path);
    let mut live_changes = HashSet::new();
    let mut items: Vec<Pending> = project
        .threads
        .iter()
        .filter(|thread| !thread.rosen_issue.pending.is_empty())
        .filter_map(|thread| {
            let change = thread.rosen_change.clone()?;
            live_changes.insert(change.clone());
            Some(Pending {
                directory: directory.clone(),
                change,
                watcher_due: thread.rosen_issue.watcher_due(),
                actions: thread.rosen_issue.pending.clone(),
            })
        })
        .collect();
    for (change, retained) in &project.retained_rosen_issues {
        if live_changes.contains(change) || retained.progress.pending.is_empty() {
            continue;
        }
        items.push(Pending {
            directory: directory.clone(),
            change: change.clone(),
            watcher_due: retained.progress.watcher_due(),
            actions: retained.progress.pending.clone(),
        });
    }
    Ok(items)
}

fn synchronize(
    directory: PathBuf,
    change: &str,
    action: IssueAction,
    closer: Option<IssueCloser>,
) -> Result<bool, String> {
    let Some(issue) = &action.issue else {
        return Ok(true);
    };
    let effect = match (action.revert.as_deref(), closer.as_ref()) {
        (Some(reason), _) => RosenIssueEffect::Reverted { reason },
        (_, Some(IssueCloser::Reviewer(by))) => RosenIssueEffect::Reviewed { by },
        (_, Some(IssueCloser::Watcher)) => RosenIssueEffect::Watcher,
        _ if action.land_comment => RosenIssueEffect::Landed { change },
        _ => return Ok(false),
    };
    let client = IssueClient::for_repository(&directory, &issue.repository)
        .map_err(|error| error.to_string())?;
    client
        .follow_rosen_issue(
            &issue.repository,
            issue.number,
            &action.commit,
            action.sequence,
            effect,
        )
        .map_err(|error| error.to_string())
}

fn finish(
    application: &AppHandle,
    project: u32,
    change: &str,
    action: &IssueAction,
) -> Result<(), String> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .finish_rosen_issue(project, change, action)
    .map_err(|error| error.to_string())
}

struct HandoffSweepTargets {
    directory: PathBuf,
    remote: Option<String>,
    candidates: Vec<HandoffSweepCandidate>,
}

struct HandoffSweepCandidate {
    change: String,
    commit: String,
    known_issue: Option<ThreadIssue>,
}

/// On startup, finds any `handoff_items` entry whose change landed and went green in Rosen
/// after its thread was deleted, and adopts its open issue into `retained_rosen_issues` so
/// the next `follow_up` pass closes it with the usual comment (#570).
pub(crate) async fn sweep_orphaned_handoff_issues(
    application: &AppHandle,
    project: u32,
) -> Result<(), String> {
    let handle = application.clone();
    let Some(HandoffSweepTargets {
        directory,
        remote,
        candidates,
    }) = tauri::async_runtime::spawn_blocking(move || handoff_sweep_candidates(&handle, project))
        .await
        .map_err(|error| error.to_string())??
    else {
        return Ok(());
    };
    let mut commits: Vec<String> = candidates.iter().map(|item| item.commit.clone()).collect();
    commits.sort_unstable();
    commits.dedup();
    let server = crate::rosen::server(application, project).await?;
    let covered = velocity_rosen::issue_build_coverage(&server, &commits)
        .await
        .map_err(|error| error.to_string())?;
    let green_candidates: Vec<_> = candidates
        .into_iter()
        .filter(|item| covered.contains(&item.commit))
        .collect();
    if green_candidates.is_empty() {
        return Ok(());
    }
    let dir = directory.clone();
    let Some((repository, open_numbers)) =
        tauri::async_runtime::spawn_blocking(move || open_issue_numbers(&dir, remote.as_deref()))
            .await
            .map_err(|error| error.to_string())??
    else {
        return Ok(());
    };
    if open_numbers.is_empty() {
        return Ok(());
    }
    let resolved =
        resolve_open_handoff_issues(&server, &repository, &open_numbers, green_candidates).await?;
    if resolved.is_empty() {
        return Ok(());
    }
    let reviews = review_swarm::issue_reviews(&server)
        .await
        .map_err(|error| error.to_string())?;
    adopt_resolved_handoff_issues(
        application,
        project,
        &server,
        &directory,
        &reviews,
        resolved,
    )
    .await
}

async fn adopt_resolved_handoff_issues(
    application: &AppHandle,
    project: u32,
    server: &velocity_rosen::ProjectServer,
    directory: &Path,
    reviews: &IssueReviews,
    resolved: Vec<(String, String, ThreadIssue)>,
) -> Result<(), String> {
    for (change, commit, issue) in resolved {
        if !reviews.can_close_land(&change, &commit) {
            continue;
        }
        let local_green = velocity_rosen::integration::commit_green_at(server, &commit)
            .ok()
            .flatten();
        let (dir, repo, number, sha) = (
            directory.to_path_buf(),
            issue.repository.clone(),
            issue.number,
            commit.clone(),
        );
        let need_github_time = local_green.is_none();
        let Some(github_green) = tauri::async_runtime::spawn_blocking(move || {
            confirm_orphaned_handoff_github(&dir, &repo, number, &sha, need_github_time)
        })
        .await
        .map_err(|error| error.to_string())??
        else {
            continue;
        };
        let green_since = local_green.or(github_green).unwrap_or_else(unix_seconds);
        crate::state::try_with_store(application, move |store| {
            store
                .adopt_orphaned_handoff_issue(project, change, issue, commit, green_since)
                .map_err(|error| error.to_string())
        })
        .await?;
    }
    Ok(())
}

fn handoff_sweep_candidates(
    application: &AppHandle,
    project: u32,
) -> Result<Option<HandoffSweepTargets>, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let Some(project) = store.project(project) else {
        return Ok(None);
    };
    let live_changes: HashSet<&str> = project
        .threads
        .iter()
        .filter_map(|thread| thread.rosen_change.as_deref())
        .collect();
    let mut candidates = Vec::new();
    for (change, item) in &project.handoff_items {
        let Some(commit) = item.landed.as_ref() else {
            continue;
        };
        if live_changes.contains(change.as_str())
            || project.retained_rosen_issues.contains_key(change)
        {
            continue;
        }
        let known_issue = project.rosen_work_items.iter().find_map(|work| {
            if work.change.as_deref() != Some(change.as_str()) {
                return None;
            }
            match &work.key {
                WorkKey::Issue { repository, number } => {
                    Some(ThreadIssue::new(repository.clone(), *number))
                }
                _ => None,
            }
        });
        if item.source != "Issue" && known_issue.is_none() {
            continue;
        }
        candidates.push(HandoffSweepCandidate {
            change: change.clone(),
            commit: commit.clone(),
            known_issue,
        });
    }
    if candidates.is_empty() {
        return Ok(None);
    }
    Ok(Some(HandoffSweepTargets {
        directory: PathBuf::from(&project.repository_path),
        remote: project.github_remote.clone(),
        candidates,
    }))
}

fn open_issue_numbers(
    directory: &Path,
    remote: Option<&str>,
) -> Result<Option<(String, HashSet<u32>)>, String> {
    let client = match remote {
        Some(remote) => Some(
            IssueClient::for_repository(directory, remote).map_err(|error| error.to_string())?,
        ),
        None if directory.join(".git").exists()
            && IssueClient::has_origin(directory).map_err(|error| error.to_string())? =>
        {
            Some(IssueClient::discover(directory).map_err(|error| error.to_string())?)
        }
        None => None,
    };
    let Some(client) = client else {
        return Ok(None);
    };
    let repository = client.repository().to_owned();
    let numbers = client
        .open_issues()
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|issue| issue.number)
        .collect();
    Ok(Some((repository, numbers)))
}

async fn resolve_open_handoff_issues(
    server: &velocity_rosen::ProjectServer,
    repository: &str,
    open_numbers: &HashSet<u32>,
    candidates: Vec<HandoffSweepCandidate>,
) -> Result<Vec<(String, String, ThreadIssue)>, String> {
    let mut resolved = Vec::new();
    for candidate in candidates {
        let issue = match candidate.known_issue {
            Some(issue) => Some(issue),
            None => velocity_rosen::change_source_issue(
                server,
                &candidate.change,
                &candidate.commit,
                Some(repository),
            )
            .await
            .map_err(|error| error.to_string())?
            .map(|(repo, number)| ThreadIssue::new(repo, number)),
        };
        let Some(issue) = issue else {
            continue;
        };
        if issue.repository.eq_ignore_ascii_case(repository) && open_numbers.contains(&issue.number)
        {
            resolved.push((candidate.change, candidate.commit, issue));
        }
    }
    Ok(resolved)
}

fn confirm_orphaned_handoff_github(
    directory: &Path,
    repository: &str,
    number: u32,
    commit: &str,
    need_green_at: bool,
) -> Result<Option<Option<u64>>, String> {
    let client =
        IssueClient::for_repository(directory, repository).map_err(|error| error.to_string())?;
    let current = client
        .landed_awaiting_integration(number)
        .map_err(|error| error.to_string())?;
    if current.as_deref() != Some(commit) {
        return Ok(None);
    }
    let passed_at = if need_green_at {
        client
            .integration_status_covering(commit)
            .ok()
            .flatten()
            .and_then(|coverage| coverage.passed_at)
    } else {
        None
    };
    Ok(Some(passed_at))
}

/// Closes orphaned lands from other machines once a green integration build (here or on
/// GitHub) covers the commit and the 30-minute review window has elapsed (#519).
async fn follow_orphaned_holds(application: &AppHandle, project: u32, covered: &[String]) {
    let (directory, holds) = match lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) {
        Ok(store) => match store.project(project) {
            Some(item) => (
                PathBuf::from(&item.repository_path),
                item.orphaned_landed_holds(),
            ),
            None => return,
        },
        Err(error) => return report::<()>(Err(error)),
    };
    for mut hold in holds {
        if let Err(error) =
            follow_orphaned_hold(application, project, &directory, &mut hold, covered).await
        {
            report::<()>(Err(error));
        }
    }
}

async fn follow_orphaned_hold(
    application: &AppHandle,
    project: u32,
    directory: &Path,
    hold: &mut LandedHold,
    covered: &[String],
) -> Result<(), String> {
    let now = unix_seconds();
    if hold.green_since.is_none() && covered.contains(&hold.commit) {
        confirm_orphaned_green(application, project, hold, now)?;
    } else if hold.green_since.is_none() && status_check_due(project, hold.issue.number) {
        let (dir, repo, commit) = (
            directory.to_path_buf(),
            hold.issue.repository.clone(),
            hold.commit.clone(),
        );
        let status = tauri::async_runtime::spawn_blocking(move || {
            let client = velocity_github::IssueClient::for_repository(&dir, &repo)
                .map_err(|error| error.to_string())?;
            client
                .integration_status_covering(&commit)
                .map_err(|error| error.to_string())
        })
        .await
        .map_err(|error| error.to_string())??;
        if let Some(coverage) = status {
            confirm_orphaned_green(
                application,
                project,
                hold,
                coverage.passed_at.unwrap_or(now),
            )?;
        }
    }
    if !hold.watcher_due(now) {
        return Ok(());
    }
    let (dir, snapshot) = (directory.to_path_buf(), hold.clone());
    let done = tauri::async_runtime::spawn_blocking(move || close_orphaned_hold(&dir, &snapshot))
        .await
        .map_err(|error| error.to_string())??;
    if done {
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .remove_landed_hold(project, hold.issue.number)
        .map_err(|error| error.to_string())?;
        if let Ok(mut checks) = ORPHANED_STATUS_CHECKS.lock() {
            checks.remove(&(project, hold.issue.number));
        }
    }
    Ok(())
}

fn confirm_orphaned_green(
    application: &AppHandle,
    project: u32,
    hold: &mut LandedHold,
    green_at: u64,
) -> Result<(), String> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .confirm_landed_hold_build(project, hold.issue.number, &hold.commit, green_at)
    .map_err(|error| error.to_string())?;
    hold.green_since.get_or_insert(green_at);
    Ok(())
}

fn status_check_due(project: u32, number: u32) -> bool {
    let Ok(mut checks) = ORPHANED_STATUS_CHECKS.lock() else {
        return false;
    };
    let key = (project, number);
    if checks
        .get(&key)
        .is_some_and(|at| at.elapsed() < ORPHANED_STATUS_REFRESH)
    {
        return false;
    }
    checks.insert(key, Instant::now());
    true
}

fn close_orphaned_hold(directory: &Path, hold: &LandedHold) -> Result<bool, String> {
    let client = velocity_github::IssueClient::for_repository(directory, &hold.issue.repository)
        .map_err(|error| error.to_string())?;
    let issue = client
        .get(hold.issue.number)
        .map_err(|error| error.to_string())?;
    if issue.state != IssueState::Open {
        return Ok(true);
    }
    let current = client
        .landed_awaiting_integration(hold.issue.number)
        .map_err(|error| error.to_string())?;
    if current.as_deref() != Some(hold.commit.as_str()) {
        return Ok(true);
    }
    client
        .follow_rosen_issue(
            &hold.issue.repository,
            hold.issue.number,
            &hold.commit,
            0,
            RosenIssueEffect::Watcher,
        )
        .map_err(|error| error.to_string())
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
