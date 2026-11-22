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

/// A project folder is a git repository when it is the root of a work tree: it holds
/// `.git` as a directory or file. Used when importing an existing git folder into Rosen.
pub fn is_repository(folder: &Path) -> bool {
    folder.join(".git").exists()
}

/// The folder exists and holds at least one entry.
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

/// The default branch advertised by origin, falling back to the checkout's branch
/// when no remote HEAD is recorded. Reads local metadata without fetching.
pub fn opened_project_default_branch(folder: &Path) -> Result<String, GitError> {
    let output = git()
        .arg("-C")
        .arg(folder)
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

/// The branch HEAD points at in an existing git checkout (used for import).
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

/// The commit `reference` names in a git repository, if it exists.
pub fn resolve_commit(repository_root: &Path, reference: &str) -> Option<String> {
    let output = git()
        .current_dir(repository_root)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("{reference}^{{commit}}"))
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let commit = String::from_utf8(output.stdout).ok()?.trim().to_owned();
    (!commit.is_empty()).then_some(commit)
}

/// The project's main checkout for a git folder, when inspecting a legacy checkout.
pub fn main_checkout(folder: &Path) -> Result<PathBuf, GitError> {
    let output = git()
        .arg("-C")
        .arg(folder)
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
    commit_files(folder, &[file], message)
}

/// Commits a related set of instruction paths together, including deletions under folders.
/// Paths outside the set remain staged and never enter this commit.
pub fn commit_files(folder: &Path, files: &[&Path], message: &str) -> Result<bool, GitError> {
    if files.is_empty() {
        return Ok(false);
    }
    let added = git()
        .arg("-C")
        .arg(folder)
        .args(["add", "--"])
        .args(files)
        .output()
        .map_err(GitError::Spawn)?;
    succeeded("add", folder, added)?;
    let unchanged = git()
        .arg("-C")
        .arg(folder)
        .args(["diff", "--cached", "--quiet", "--"])
        .args(files)
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
        .args(files)
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
        return Err(GitError::Failed {
            operation: "publish instructions",
            path: folder.to_owned(),
            stderr: "Finish the existing merge and commit or stash other tracked edits before publishing; the instruction commit was kept".into(),
        });
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
