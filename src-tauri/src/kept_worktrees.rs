//! Retries removing deleted threads' worktrees that could not go when their deletion
//! landed (SPEC.md F5, F12, #283), as F6's merged-worktree cleanup retries on each pass.
//!
//! A worktree waits here while its stopped agent (or a process it started) still holds a
//! file, or while verification keeps it. Each pass verifies it again before removing
//! anything, so a worktree that now has unpushed work, or a path a live thread took over,
//! is never removed.

use std::path::Path;
use std::thread;
use std::time::Duration;

use tauri::{AppHandle, Manager};

use crate::state::ManagedStateStore;
use crate::threads::{ManagedThreads, lock, report};

/// How often kept worktrees are tried again.
const PASS_INTERVAL: Duration = Duration::from_secs(30);

pub fn initialize(application: &AppHandle) {
    let application = application.clone();
    thread::spawn(move || {
        loop {
            report(retry(&application));
            thread::sleep(PASS_INTERVAL);
        }
    });
}

fn retry(application: &AppHandle) -> Result<(), String> {
    let kept = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .kept_worktrees();
    for entry in kept {
        let key = (entry.project, entry.worktree.thread);
        // Its agent has not exited yet; Windows refuses to remove a folder it runs in.
        let exiting = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?
        .is_stopping(key);
        if exiting {
            continue;
        }
        let gone = entry.reused
            || velocity_threads::deletion::remove_owned_worktree(
                Path::new(&entry.repository),
                Path::new(&entry.worktree.path),
                &entry.worktree.branch,
            )
            .is_ok();
        if gone {
            lock(
                application.state::<ManagedStateStore>().inner(),
                "persisted state",
            )?
            .forget_deleted_worktree(entry.project, &entry.worktree.path)
            .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}
