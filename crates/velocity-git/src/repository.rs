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

/// The default branch advertised by origin, falling back to the checkout's branch
/// when no remote HEAD is recorded. Reads local metadata without fetching.
pub fn opened_project_default_branch(folder: &Path) -> Result<String, GitError> {
    let checkout = CheckoutPath::new(folder).map_err(GitError::Spawn)?;
    let output = git()
        .arg("-C")
        .arg(checkout.path())
        .args(["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"])
        .output()
        .map_err(GitError::Spawn)?;
    // Exit 1 means the ref is absent or is not symbolic. Other failures must not
    // silently turn a feature branch into the project's default branch.
    if output.status.code() == Some(1) {
        return current_branch(folder);
    }
    let reference = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    succeeded("symbolic-ref", folder, output)?;
    reference
        .strip_prefix("refs/remotes/origin/")
        .filter(|branch| !branch.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| GitError::Failed {
            operation: "symbolic-ref",
            path: folder.to_path_buf(),
            stderr: format!("origin/HEAD points outside origin: {reference}"),
        })
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

/// The project's main checkout for any folder git knows: the checkout itself, or the clone a
/// linked worktree (a thread's, a duty agent's or a warm pool slot) was made from.
pub fn main_checkout(folder: &Path) -> Result<PathBuf, GitError> {
    let checkout = CheckoutPath::new(folder).map_err(GitError::Spawn)?;
    let output = git()
        .arg("-C")
        .arg(checkout.path())
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .output()
        .map_err(GitError::Spawn)?;
    let common = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    succeeded("rev-parse", folder, output)?;
    let root = common.parent().unwrap_or(&common).to_path_buf();
    fs::canonicalize(&root).map_err(GitError::Spawn)
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

/// Publish instruction files before agents branch from origin. Local projects stay local.
pub fn publish_instructions(folder: &Path) -> Result<(), GitError> {
    let checkout = CheckoutPath::new(folder).map_err(GitError::Spawn)?;
    let folder = checkout.path();
    let remote = git()
        .arg("-C")
        .arg(folder)
        .args(["remote", "get-url", "origin"])
        .output()
        .map_err(GitError::Spawn)?;
    if !remote.status.success() {
        return Ok(());
    }
    let branch = opened_project_default_branch(folder)?;
    if current_branch(folder)? != branch {
        return Err(GitError::Failed {
            operation: "publish instructions",
            path: folder.to_owned(),
            stderr: format!("Switch the main checkout to {branch} before publishing instructions"),
        });
    }
    require_publishable_checkout(folder)?;
    for (operation, arguments) in [
        ("fetch", vec!["fetch", "origin", branch.as_str()]),
        ("merge", vec!["merge", "--no-edit", "FETCH_HEAD"]),
        ("push", vec!["push", "origin", "HEAD"]),
    ] {
        let output = git()
            .arg("-C")
            .arg(folder)
            .args(arguments)
            .output()
            .map_err(GitError::Spawn)?;
        if let Err(error) = succeeded(operation, folder, output) {
            if operation == "merge" {
                let _ = git()
                    .arg("-C")
                    .arg(folder)
                    .args(["merge", "--abort"])
                    .output();
            }
            return Err(error);
        }
    }
    Ok(())
}

// Never fold unrelated staged edits into the merge, or abort a merge the user started.
fn require_publishable_checkout(folder: &Path) -> Result<(), GitError> {
    let output = git()
        .arg("-C")
        .arg(folder)
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .map_err(GitError::Spawn)?;
    let dirty = !output.stdout.is_empty();
    succeeded("status", folder, output)?;
    let merging = git()
        .arg("-C")
        .arg(folder)
        .args(["rev-parse", "--quiet", "--verify", "MERGE_HEAD"])
        .output()
        .map_err(GitError::Spawn)?;
    if dirty || merging.status.success() {
        return Err(GitError::Failed { operation: "publish instructions", path: folder.to_owned(),
            stderr: "Finish the existing merge and commit or stash other tracked edits before publishing; the instruction commit was kept".into() });
    }
    Ok(())
}

#[cfg(all(test, windows))]
mod publication_regression {
    use super::*;

    fn run(folder: &Path, arguments: &[&str]) -> Result<String, GitError> {
        let output = git()
            .arg("-C")
            .arg(folder)
            .args(arguments)
            .output()
            .map_err(GitError::Spawn)?;
        let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        succeeded("publication regression", folder, output)?;
        Ok(text)
    }

    // #262: instruction commits must reach origin even when other PRs advance the default branch.
    #[test]
    fn instructions_publish_after_remote_advance() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let remote = directory.path().join("remote.git");
        let local = directory.path().join("main checkout with spaces");
        let other = directory.path().join("other");
        for folder in [&remote, &local, &other] {
            fs::create_dir(folder)?;
        }
        run(&remote, &["init", "--bare", "--initial-branch=master"])?;
        for folder in [&local, &other] {
            run(folder, &["init", "--initial-branch=master"])?;
            run(folder, &["config", "user.name", "Regression"])?;
            run(
                folder,
                &["config", "user.email", "regression@example.invalid"],
            )?;
            let remote_path = remote.to_string_lossy();
            run(folder, &["remote", "add", "origin", &remote_path])?;
        }
        fs::write(local.join("README"), "initial")?;
        commit_file(&local, Path::new("README"), "initial")?;
        run(&local, &["push", "origin", "master"])?;
        run(&other, &["pull", "origin", "master"])?;
        fs::write(other.join("README"), "merged PR")?;
        commit_file(&other, Path::new("README"), "remote advance")?;
        run(&other, &["push", "origin", "master"])?;
        fs::write(local.join("AGENTS.md"), "project instructions")?;
        commit_file(&local, Path::new("AGENTS.md"), "Update AGENTS.md")?;
        let hook = remote.join("hooks/pre-receive");
        fs::write(&hook, "#!/bin/sh\nexit 1\n")?;
        assert!(publish_instructions(&local).is_err());
        fs::remove_file(hook)?;
        publish_instructions(&local)?;
        assert_eq!(
            run(&remote, &["show", "master:AGENTS.md"])?,
            "project instructions"
        );
        assert_eq!(fs::read_to_string(local.join("README"))?, "merged PR");
        publish_instructions(&local)?;
        Ok(())
    }
}
