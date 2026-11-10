//! The files a PR changes, for swarm rebases that only touch affected members (F6, F10).
use serde::Deserialize;

use crate::{PullRequestClient, PullRequestError};

#[derive(Deserialize)]
struct ChangedFiles {
    files: Vec<ChangedFile>,
}

#[derive(Deserialize)]
struct ChangedFile {
    path: String,
}

impl PullRequestClient {
    /// Repository-relative paths the PR adds, edits or deletes.
    pub fn changed_files(&self, number: u32) -> Result<Vec<String>, PullRequestError> {
        let number = number.to_string();
        let changed: ChangedFiles = self.command.read(&[
            "pr",
            "view",
            &number,
            "--repo",
            &self.repository,
            "--json",
            "files",
        ])?;
        Ok(changed.files.into_iter().map(|file| file.path).collect())
    }
}
