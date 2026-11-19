//! Reads historical statement locks for legacy GitHub proof cards.

use std::path::Path;

use crate::ProofCheckError;
use crate::statement_lock::StatementLock;

/// Reads the statement lock saved for PR `number`, when one exists.
pub fn load_statement_lock(
    app_data: &Path,
    project: &str,
    number: u32,
) -> Result<Option<StatementLock>, ProofCheckError> {
    let path = app_data
        .join("statement-locks")
        .join(project)
        .join(format!("pr-{number}.json"));
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}
