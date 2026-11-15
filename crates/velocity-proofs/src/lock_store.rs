//! Where statement locks are kept: one JSON file per PR or Rosen change in app data, beside the
//! AIRC channels that hold the approval record they belong to.

use std::path::{Path, PathBuf};

use crate::ProofCheckError;
use crate::statement_lock::StatementLock;

fn lock_path(app_data: &Path, project: &str, number: u32) -> PathBuf {
    app_data
        .join("statement-locks")
        .join(project)
        .join(format!("pr-{number}.json"))
}

/// Saves the lock taken when PR `number` was approved, replacing any earlier one.
pub fn save_statement_lock(
    app_data: &Path,
    project: &str,
    number: u32,
    lock: &StatementLock,
) -> Result<(), ProofCheckError> {
    let path = lock_path(app_data, project, number);
    save_lock(&path, lock)
}

fn save_lock(path: &Path, lock: &StatementLock) -> Result<(), ProofCheckError> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    // Written beside and renamed over, so a crash never leaves half a lock.
    let partial = path.with_extension("json.partial");
    std::fs::write(&partial, serde_json::to_vec_pretty(lock)?)?;
    std::fs::rename(&partial, path)?;
    Ok(())
}

/// The lock taken at PR `number`'s last approval, if it was ever approved.
pub fn load_statement_lock(
    app_data: &Path,
    project: &str,
    number: u32,
) -> Result<Option<StatementLock>, ProofCheckError> {
    load_lock(&lock_path(app_data, project, number))
}

fn load_lock(path: &Path) -> Result<Option<StatementLock>, ProofCheckError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn change_lock_path(
    app_data: &Path,
    project: &str,
    change: &str,
) -> Result<PathBuf, ProofCheckError> {
    // A change id becomes a filename; accept only Rosen's full, stable id.
    if change.len() != 32 || !change.bytes().all(|byte| (b'k'..=b'z').contains(&byte)) {
        return Err(ProofCheckError::StatementLock(
            "Invalid Rosen change id".into(),
        ));
    }
    Ok(app_data
        .join("statement-locks")
        .join(project)
        .join(format!("change-{change}.json")))
}

/// Saves a Rosen approval's lock before its approval is published, replacing an older one.
pub fn save_change_statement_lock(
    app_data: &Path,
    project: &str,
    change: &str,
    lock: &StatementLock,
) -> Result<(), ProofCheckError> {
    save_lock(&change_lock_path(app_data, project, change)?, lock)
}

/// The lock taken at a Rosen change's last approval, if it was ever approved.
pub fn load_change_statement_lock(
    app_data: &Path,
    project: &str,
    change: &str,
) -> Result<Option<StatementLock>, ProofCheckError> {
    load_lock(&change_lock_path(app_data, project, change)?)
}

/// Refuses landing without a readable approval lock at this head and unchanged statements.
pub fn require_change_statement_lock(
    app_data: &Path,
    project: &str,
    change: &str,
    current: &StatementLock,
) -> Result<(), ProofCheckError> {
    let lock = load_change_statement_lock(app_data, project, change)?
        .ok_or_else(|| ProofCheckError::StatementLock("Missing approval lock".into()))?;
    if lock.head != current.head {
        return Err(ProofCheckError::StatementLock(
            "Approval lock is for another head".into(),
        ));
    }
    let changed = lock.changed_in(current);
    if !changed.is_empty() {
        return Err(ProofCheckError::StatementLock(format!(
            "Theorem statements changed or removed since approval: {}",
            changed.join(", ")
        )));
    }
    Ok(())
}
