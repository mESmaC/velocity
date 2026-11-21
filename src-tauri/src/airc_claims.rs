//! Thread claim wiring only; the channel crate owns claim decisions and storage.
use std::path::Path;
use tauri::{AppHandle, Manager};
use velocity_acp::client::SessionSetup;
use velocity_airc::{Channel, ClaimResult};
use velocity_store::claim_piece::thread_claim_piece;
use velocity_threads::ThreadKey;

use crate::state::ManagedStateStore;
use crate::threads::lock;

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
    let channel = Channel::open(&data, &key.0.to_string()).map_err(|error| error.to_string())?;
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
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let channel = Channel::open(&data, &key.0.to_string()).map_err(|error| error.to_string())?;
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
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let channel = Channel::open(&data, &key.0.to_string()).map_err(|error| error.to_string())?;
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

pub fn release(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let channel = Channel::open(&data, &key.0.to_string()).map_err(|error| error.to_string())?;
    let member = thread_member(application, key);
    let legacy = format!("thread-{}", key.1);
    for claim in channel.claims().map_err(|error| error.to_string())? {
        if claim.member == member || claim.member == legacy {
            channel
                .release(&claim.member, claim.key())
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}
