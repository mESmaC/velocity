//! Rebase only the swarm members a merge affects (F6, F10): after a member's PR merges,
//! each other live member whose own changes share a file with that PR is told to bring
//! its branch up to date. Members the merge doesn't touch rebase once at landing.

use std::collections::BTreeSet;
use std::path::PathBuf;

use tauri::{AppHandle, Manager};
use velocity_core::swarm::{SwarmEventKind, SwarmFeedEvent};
use velocity_github::PullRequestClient;
use velocity_store::ThreadState;
use velocity_store::duty::clock_timestamp;
use velocity_store::swarms::live_members;

use crate::state::ManagedStateStore;
use crate::threads::{lock, report};

struct AffectedCheck {
    repository: PathBuf,
    base: String,
    title: String,
    members: Vec<(u32, String, PathBuf)>,
}

/// Reads the merged PR's files and each live member's changes, then queues a rebase notice
/// for every member they overlap. Runs off the watcher thread: it waits on `gh` and `git`.
pub(crate) fn notify_affected(
    application: &AppHandle,
    project: u32,
    swarm: u32,
    merged: u32,
    number: u32,
) -> Result<(), String> {
    let Some(check) = affected_check(application, project, swarm, merged)? else {
        return Ok(());
    };
    let (merged_head, merged_files, claims) =
        merge_evidence(application, project, number, &check.repository)?;
    let mut affected = 0;
    for (thread, nick, worktree) in &check.members {
        let mut overlap = velocity_git::landing::edited_overlap(worktree, &merged_head)
            .map_err(|error| error.to_string())?;
        for claim in claims
            .iter()
            .filter(|claim| claim.member == format!("thread-{thread}"))
        {
            overlap.extend(
                claim
                    .files
                    .iter()
                    .filter(|file| {
                        velocity_git::landing::contested_path(file) && merged_files.contains(*file)
                    })
                    .cloned(),
            );
        }
        let overlap: Vec<String> = overlap.into_iter().collect();
        if overlap.is_empty() {
            continue;
        }
        let notice = rebase_notice(number, &check.title, &check.base, &overlap);
        match crate::threads::steer(application, (project, *thread), &notice) {
            Ok(_) => affected += 1,
            Err(error) => eprintln!("Swarm rebase notice for {nick}: {error}"),
        }
    }
    if affected > 0 {
        let message = format!(
            "{affected} affected agent{} rebasing after #{number}",
            if affected == 1 { "" } else { "s" }
        );
        let event =
            SwarmFeedEvent::new(&clock_timestamp(), "swarm", SwarmEventKind::Rebase, message);
        report(crate::swarm::post_event(application, project, swarm, event));
    }
    Ok(())
}

type MergeEvidence = (String, BTreeSet<String>, Vec<velocity_airc::Claim>);
fn merge_evidence(
    application: &AppHandle,
    project: u32,
    number: u32,
    repository: &std::path::Path,
) -> Result<MergeEvidence, String> {
    let client = PullRequestClient::discover(repository).map_err(|error| error.to_string())?;
    let merged_head = client
        .snapshot(number)
        .map_err(|error| error.to_string())?
        .pull_request
        .head_ref_oid;
    let merged_files: BTreeSet<String> = client
        .changed_files(number)
        .map_err(|error| error.to_string())?
        .into_iter()
        .collect();
    let channel = velocity_airc::Channel::open(
        &application
            .path()
            .app_data_dir()
            .map_err(|error| error.to_string())?,
        &project.to_string(),
    )
    .map_err(|error| error.to_string())?;
    let claims = channel.claims().map_err(|error| error.to_string())?;
    Ok((merged_head, merged_files, claims))
}

/// The other live members with a worktree, when the swarm rebases on merge.
fn affected_check(
    application: &AppHandle,
    project: u32,
    swarm: u32,
    merged: u32,
) -> Result<Option<AffectedCheck>, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let Some(item) = store.project(project) else {
        return Ok(None);
    };
    let Some(record) = item.swarms.iter().find(|record| record.id == swarm) else {
        return Ok(None);
    };
    if !record.rebase_on_merge {
        return Ok(None);
    }
    let members: Vec<_> = live_members(item, record)
        .filter(|(_, thread)| {
            thread.id != merged
                && matches!(thread.state, ThreadState::Working | ThreadState::Review)
        })
        .filter_map(|(member, thread)| {
            let path = thread.worktree_path.as_ref()?;
            Some((
                thread.id,
                format!("a{}", member.agent_number),
                PathBuf::from(path),
            ))
        })
        .collect();
    let title = item
        .threads
        .iter()
        .find(|thread| thread.id == merged)
        .map(|thread| thread.title.clone())
        .unwrap_or_default();
    let base = if item.default_branch.trim().is_empty() {
        "master".to_owned()
    } else {
        item.default_branch.clone()
    };
    Ok((!members.is_empty()).then(|| AffectedCheck {
        repository: PathBuf::from(&item.repository_path),
        base,
        title,
        members,
    }))
}

fn rebase_notice(number: u32, title: &str, base: &str, overlap: &[String]) -> String {
    const SHOWN: usize = 8;
    let mut files = overlap
        .iter()
        .take(SHOWN)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if overlap.len() > SHOWN {
        files.push_str(&format!(" and {} more", overlap.len() - SHOWN));
    }
    format!(
        "PR #{number} from your swarm ({title}) just merged into {base} and touched files you changed: {files}. Bring your branch up to date with origin/{base} now, the way AGENTS.md says (rebase, or merge where force-pushing is not allowed), resolve any conflicts, re-run the checks and push."
    )
}
