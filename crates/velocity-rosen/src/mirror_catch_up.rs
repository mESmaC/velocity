//! Catch a project's base up to its Git mirror when another machine's server landed
//! there (#501, Rosen R1 #416), and adopt a ported mirror tip (Rosen #396).
//!
//! The owner works on two machines, each with its own server mirroring to one remote.
//! The idle machine's base falls behind and its next push is refused as
//! non-fast-forward. Rosen's catch-up fetches the mirror and, when the base is an
//! ancestor of its tip, moves the base there and rebases open changes, keeping the
//! server's database. Velocity never re-imports or replaces a database to catch up.

use std::io::Read;
use std::path::Path;
use std::process::{Child, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use rosen_client::protocol::{CatchUpStatus, ErrorCode};
use rusqlite::{Connection, OpenFlags, OptionalExtension};
use velocity_core::rosen_mirror::MirrorDivergence;

use crate::{ClientError, ProjectServer, RosenError};

/// How the project's Git mirror branch compares with Rosen's local base (#520).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MirrorTip {
    /// No mirror is configured or no base has been imported yet.
    NotConfigured,
    /// The mirror branch matches Rosen's base (or does not exist on the remote yet).
    UpToDate,
    /// Rosen's base already contains the mirror tip; the next push sends the rest.
    Ahead,
    /// The mirror branch is strictly ahead of Rosen's base by `commits` commits.
    Behind {
        commits: u32,
        base_commit: String,
        mirror_commit: String,
    },
    /// Both sides hold commits the other lacks.
    Diverged {
        base_commit: String,
        mirror_commit: String,
    },
}

struct MirrorBase {
    remote: String,
    branch: String,
    base_commit: String,
}

const LS_REMOTE_TIMEOUT: Duration = Duration::from_secs(30);
const FETCH_TIMEOUT: Duration = Duration::from_secs(60);

/// What catching up from the mirror did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CatchUp {
    /// Nothing to catch up here: Rosen's reason, such as a project with no mirror.
    NotApplicable(String),
    /// The mirror branch is at the base.
    UpToDate,
    /// The base already contains the mirror tip; the next push sends the rest.
    Ahead,
    /// The base moved forward to the mirror tip and open changes were rebased onto it.
    FastForwarded(FastForward),
    /// Both sides have commits the other lacks. Nothing moved.
    Diverged(MirrorDivergence),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FastForward {
    pub new_base: String,
    /// The mirror commits the base now holds.
    pub commits: usize,
    /// Open changes moved onto the new base.
    pub rebased: usize,
    /// Of those, the changes that now hold conflicts.
    pub conflicted: usize,
    /// Changes Rosen could not move; each rebases when it lands.
    pub rebase_errors: Vec<String>,
}

/// Rosen refuses catch-up for a project with no mirror.
const NO_MIRROR: &str = "no mirror configured";
/// Rosen takes catch-up only on the server's local endpoint. A project whose server
/// Velocity reaches over the network is caught up on that server's own machine.
const NOT_LOCAL: &str = "operator command";

/// Fetches the project's mirror and fast-forwards the base to its tip when that is
/// possible. Diverged histories come back as [`CatchUp::Diverged`], untouched.
pub async fn catch_up_from_mirror(server: &ProjectServer) -> Result<CatchUp, RosenError> {
    let client = server.connect().await?;
    let answer = match client.mirror_catch_up().await {
        Ok(answer) => answer,
        Err(ClientError::Server(body)) if body.code == ErrorCode::Refused => {
            return refusal(body.message);
        }
        Err(error) => return Err(error.into()),
    };
    Ok(match answer.status {
        CatchUpStatus::UpToDate => CatchUp::UpToDate,
        CatchUpStatus::Ahead => CatchUp::Ahead,
        CatchUpStatus::FastForwarded => CatchUp::FastForwarded(FastForward {
            new_base: answer.new_base.to_hex(),
            commits: answer.commits.len(),
            rebased: answer.rebased.len(),
            conflicted: answer.conflicted.len(),
            rebase_errors: answer
                .rebase_errors
                .into_iter()
                .map(|failure| format!("{}: {}", failure.change, failure.error))
                .collect(),
        }),
    })
}

/// Sorts Rosen's refusal into a divergence, nothing to do, or an error to report.
fn refusal(message: String) -> Result<CatchUp, RosenError> {
    if let Some(divergence) = MirrorDivergence::from_refusal(&message) {
        return Ok(CatchUp::Diverged(divergence));
    }
    if message.contains(NO_MIRROR) || message.contains(NOT_LOCAL) {
        return Ok(CatchUp::NotApplicable(message));
    }
    Err(RosenError::MirrorRefused(message))
}

/// Adopts the mirror tip `commit` as a parent of the base, for a mirror whose edits
/// were already ported into Rosen (Rosen #396). Rosen refuses when they were not.
/// Returns the new base.
pub async fn adopt_mirror_tip(server: &ProjectServer, commit: &str) -> Result<String, RosenError> {
    let commit = crate::policy::object(commit)?;
    let client = server.connect().await?;
    match client.mirror_adopt(commit).await {
        Ok(adoption) => Ok(adoption.new_base.to_hex()),
        Err(ClientError::Server(body)) if body.code == ErrorCode::Refused => {
            Err(RosenError::MirrorRefused(body.message))
        }
        Err(error) => Err(error.into()),
    }
}

/// Checks whether the project's Git mirror has moved ahead of Rosen's local base (#520).
///
/// The fast path runs only `git ls-remote` (and a local `merge_base` check when the base
/// is ahead) without pausing landing or touching Rosen's store. Only when the remote holds
/// commits Rosen lacks does this fetch into a temporary scratch repository to count them.
pub async fn probe_mirror_tip(server: &ProjectServer) -> Result<MirrorTip, RosenError> {
    let reading = server.clone();
    let Some(base) = tokio::task::spawn_blocking(move || read_mirror_base(&reading))
        .await
        .map_err(|error| RosenError::MirrorData(error.to_string()))??
    else {
        return Ok(MirrorTip::NotConfigured);
    };
    let remote = resolve_remote(server, &base.remote)?;
    let tip = {
        let dir = server.data_dir().to_path_buf();
        let remote = remote.clone();
        let branch = base.branch.clone();
        tokio::task::spawn_blocking(move || read_remote_tip(&dir, &remote, &branch))
            .await
            .map_err(|error| RosenError::MirrorData(error.to_string()))??
    };
    let Some(tip) = tip else {
        return Ok(MirrorTip::UpToDate);
    };
    if tip == base.base_commit {
        return Ok(MirrorTip::UpToDate);
    }
    if is_local_ancestor(server, &tip, &base.base_commit).await {
        return Ok(MirrorTip::Ahead);
    }
    let reading = server.clone();
    tokio::task::spawn_blocking(move || {
        count_mirror_delta(&reading, &remote, &base.branch, &base.base_commit)
    })
    .await
    .map_err(|error| RosenError::MirrorData(error.to_string()))?
}

fn read_mirror_base(server: &ProjectServer) -> Result<Option<MirrorBase>, RosenError> {
    let path = server.data_dir().join("metadata.sqlite");
    if !path.is_file() {
        return Ok(None);
    }
    let mut connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    connection.busy_timeout(Duration::from_secs(2))?;
    let transaction = connection.transaction()?;
    if !has_table(&transaction, "mirror")? || !has_table(&transaction, "base_refs")? {
        return Ok(None);
    }
    let Some(remote): Option<String> = transaction
        .query_row("SELECT remote FROM mirror WHERE id = 1", [], |row| {
            row.get(0)
        })
        .optional()?
    else {
        return Ok(None);
    };
    let Some((branch, base_commit)): Option<(String, String)> = transaction
        .query_row("SELECT name, target FROM base_refs LIMIT 1", [], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .optional()?
    else {
        return Ok(None);
    };
    Ok(Some(MirrorBase {
        remote,
        branch,
        base_commit,
    }))
}

fn has_table(connection: &Connection, name: &str) -> Result<bool, rusqlite::Error> {
    connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [name],
        |row| row.get(0),
    )
}

/// Resolves a relative mirror path the same way `rosen-server` does (against `mirror.git`).
fn resolve_remote(server: &ProjectServer, remote: &str) -> Result<String, RosenError> {
    if !Path::new(remote).is_absolute() && !remote.contains(':') {
        let staging = std::path::absolute(server.data_dir().join("mirror.git"))
            .map_err(|error| RosenError::MirrorData(error.to_string()))?;
        return Ok(staging.join(remote).to_string_lossy().into_owned());
    }
    Ok(remote.to_owned())
}

fn read_remote_tip(
    directory: &Path,
    remote: &str,
    branch: &str,
) -> Result<Option<String>, RosenError> {
    let full = format!("refs/heads/{branch}");
    let output = run_git(
        directory,
        remote,
        &["ls-remote", "--", remote, &full],
        LS_REMOTE_TIMEOUT,
    )?;
    for line in output.lines() {
        if let Some((commit, reference)) = line.split_once('\t')
            && reference.trim() == full
        {
            return Ok(Some(commit.trim().to_owned()));
        }
    }
    Ok(None)
}

async fn is_local_ancestor(server: &ProjectServer, tip: &str, base: &str) -> bool {
    let Ok(tip_identifier) = crate::policy::object(tip) else {
        return false;
    };
    let Ok(client) = crate::connection::requests(server.endpoint()).await else {
        return false;
    };
    client
        .merge_base(tip, base)
        .await
        .is_ok_and(|answer| answer.merge_base == Some(tip_identifier))
}

fn count_mirror_delta(
    server: &ProjectServer,
    remote: &str,
    branch: &str,
    base_commit: &str,
) -> Result<MirrorTip, RosenError> {
    let scratch = tempfile::tempdir_in(server.data_dir())
        .or_else(|_| tempfile::tempdir())
        .map_err(|error| RosenError::MirrorData(error.to_string()))?;
    let repo = scratch.path();
    prepare_scratch_repo(repo, &server.data_dir().join("objects"), remote)?;
    const FETCHED: &str = "refs/velocity-mirror-tip";
    let refspec = format!("refs/heads/{branch}:{FETCHED}");
    run_git(
        repo,
        remote,
        &[
            "fetch",
            "--quiet",
            "--no-tags",
            "--no-write-fetch-head",
            "--no-auto-maintenance",
            "--no-recurse-submodules",
            "--",
            remote,
            &refspec,
        ],
        FETCH_TIMEOUT,
    )?;
    let fetched = run_git(
        repo,
        remote,
        &["rev-parse", "--verify", FETCHED],
        LS_REMOTE_TIMEOUT,
    )?
    .trim()
    .to_owned();
    let only_remote = rev_list_count(repo, remote, base_commit, &fetched)?;
    let only_base = rev_list_count(repo, remote, &fetched, base_commit)?;
    Ok(match (only_base, only_remote) {
        (_, 0) if fetched == base_commit => MirrorTip::UpToDate,
        (_, 0) => MirrorTip::Ahead,
        (0, behind) => MirrorTip::Behind {
            commits: u32::try_from(behind).unwrap_or(u32::MAX),
            base_commit: base_commit.to_owned(),
            mirror_commit: fetched,
        },
        (_, _) => MirrorTip::Diverged {
            base_commit: base_commit.to_owned(),
            mirror_commit: fetched,
        },
    })
}

fn prepare_scratch_repo(repo: &Path, objects_dir: &Path, remote: &str) -> Result<(), RosenError> {
    run_git(
        repo,
        remote,
        &["init", "--bare", "--quiet"],
        LS_REMOTE_TIMEOUT,
    )?;
    run_git(repo, remote, &["config", "gc.auto", "0"], LS_REMOTE_TIMEOUT)?;
    let objects = std::path::absolute(objects_dir)
        .map_err(|error| RosenError::MirrorData(error.to_string()))?;
    let line = format!("{}\n", objects.to_string_lossy().replace('\\', "/"));
    let info = repo.join("objects").join("info");
    std::fs::create_dir_all(&info).map_err(|error| RosenError::MirrorData(error.to_string()))?;
    std::fs::write(info.join("alternates"), line)
        .map_err(|error| RosenError::MirrorData(error.to_string()))?;
    Ok(())
}

fn rev_list_count(repo: &Path, remote: &str, from: &str, to: &str) -> Result<u64, RosenError> {
    let range = format!("{from}..{to}");
    let output = run_git(
        repo,
        remote,
        &["rev-list", "--count", &range],
        LS_REMOTE_TIMEOUT,
    )?;
    output
        .trim()
        .parse()
        .map_err(|error| RosenError::MirrorData(format!("invalid commit count: {error}")))
}

fn run_git(
    directory: &Path,
    remote: &str,
    arguments: &[&str],
    timeout: Duration,
) -> Result<String, RosenError> {
    let mut command = crate::process::hidden_command("git");
    command
        .current_dir(directory)
        .args(["-c", "core.longpaths=true", "-c", "gc.auto=0"]);
    if uses_collapse_api_host(remote) {
        command.args(["-c", "http.https://api.collapse.sh/.proactiveAuth=basic"]);
    }
    let mut child = command
        .args(arguments)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| RosenError::MirrorData(format!("could not run git: {error}")))?;
    let stdout = child.stdout.take().map(drain_pipe);
    let stderr = child.stderr.take().map(drain_pipe);
    let status = wait_with_timeout(&mut child, timeout)?;
    let stdout = join_pipe(stdout);
    let stderr = join_pipe(stderr);
    if !status.success() {
        let detail = hide_credentials(remote, stderr.trim());
        return Err(RosenError::MirrorData(format!("git {status}: {detail}")));
    }
    Ok(stdout)
}

fn drain_pipe<R: Read + Send + 'static>(mut reader: R) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut buffer = Vec::new();
        let _ = reader.read_to_end(&mut buffer);
        String::from_utf8_lossy(&buffer).into_owned()
    })
}

fn join_pipe(handle: Option<std::thread::JoinHandle<String>>) -> String {
    handle
        .and_then(|thread| thread.join().ok())
        .unwrap_or_default()
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Result<ExitStatus, RosenError> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(status),
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RosenError::MirrorData(format!(
                    "git timed out after {} seconds",
                    timeout.as_secs()
                )));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => return Err(RosenError::MirrorData(error.to_string())),
        }
    }
}

/// Private Collapse repositories answer unauthenticated reads with 404 rather than a
/// login challenge, so reads against Collapse's API host send Basic auth proactively (#571).
fn uses_collapse_api_host(remote: &str) -> bool {
    const PREFIX: &str = "https://api.collapse.sh/";
    remote
        .trim()
        .get(..PREFIX.len())
        .is_some_and(|head| head.eq_ignore_ascii_case(PREFIX))
}

/// Replaces any `user:token@` prefix in `remote` so credentials never reach logs or UI.
fn hide_credentials(remote: &str, text: &str) -> String {
    let Some((scheme, rest)) = remote.split_once("://") else {
        return text.to_owned();
    };
    let host_start = rest.find('/').unwrap_or(rest.len());
    match rest[..host_start].rfind('@') {
        Some(at) if at > 0 => {
            let safe = format!("{scheme}://***@{}", &rest[at + 1..]);
            text.replace(remote, &safe).replace(&rest[..=at], "***@")
        }
        _ => text.to_owned(),
    }
}
