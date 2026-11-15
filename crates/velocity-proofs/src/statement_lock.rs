//! The statement lock (SPEC.md F17): when PR duty approves, Velocity hashes every theorem
//! statement at the approved head. A later head that changes or removes one of them withdraws
//! the approval and restarts the review, because weakening a statement until its proof goes
//! through is the cheat this catches.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ProofCheckError;
use crate::lean_source::{ProofFile, theorem_statements};

/// Every theorem statement at one PR or Rosen change head, hashed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatementLock {
    /// The head commit the statements were read at.
    pub head: String,
    /// Theorem key (`<file>::<qualified name>`) to the SHA-256 of its normalised statement.
    pub statements: BTreeMap<String, String>,
}

impl StatementLock {
    /// Hashes every theorem statement in `files`, read at `head`.
    pub fn of_files(head: &str, files: &[ProofFile]) -> Self {
        let statements = files
            .iter()
            .flat_map(theorem_statements)
            .map(|theorem| (theorem.key, hash(&theorem.statement)))
            .collect();
        Self {
            head: head.to_owned(),
            statements,
        }
    }

    /// The locked theorems `current` changed or removed, in key order. New theorems are not
    /// listed: they had no statement to lock, and the new head gets a review anyway.
    pub fn changed_in(&self, current: &StatementLock) -> Vec<String> {
        self.statements
            .iter()
            .filter(|(key, locked)| current.statements.get(*key) != Some(*locked))
            .map(|(key, _)| key.clone())
            .collect()
    }
}

/// Hashes the Lean sources in a Rosen review's plain merge-result tree, without git or Lean.
/// Lake's generated files and dependencies are excluded, just as for git statement locks.
pub fn statements_in_tree(tree: &Path, head: &str) -> Result<StatementLock, ProofCheckError> {
    // A missing review folder must not look like a project with no proofs.
    std::fs::metadata(tree)?;
    let folder = tree.join("proofs");
    let entries = match std::fs::read_dir(&folder) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(StatementLock::of_files(head, &[]));
        }
        Err(error) => return Err(error.into()),
    };
    let mut files = Vec::new();
    read_sources(tree, entries, &mut files)?;
    Ok(StatementLock::of_files(head, &files))
}

fn read_sources(
    tree: &Path,
    entries: std::fs::ReadDir,
    files: &mut Vec<ProofFile>,
) -> Result<(), ProofCheckError> {
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        if entry.file_name() == ".lake" {
            continue;
        }
        if entry.file_type()?.is_dir() {
            read_sources(tree, std::fs::read_dir(&path)?, files)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "lean")
        {
            let relative = path.strip_prefix(tree).map_err(std::io::Error::other)?;
            files.push(ProofFile::new(
                &relative.to_string_lossy(),
                &std::fs::read_to_string(&path)?,
            ));
        }
    }
    Ok(())
}

fn hash(statement: &str) -> String {
    Sha256::digest(statement.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
