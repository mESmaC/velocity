//! GitHub effects are run by the Rosen event follower, never by the checker (#386).
use std::path::PathBuf;

use tauri::{AppHandle, Manager};
use velocity_rosen::IssueEvent;
use velocity_store::rosen_issue::{IssueAction, IssueObservation};

use crate::state::ManagedStateStore;
use crate::threads::{lock, report};

pub(crate) async fn record_source(
    application: &AppHandle,
    key: (u32, u32),
    server: &velocity_rosen::ProjectServer,
    agent: &str,
    change: &str,
) -> Result<(), String> {
    let issue = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        store
            .project(key.0)
            .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
            .and_then(|thread| thread.issue.clone())
    };
    if let Some(issue) = issue {
        velocity_rosen::record_issue(server, agent, change, &issue.repository, issue.number)
            .await
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub(crate) fn workspace_note(application: &AppHandle, key: (u32, u32), change: &str) -> String {
    let mut note = velocity_rosen::workspace_note(change);
    if let Ok(store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) && let Some(issue) = store
        .project(key.0)
        .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
        .and_then(|thread| thread.issue.as_ref())
    {
        note.push_str(&format!(
            "\n\nThis change implements {}#{}. Record its issue number in the named snapshot: \
             `rosen commit -m \"<one-line summary> (#{})\"`. Velocity handles issue \
             follow-through, honoring the swarm's Close issues option. It closes the issue \
             after the landed change's build passes and its mirror carries the commit, \
             and reopens it if Rosen reverts it. Leave issue follow-through to Velocity.",
            issue.repository, issue.number, issue.number,
        ));
    }
    note
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
    let covered = match confirm_builds(application, project).await {
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
    for (thread, directory, actions) in pending {
        for action in actions {
            if action.revert.is_none() && !covered.contains(&action.commit) {
                break;
            }
            let (directory, work) = (directory.clone(), action.clone());
            let done = tauri::async_runtime::spawn_blocking(move || synchronize(directory, work))
                .await
                .map_err(|error| error.to_string())
                .and_then(|result| result);
            match done {
                Ok(true) => {
                    if let Err(error) = finish(application, project, thread, &action) {
                        report::<()>(Err(error));
                        break;
                    }
                }
                Ok(false) => break,
                Err(error) => {
                    report::<()>(Err(error));
                    break;
                }
            }
        }
    }
}

async fn confirm_builds(application: &AppHandle, project: u32) -> Result<Vec<String>, String> {
    let candidates = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .rosen_issue_build_candidates(project);
    if candidates.is_empty() {
        return Ok(Vec::new());
    }
    let server = crate::rosen::server(application, project).await?;
    let commits: Vec<_> = candidates
        .iter()
        .map(|(_, commit)| commit.clone())
        .collect();
    let covered = velocity_rosen::issue_build_coverage(&server, &commits)
        .await
        .map_err(|error| error.to_string())?;
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    for (thread, commit) in candidates {
        if covered.contains(&commit) {
            store
                .confirm_rosen_issue_build(project, thread, &commit)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(covered)
}

type Pending = Vec<(u32, PathBuf, Vec<IssueAction>)>;

fn pending(application: &AppHandle, project: u32) -> Result<Pending, String> {
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
        .map(|thread| {
            (
                thread.id,
                PathBuf::from(&project.repository_path),
                thread.rosen_issue.pending.clone(),
            )
        })
        .collect())
}

fn synchronize(directory: PathBuf, action: IssueAction) -> Result<bool, String> {
    let Some(issue) = &action.issue else {
        return Ok(true);
    };
    let client =
        velocity_github::IssueClient::discover(&directory).map_err(|error| error.to_string())?;
    client
        .follow_rosen_issue(
            &issue.repository,
            issue.number,
            &action.commit,
            action.sequence,
            action.revert.as_deref(),
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
