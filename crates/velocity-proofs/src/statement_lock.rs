//! Theorem statement hashes for comparison with a historical proof snapshot.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::lean_source::{ProofFile, theorem_statements};

/// The hashes of all theorem statements at one commit.
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

    /// The previously recorded theorems `current` changed or removed, in key order.
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
