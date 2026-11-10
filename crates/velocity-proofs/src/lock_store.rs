//! Where statement locks are kept: one JSON file per PR in the app data folder, beside the
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
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    // Written beside and renamed over, so a crash never leaves half a lock.
    let partial = path.with_extension("json.partial");
    std::fs::write(&partial, serde_json::to_vec_pretty(lock)?)?;
    std::fs::rename(&partial, &path)?;
    Ok(())
}

/// The lock taken at PR `number`'s last approval, if it was ever approved.
pub fn load_statement_lock(
    app_data: &Path,
    project: &str,
    number: u32,
) -> Result<Option<StatementLock>, ProofCheckError> {
    match std::fs::read(lock_path(app_data, project, number)) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
