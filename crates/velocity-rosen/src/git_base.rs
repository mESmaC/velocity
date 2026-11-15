//! Carry Rosen's landed history into git when a project switches back (F5, #369).
//! The pinned Rosen store uses git loose objects; an export repository reads them
//! through alternates, just as Rosen's mirror does, without modifying its store.

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::{ProjectServer, RosenError};

// Pool refreshes and concurrent launches share the export ref and branch update.
static SYNCHRONIZING: Mutex<BTreeMap<PathBuf, Arc<Mutex<()>>>> = Mutex::new(BTreeMap::new());
const EXPORT_REF: &str = "refs/heads/velocity-export";

/// Returns the synchronized git tip, or `None` if Rosen never imported this project.
/// A missing mirror is fine: the objects come straight from Rosen's local store.
pub fn synchronize_git_base(
    server: &ProjectServer,
    repository: &Path,
    branch: &str,
) -> Result<Option<String>, RosenError> {
    let synchronizing = {
        let mut projects = SYNCHRONIZING.lock().map_err(failure)?;
        Arc::clone(projects.entry(server.data_dir().to_path_buf()).or_default())
    };
    let _synchronizing = synchronizing.lock().map_err(failure)?;
    let Some(base) = read_base(server)? else {
        return Ok(None);
    };
    crate::policy::object(&base)?;
    export_base(server, repository, &base)?;
    let reference = format!("refs/heads/{branch}");
    fast_forward(repository, &reference, &base)?;
    // Git merges can continue after the switch. A successful fetch may be newer than
    // Rosen's last land; an offline or mirror-less project keeps the local landed tip.
    let remote_refspec = format!("+refs/heads/{branch}:refs/remotes/origin/{branch}");
    if git(repository, ["fetch", "--quiet", "origin", &remote_refspec])?
        .status
        .success()
    {
        let remote = format!("refs/remotes/origin/{branch}");
        let tip = checked_git(repository, ["rev-parse", "--verify", &remote])?;
        fast_forward(repository, &reference, tip.trim())?;
    }
    checked_git(repository, ["rev-parse", "--verify", &reference])
        .map(|tip| Some(tip.trim().to_owned()))
}

fn read_base(server: &ProjectServer) -> Result<Option<String>, RosenError> {
    let metadata = server.data_dir().join("metadata.sqlite");
    if !metadata.is_file() {
        return Ok(None);
    }
    let connection =
        Connection::open_with_flags(metadata, OpenFlags::SQLITE_OPEN_READ_ONLY).map_err(failure)?;
    connection
        .busy_timeout(std::time::Duration::from_secs(2))
        .map_err(failure)?;
    connection
        .query_row("SELECT target FROM base_refs LIMIT 1", [], |row| row.get(0))
        .optional()
        .map_err(failure)
}

fn export_base(server: &ProjectServer, repository: &Path, base: &str) -> Result<(), RosenError> {
    let staging = server.data_dir().join("velocity-git-export");
    checked_git(
        repository,
        [
            OsStr::new("init"),
            OsStr::new("--bare"),
            OsStr::new("--quiet"),
            staging.as_os_str(),
        ],
    )?;
    let objects = std::path::absolute(server.data_dir().join("objects")).map_err(failure)?;
    let alternates = staging.join("objects/info/alternates");
    std::fs::write(
        alternates,
        format!("{}\n", objects.to_string_lossy().replace('\\', "/")),
    )
    .map_err(failure)?;
    checked_git(&staging, ["config", "gc.auto", "0"])?;
    checked_git(&staging, ["update-ref", EXPORT_REF, base])?;
    // Fetch copies reachable objects into git's own storage. The checkout never
    // depends on Rosen keeping its store or this read-only alternate alive.
    checked_git(
        repository,
        [
            OsStr::new("fetch"),
            OsStr::new("--quiet"),
            OsStr::new("--no-tags"),
            OsStr::new("--no-write-fetch-head"),
            OsStr::new("--"),
            staging.as_os_str(),
            OsStr::new(EXPORT_REF),
        ],
    )?;
    Ok(())
}

fn fast_forward(repository: &Path, reference: &str, target: &str) -> Result<(), RosenError> {
    let current = checked_git(repository, ["rev-parse", "--verify", reference])?;
    let current = current.trim();
    if current == target || is_ancestor(repository, target, current)? {
        return Ok(());
    }
    if !is_ancestor(repository, current, target)? {
        return Err(failure(format!(
            "cannot start git worktrees: {reference} at {current} has diverged from \
             landed history at {target}; reconcile the histories before starting new work"
        )));
    }
    if let Some(checkout) = branch_checkout(repository, reference)? {
        // merge --ff-only preserves the checkout's index and uncommitted files;
        // updating only its ref would leave them describing the old branch tip.
        checked_git(
            &checkout,
            ["merge", "--ff-only", "--no-autostash", "--no-edit", target],
        )?;
    } else {
        // Compare-and-swap refuses a concurrent branch update rather than losing it.
        checked_git(repository, ["update-ref", reference, target, current])?;
    }
    Ok(())
}

fn is_ancestor(repository: &Path, ancestor: &str, descendant: &str) -> Result<bool, RosenError> {
    let output = git(
        repository,
        ["merge-base", "--is-ancestor", ancestor, descendant],
    )?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(command_failure(&output)),
    }
}

fn branch_checkout(
    repository: &Path,
    reference: &str,
) -> Result<Option<std::path::PathBuf>, RosenError> {
    let listing = checked_git(repository, ["worktree", "list", "--porcelain", "-z"])?;
    let mut checkout = None;
    for field in listing.split('\0') {
        if let Some(path) = field.strip_prefix("worktree ") {
            checkout = Some(std::path::PathBuf::from(path));
        } else if field.strip_prefix("branch ") == Some(reference) {
            return Ok(checkout);
        }
    }
    Ok(None)
}

fn git<I, S>(repository: &Path, arguments: I) -> Result<Output, RosenError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    crate::process::hidden_command("git")
        .current_dir(repository)
        .args(["-c", "core.longpaths=true", "-c", "gc.auto=0"])
        .args(arguments)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .output()
        .map_err(failure)
}

fn checked_git<I, S>(repository: &Path, arguments: I) -> Result<String, RosenError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = git(repository, arguments)?;
    if !output.status.success() {
        return Err(command_failure(&output));
    }
    String::from_utf8(output.stdout).map_err(failure)
}

fn command_failure(output: &Output) -> RosenError {
    failure(format!(
        "git {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

fn failure(error: impl std::fmt::Display) -> RosenError {
    RosenError::GitBase(error.to_string())
}
