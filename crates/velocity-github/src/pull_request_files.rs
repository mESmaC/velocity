//! The files a PR changes, for swarm rebases that only touch affected members (F6, F10), and
//! its diff, for PR duty's review tiers (F7).
use std::path::Path;

use serde::Deserialize;
use velocity_core::review_diff::{DiffSummary, summarize_diff};

use crate::{PullRequestClient, PullRequestError};

pub(crate) struct ReviewFiles {
    range: String,
    pub paths: Vec<String>,
    pub stat: String,
}

impl ReviewFiles {
    pub(crate) fn read(checkout: &Path, base: &str, head: &str) -> Result<Self, PullRequestError> {
        let range = format!("{base}...{head}");
        let paths = git_diff(
            checkout,
            &["--name-only", "-z", "--no-renames", &range, "--"],
        )?
        .split_terminator('\0')
        .map(str::to_owned)
        .collect();
        let stat = git_diff(checkout, &["--stat", "--no-renames", &range, "--"])?;
        Ok(Self { range, paths, stat })
    }

    pub(crate) fn patch(&self, checkout: &Path, path: &str) -> Result<String, PullRequestError> {
        if !self.paths.iter().any(|changed| changed == path) {
            return Err(PullRequestError::Invalid(
                "Use one path from the PR overview".into(),
            ));
        }
        // Git's literal pathspec also matches descendants when a file became
        // a directory, so exclude them to keep this to one file.
        let file = format!(":(top,literal){path}");
        let descendants = format!(":(exclude,top,literal){path}/");
        git_diff(
            checkout,
            &[
                "--patch",
                "--no-renames",
                &self.range,
                "--",
                &file,
                &descendants,
            ],
        )
    }
}

fn git_diff(checkout: &Path, arguments: &[&str]) -> Result<String, PullRequestError> {
    let output = crate::command::git_in(checkout)
        .args(["diff", "--no-ext-diff", "--no-textconv", "--no-color"])
        .args(arguments)
        .output()?;
    if !output.status.success() {
        return Err(PullRequestError::Invalid(format!(
            "Could not read the review diff in this checkout: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

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
