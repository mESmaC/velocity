//! GitHub effects are run by the Rosen event follower, never by the checker (#386).
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};
use velocity_github::{IssueState, RosenIssueEffect};
use velocity_rosen::{
    IssueEvent,
    review_swarm::{self, IssueCloser, IssueReviews},
};
use velocity_store::continuous_dispatch::LandedHold;
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
        finish(application, project, thread.id, action)?;
    }
    Ok(())
}

async fn issue_reviews(application: &AppHandle, project: u32) -> Result<IssueReviews, String> {
    let server = crate::rosen::server(application, project).await?;
    review_swarm::issue_reviews(&server)
        .await
        .map_err(|error| error.to_string())
}

type BuildTargets = (Vec<(u32, String)>, Vec<LandedHold>, Vec<String>);

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
    let (candidates, orphaned, commits) = build_check_targets(application, project)?;
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
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    if store
        .confirm_rosen_work_build(project, sequence, &covered)
        .map_err(|error| error.to_string())?
    {
        crate::rosen_events::publish_progress(application, &store)?;
    }
    for (thread, commit) in candidates {
        if covered.contains(&commit) {
            store
                .confirm_rosen_issue_build(project, thread, &commit)
                .map_err(|error| error.to_string())?;
        }
    }
    for hold in orphaned {
        if covered.contains(&hold.commit) {
            store
                .confirm_landed_hold_build(project, hold.issue.number, &hold.commit, now)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(covered)
}

struct Pending {
    id: u32,
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
    Ok(project
        .threads
        .iter()
        .filter(|thread| !thread.rosen_issue.pending.is_empty())
        .filter_map(|thread| {
            Some(Pending {
                id: thread.id,
                directory: PathBuf::from(&project.repository_path),
                change: thread.rosen_change.clone()?,
                watcher_due: thread.rosen_issue.watcher_due(),
                actions: thread.rosen_issue.pending.clone(),
            })
        })
        .collect())
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
    let client = velocity_github::IssueClient::for_repository(&directory, &issue.repository)
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
    thread: u32,
    action: &IssueAction,
) -> Result<(), String> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .finish_rosen_issue(project, thread, action)
    .map_err(|error| error.to_string())
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
