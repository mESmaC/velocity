//! The files a PR changes, for swarm rebases that only touch affected members (F6, F10), and
//! its diff, for PR duty's review tiers (F7).
use serde::Deserialize;
use velocity_core::review_diff::{DiffSummary, summarize_diff};

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

    /// What the PR's diff changes. GitHub refuses a diff over its size limit; the file list
    /// still tiers such a PR, which is then never trivial.
    pub fn diff_summary(&self, number: u32) -> Result<DiffSummary, PullRequestError> {
        let number_text = number.to_string();
        let diff = self.command.text(&[
            "pr",
            "diff",
            &number_text,
            "--repo",
            &self.repository,
            "--color",
            "never",
        ]);
        match diff {
            Ok(diff) => Ok(summarize_diff(&diff)),
            Err(_) => Ok(DiffSummary::from_paths(self.changed_files(number)?)),
        }
    }
}
