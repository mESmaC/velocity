//! The statement lock (SPEC.md F17): when PR duty approves, Velocity hashes every theorem
//! statement at the approved head. A later head that changes or removes one of them withdraws
//! the approval and restarts the review, because weakening a statement until its proof goes
//! through is the cheat this catches.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::lean_source::{ProofFile, theorem_statements};

/// Every theorem statement at one PR head, hashed.
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

fn hash(statement: &str) -> String {
    Sha256::digest(statement.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
