use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Output;

use crate::command::git;

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("could not run git: {0}")]
    Spawn(#[source] io::Error),
    #[error("git {operation} failed for {path}: {stderr}")]
    Failed {
        operation: &'static str,
        path: PathBuf,
        stderr: String,
    },
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

/// A project folder is a repository when it is the root of a work tree: it holds
/// `.git` as a directory (`git init`, a clone) or as a file (a linked worktree).
/// A folder inside another repository is not one, so it gets its own `git init`.
pub fn is_repository(folder: &Path) -> bool {
    folder.join(".git").exists()
}

/// The folder exists and holds at least one entry, so `git clone` would refuse it.
pub fn folder_is_occupied(folder: &Path) -> Result<bool, GitError> {
    match fs::read_dir(folder) {
        Ok(mut entries) => Ok(entries.next().is_some()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(GitError::Read {
            path: folder.to_path_buf(),
            source,
        }),
    }
}

/// `git init`, which also creates a missing folder. Long paths stay on in the new
/// repository so later git commands in it work past 260 characters.
pub fn initialize_repository(folder: &Path) -> Result<(), GitError> {
    let initialized = git()
        .args(["init", "--quiet", "--"])
        .arg(folder)
        .output()
        .map_err(GitError::Spawn)?;
    succeeded("init", folder, initialized)?;
    let configured = git()
        .arg("--git-dir")
        .arg(folder.join(".git"))
        .args(["config", "core.longpaths", "true"])
        .output()
        .map_err(GitError::Spawn)?;
    succeeded("config", folder, configured)
}

/// The branch HEAD points at: the default branch of a fresh `git init` or clone.
pub fn current_branch(folder: &Path) -> Result<String, GitError> {
    let output = git()
        .arg("--git-dir")
        .arg(folder.join(".git"))
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()
        .map_err(GitError::Spawn)?;
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    succeeded("symbolic-ref", folder, output)?;
    Ok(branch)
}

fn succeeded(operation: &'static str, path: &Path, output: Output) -> Result<(), GitError> {
    if output.status.success() {
        Ok(())
    } else {
        Err(GitError::Failed {
            operation,
            path: path.to_path_buf(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}
