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
    let (piece, task) = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let thread = store
            .project(key.0)
            .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
            .ok_or("The thread was removed before its claim")?;
        (thread_claim_piece(thread, branch), thread.title.clone())
    };
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let channel = Channel::open(&data, &key.0.to_string()).map_err(|error| error.to_string())?;
    let member = format!("thread-{}", key.1);
    let (project, _) = crate::threads::lock(
        application
            .state::<crate::state::ManagedStateStore>()
            .inner(),
        "persisted state",
    )?
    .follow_through_thread(key.0, key.1)
    .map_err(|error| error.to_string())?;
    channel
        .register_checkout(
            &member,
            velocity_airc::waits::Checkout::new(
                project.repository_path.clone(),
                setup.worktree.display().to_string(),
                branch.into(),
                if project.default_branch.trim().is_empty() {
                    "master".into()
                } else {
                    project.default_branch.clone()
                },
            ),
        )
        .map_err(|error| error.to_string())?;
    channel
        .join(&member, &setup.worktree.display().to_string())
        .map_err(|error| error.to_string())?;
    match channel
        .claim_piece(&member, &piece, &task, branch, Vec::new())
        .map_err(|error| error.to_string())?
    {
        ClaimResult::Granted { .. } => {}
        ClaimResult::Refused { holder, .. } => {
            return Err(format!("Piece already claimed by {holder}"));
        }
    }
    Ok(())
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
    let worktree = worktree.display().to_string();
    // Files outside the worktree (scratch files, temp folders) are not repository files:
    // they are never claimed, so they never post an overlap (#234).
    for file in paths.iter().filter_map(|path| {
        velocity_airc::claims::worktree_file(&worktree, &path.display().to_string())
    }) {
        channel
            .observe_file(&format!("thread-{}", key.1), &file)
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
    let member = format!("thread-{}", key.1);
    for claim in channel.claims().map_err(|error| error.to_string())? {
        if claim.member == member {
            channel
                .release(&member, claim.key())
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}
