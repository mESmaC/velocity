//! Changed paths for targeted rebases on legacy GitHub threads.

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
