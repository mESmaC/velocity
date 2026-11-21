//! Thread claim wiring and stale-claim reaping; the channel crate owns claim storage.
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;
use tauri::{AppHandle, Manager};
use velocity_acp::client::SessionSetup;
use velocity_airc::{Claim, ClaimResult, ReapedClaim};
use velocity_store::claim_piece::thread_claim_piece;
use velocity_threads::continuous_dispatch::{
    LiveLocalMembers, filter_live_claims, is_live_local_member,
};
use velocity_threads::{ScoutSweeps, ThreadKey};

use crate::state::ManagedStateStore;
use crate::threads::{ManagedThreads, lock};

/// How often Velocity reaps AIRC pieces and presence left by ended local threads or scouts (#566).
const REAP_INTERVAL: Duration = Duration::from_secs(60);
/// Brief startup settle before the first reap pass while Rosen servers initialize.
const STARTUP_REAP_DELAY: Duration = Duration::from_secs(2);
/// Retry delay when a project's Rosen server was still booting during startup reap.
const STARTUP_RETRY_DELAY: Duration = Duration::from_secs(15);

/// Claims the saved thread's piece before its agent starts. The lock key is the thread's
/// work item or branch (`thread_claim_piece`); the title is only the claim's display text.
pub fn attach(
    application: &AppHandle,
    key: ThreadKey,
    setup: &SessionSetup,
    branch: &str,
) -> Result<(), String> {
    attach_with_prompt(application, key, setup, branch, None)
}

/// Claims the saved thread's piece, using complete-phrase prompt text for composer threads
/// whose provisional title was shortened for the sidebar (#532).
pub fn attach_with_prompt(
    application: &AppHandle,
    key: ThreadKey,
    setup: &SessionSetup,
    branch: &str,
    prompt: Option<&str>,
) -> Result<(), String> {
    let (project, thread) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .follow_through_thread(key.0, key.1)
    .map_err(|error| error.to_string())?;
    let piece = thread_claim_piece(&thread, branch);
    let task = claim_task(&thread, prompt);
    let member = thread.airc_member();
    let nick = thread.agent_name();
    let rosen_change = thread.rosen_change.clone();
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let channel = crate::airc_access::open_project_channel(&data, &project)?;
    let base = if project.default_branch.trim().is_empty() {
        "master".into()
    } else {
        project.default_branch.clone()
    };
    let checkout = checkout_record(
        &project.repository_path,
        &setup.worktree,
        rosen_change.as_deref(),
        branch,
        base,
    );
    channel
        .register_checkout(&member, checkout)
        .map_err(|error| error.to_string())?;
    channel
        .join(&member, &nick)
        .map_err(|error| error.to_string())?;
    // On Rosen projects AIRC keeps only work-item identity (no branch or files); Rosen owns path claims (#531).
    let result = if rosen_change.is_some() {
        channel.claim_work_item(&member, &piece, &task)
    } else {
        channel.claim_piece(&member, &piece, &task, branch, Vec::new())
    }
    .map_err(|error| error.to_string())?;
    match result {
        ClaimResult::Granted { .. } => {}
        ClaimResult::Refused { holder, .. } => {
            return Err(format!("Piece already claimed by {holder}"));
        }
    }
    Ok(())
}

fn checkout_record(
    repository: &str,
    worktree: &Path,
    rosen_change: Option<&str>,
    branch: &str,
    base: String,
) -> velocity_airc::waits::Checkout {
    let workspace = worktree.display().to_string();
    match rosen_change {
        Some(change) => velocity_airc::waits::Checkout::rosen(
            repository.to_owned(),
            workspace,
            Some(change.to_owned()),
            base,
        ),
        None => velocity_airc::waits::Checkout::new(
            repository.to_owned(),
            workspace,
            branch.into(),
            base,
        ),
    }
}

fn claim_task(thread: &velocity_store::ThreadItem, prompt: Option<&str>) -> String {
    let is_composer = thread.issue.is_none()
        && thread.spec_section.is_none()
        && thread.task_ids.is_empty()
        && thread.goal_member.is_none();
    if is_composer
        && let Some(prompt) = prompt
        && thread.title == velocity_threads::naming::fallback_title(prompt)
    {
        let full = velocity_core::thread_titles::claim_task_from_prompt(prompt);
        if !full.is_empty() {
            return full;
        }
    }
    thread.title.clone()
}

/// Updates a composer thread's held claim task text once its model title settles (#532).
pub fn update_title(application: &AppHandle, key: ThreadKey, title: &str) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() {
        return Ok(());
    }
    let thread = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        store
            .project(key.0)
            .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
            .cloned()
    };
    let Some(thread) = thread else {
        return Ok(());
    };
    if thread.issue.is_some() || thread.spec_section.is_some() || !thread.task_ids.is_empty() {
        return Ok(());
    }
    let piece = thread_claim_piece(&thread, &thread.branch_name);
    let channel = crate::airc_access::open_channel(application, key.0)?;
    let Some(claim) = channel
        .claims()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|claim| thread.matches_airc_member(&claim.member) && claim.key() == piece)
    else {
        return Ok(());
    };
    if claim.task == title {
        return Ok(());
    }
    if thread.rosen_change.is_some() {
        channel
            .claim_work_item(&claim.member, claim.key(), title)
            .map(|_| ())
            .map_err(|error| error.to_string())
    } else {
        channel
            .claim_piece(&claim.member, claim.key(), title, &claim.branch, Vec::new())
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

fn thread_member(application: &AppHandle, key: ThreadKey) -> String {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()
    .and_then(|store| {
        store
            .project(key.0)
            .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
            .map(velocity_store::ThreadItem::airc_member)
    })
    .unwrap_or_else(|| format!("thread-{}", key.1))
}

pub fn observe(
    application: &AppHandle,
    key: ThreadKey,
    worktree: &Path,
    paths: &[std::path::PathBuf],
) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    let channel = crate::airc_access::open_channel(application, key.0)?;
    // On Rosen projects, Rosen owns path claims; skip store lookup and path normalization (#531, #545).
    if channel.server_backed() {
        return Ok(());
    }
    let member = thread_member(application, key);
    let worktree = worktree.display().to_string();
    // Files outside the worktree (scratch files, temp folders) are not repository files:
    // they are never claimed, so they never post an overlap (#234).
    for file in paths.iter().filter_map(|path| {
        velocity_airc::claims::worktree_file(&worktree, &path.display().to_string())
    }) {
        channel
            .observe_file(&member, &file)
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

/// Releases all AIRC pieces held by thread `key` and leaves the AIRC channel for it (#566).
pub fn release(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    let channel = crate::airc_access::open_channel(application, key.0)?;
    let member = thread_member(application, key);
    let legacy = format!("thread-{}", key.1);
    channel
        .release_matching_and_leave(
            |candidate| {
                candidate == member
                    || candidate == legacy
                    || velocity_core::thread_titles::thread_id_from_member(candidate) == Some(key.1)
            },
            None,
        )
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Releases all AIRC pieces held by duty scout `nickname` and leaves the channel (#566).
pub fn release_duty_member(
    application: &AppHandle,
    project: u32,
    nickname: &str,
) -> Result<(), String> {
    let channel = crate::airc_access::open_channel(application, project)?;
    let member = velocity_acp::client::duty_member(nickname);
    channel
        .release_member_and_leave(&member, None)
        .map(|_| ())
        .map_err(|error| error.to_string())
}

/// Starts startup and periodic reaping of dead local AIRC claims across every project (#566).
pub fn initialize(application: &AppHandle) {
    let handle = application.clone();
    std::thread::spawn(move || reap_loop(handle));
}

fn reap_loop(application: AppHandle) {
    std::thread::sleep(STARTUP_REAP_DELAY);
    let failed = reap_all_projects(&application);
    if !failed.is_empty() {
        std::thread::sleep(STARTUP_RETRY_DELAY);
        for project_id in failed {
            let _ = reap_project(&application, project_id);
        }
    }
    loop {
        std::thread::sleep(REAP_INTERVAL);
        let _ = reap_all_projects(&application);
    }
}

fn reap_all_projects(application: &AppHandle) -> Vec<u32> {
    let project_ids: Vec<u32> = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .map(|store| {
        store
            .state()
            .projects
            .iter()
            .map(|project| project.id)
            .collect()
    })
    .unwrap_or_default();
    let mut failed = Vec::new();
    for project_id in project_ids {
        if reap_project(application, project_id).is_err() {
            failed.push(project_id);
        }
    }
    failed
}

/// Snapshot of local threads, spec interviews and duty scouts currently alive in `project` (#566).
pub fn live_local_members(application: &AppHandle, project: u32) -> LiveLocalMembers {
    let running_scouts = application
        .try_state::<ScoutSweeps>()
        .map(|sweeps| {
            sweeps
                .running()
                .into_iter()
                .filter(|(owner, _)| *owner == project)
                .map(|(_, nickname)| nickname)
                .collect()
        })
        .unwrap_or_default();
    let live_threads = application
        .try_state::<ManagedThreads>()
        .and_then(|runtimes| lock(runtimes.inner(), "thread sessions").ok())
        .map(|runtimes| runtimes.live_threads_for(project))
        .unwrap_or_default();
    let running_interviews = application
        .try_state::<velocity_interview::Interviews>()
        .map(|interviews| {
            lock(
                application.state::<ManagedStateStore>().inner(),
                "persisted state",
            )
            .ok()
            .and_then(|store| {
                store.project(project).map(|item| {
                    item.threads
                        .iter()
                        .filter(|thread| interviews.is_running((project, thread.id)))
                        .map(|thread| thread.id)
                        .collect()
                })
            })
            .unwrap_or_default()
        })
        .unwrap_or_default();
    LiveLocalMembers {
        running_scouts,
        live_threads,
        running_interviews,
    }
}

/// Reaps dead local AIRC pieces and joined presence for `project_id`, leaving remote
/// machines' claims untouched (#566).
pub fn reap_project(application: &AppHandle, project_id: u32) -> Result<Vec<ReapedClaim>, String> {
    let Some(project) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .project(project_id)
    .cloned() else {
        return Ok(Vec::new());
    };
    if project.rosen_in_use() && crate::rosen::paused(application) {
        return Ok(Vec::new());
    }
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let channel = crate::airc_access::open_project_channel(&data, &project)?;
    let live = live_local_members(application, project_id);
    let reaped = channel
        .reap_dead_members(|member| is_live_local_member(&project, member, &live))
        .map_err(|error| error.to_string())?;
    if !reaped.is_empty() {
        crate::project_health::record_stale_claim_released(
            application,
            project_id,
            &project.name,
            &reaped,
        );
    }
    Ok(reaped)
}

/// Reads a project's AIRC claims for dispatch, filtering out any held by known-dead local
/// members and releasing those dead claims immediately (#566).
pub fn live_dispatch_claims(
    application: &AppHandle,
    project_id: u32,
) -> Result<(Vec<Claim>, Vec<ReapedClaim>), String> {
    let Some(project) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .project(project_id)
    .cloned() else {
        return Ok((Vec::new(), Vec::new()));
    };
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let channel = crate::airc_access::open_project_channel(&data, &project)?;
    let live = live_local_members(application, project_id);
    let scoped = channel.scoped_claims().map_err(|error| error.to_string())?;
    let filtered = filter_live_claims(&project, &scoped, &live);
    let active_claims: Vec<Claim> = scoped
        .into_iter()
        .filter(|entry| !entry.local || is_live_local_member(&project, &entry.claim.member, &live))
        .map(|entry| entry.claim)
        .collect();
    if filtered.dead_local.is_empty() {
        return Ok((active_claims, Vec::new()));
    }
    let dead_members: HashSet<String> = filtered
        .dead_local
        .iter()
        .map(|claim| claim.member.clone())
        .collect();
    let reaped = channel
        .release_matching_and_leave(
            |member| dead_members.contains(member),
            Some("stale AIRC claim released"),
        )
        .unwrap_or_else(|_| {
            filtered
                .dead_local
                .iter()
                .map(|claim| ReapedClaim {
                    piece: claim.key().to_owned(),
                    member: claim.member.clone(),
                    nick: claim.nick.clone(),
                    task: claim.task.clone(),
                })
                .collect()
        });
    if !reaped.is_empty() {
        crate::project_health::record_stale_claim_released(
            application,
            project_id,
            &project.name,
            &reaped,
        );
    }
    Ok((active_claims, reaped))
}
