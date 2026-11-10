use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Output;

use crate::command::git;
use crate::short_checkout::CheckoutPath;

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
    // Create through Rust first: Git cannot create a long target even with core.longpaths.
    fs::create_dir_all(folder).map_err(|source| GitError::Read {
        path: folder.to_path_buf(),
        source,
    })?;
    let checkout = CheckoutPath::new(folder).map_err(GitError::Spawn)?;
    let directory = checkout.path();
    #[cfg(windows)]
    if folder.as_os_str().len() >= 200 {
        return initialize_long_repository(folder, directory);
    }
    let initialized = git()
        .args(["init", "--quiet", "--"])
        .arg(directory)
        .output()
        .map_err(GitError::Spawn)?;
    succeeded("init", folder, initialized)?;
    let configured = git()
        .arg("--git-dir")
        .arg(directory.join(".git"))
        .args(["config", "core.longpaths", "true"])
        .output()
        .map_err(GitError::Spawn)?;
    succeeded("config", folder, configured)
}

#[cfg(windows)]
fn initialize_long_repository(folder: &Path, directory: &Path) -> Result<(), GitError> {
    // Unlike add/commit, init resolves junctions and rejects the resulting long path.
    // Initialize ordinary metadata at a short path, then publish it with a native rename.
    if !is_repository(folder) {
        let staging = tempfile::tempdir().map_err(GitError::Spawn)?;
        initialize_repository(staging.path())?;
        // Stage on the destination volume as well: TEMP may live on another drive.
        let publication = tempfile::tempdir_in(folder).map_err(GitError::Spawn)?;
        let metadata = publication.path().join(".git");
        copy_metadata(&staging.path().join(".git"), &metadata).map_err(GitError::Spawn)?;
        fs::rename(metadata, folder.join(".git")).map_err(GitError::Spawn)?;
    }
    let configured = git()
        .arg("--git-dir")
        .arg(directory.join(".git"))
        .args(["config", "core.longpaths", "true"])
        .output()
        .map_err(GitError::Spawn)?;
    succeeded("config", folder, configured)
}

#[cfg(windows)]
fn copy_metadata(source: &Path, destination: &Path) -> io::Result<()> {
    fs::create_dir(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_metadata(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// The branch HEAD points at: the default branch of a fresh `git init` or clone.
pub fn current_branch(folder: &Path) -> Result<String, GitError> {
    let checkout = CheckoutPath::new(folder).map_err(GitError::Spawn)?;
    let directory = checkout.path();
    let output = git()
        .arg("--git-dir")
        .arg(directory.join(".git"))
        .args(["symbolic-ref", "--short", "HEAD"])
        .output()
        .map_err(GitError::Spawn)?;
    let branch = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    succeeded("symbolic-ref", folder, output)?;
    Ok(branch)
}

/// Commits one file in `folder`, and nothing else that happens to be staged. Returns
/// `false` when the file already matches `HEAD`, so there is nothing to commit.
pub fn commit_file(folder: &Path, file: &Path, message: &str) -> Result<bool, GitError> {
    let checkout = CheckoutPath::new(folder).map_err(GitError::Spawn)?;
    commit_at(checkout.path(), file, message)
}

fn commit_at(folder: &Path, file: &Path, message: &str) -> Result<bool, GitError> {
    let added = git()
        .arg("-C")
        .arg(folder)
        .args(["add", "--"])
        .arg(file)
        .output()
        .map_err(GitError::Spawn)?;
    succeeded("add", folder, added)?;
    let unchanged = git()
        .arg("-C")
        .arg(folder)
        .args(["diff", "--cached", "--quiet", "--"])
        .arg(file)
        .status()
        .map_err(GitError::Spawn)?
        .success();
    // A repository with no commits yet has no HEAD to compare with: always commit.
    if unchanged && has_head(folder)? {
        return Ok(false);
    }
    let committed = git()
        .arg("-C")
        .arg(folder)
        .args(["commit", "--quiet", "-m", message, "--"])
        .arg(file)
        .output()
        .map_err(GitError::Spawn)?;
    succeeded("commit", folder, committed)?;
    Ok(true)
}

fn has_head(folder: &Path) -> Result<bool, GitError> {
    Ok(git()
        .arg("-C")
        .arg(folder)
        .args(["rev-parse", "--verify", "--quiet", "HEAD"])
        .output()
        .map_err(GitError::Spawn)?
        .status
        .success())
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
