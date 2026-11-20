//! Replacing a project server that runs another Rosen build than this Velocity's (#515).
//!
//! A project's server outlives Velocity, so agents' watchers keep storing across an app
//! restart. After an update that left the old build serving every project until someone
//! ended its process by hand. The first time a run makes a project's server ready, the
//! running server's executable is compared with the one this Velocity starts, and another
//! build is stopped with Rosen's own shutdown request and started again from this one, on
//! the same data folder and endpoint.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::connection::Connection;
use crate::server::{BINARY_FOLDER_ENV, ProjectServer, binary_folder, executable};

/// How long a replacement waits for the lands this run has in flight on the server.
const LAND_WAIT: Duration = Duration::from_secs(30);
/// How long the old server has to exit once asked. Its shutdown can't be taken back, so
/// this is generous: Rosen drains its connections for two seconds, then flushes.
const STOP_WAIT: Duration = Duration::from_secs(30);
/// How long it has when the request to stop failed, in case it arrived all the same.
const ASK_FAILED_WAIT: Duration = Duration::from_secs(3);
/// Commits are shown by this many characters, as GitHub shows them.
const SHORT_COMMIT: usize = 7;

/// A Rosen build, named for the owner: by its commit when a release unpacked it and named
/// it, otherwise by the folder its server runs from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RosenBuild {
    Commit(String),
    Folder(PathBuf),
}

impl RosenBuild {
    fn of(server_executable: &Path) -> Self {
        match crate::bundled::commit_beside(server_executable) {
            Some(commit) => Self::Commit(commit),
            None => Self::Folder(
                server_executable
                    .parent()
                    .unwrap_or(server_executable)
                    .to_path_buf(),
            ),
        }
    }

    /// The short commit, or the folder after `folder_words`: `the build in C:\…`.
    fn described(&self, folder_words: &str) -> String {
        match self {
            Self::Commit(commit) => commit.get(..SHORT_COMMIT).unwrap_or(commit).to_owned(),
            Self::Folder(folder) => format!("{folder_words} {}", plain(folder)),
        }
    }
}

/// What became of a project server found running another Rosen build than this Velocity's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ServerUpdate {
    /// This Velocity's build now serves the project.
    Replaced { build: RosenBuild },
    /// The other build still serves the project; the next start of Velocity tries again.
    Kept { running: RosenBuild, reason: String },
}

impl ServerUpdate {
    /// What the project shows: `Rosen updated to 732a44e`, or
    /// `running Rosen cd537b7; restart to update` beside [`Self::reason`].
    pub fn headline(&self) -> String {
        match self {
            Self::Replaced { build } => {
                format!("Rosen updated to {}", build.described("the build in"))
            }
            Self::Kept { running, .. } => format!(
                "running Rosen {}; restart to update",
                running.described("from")
            ),
        }
    }

    /// Why the server could not be replaced.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Replaced { .. } => None,
            Self::Kept { reason, .. } => Some(reason),
        }
    }
}

impl ProjectServer {
    /// Replaces the server running for this project when it is another Rosen build than
    /// this Velocity's, and says what happened. `None` when no server runs, it is already
    /// this build, or this Velocity has no build of its own to insist on.
    ///
    /// Call it before this run starts a watcher or a land on the server. Watchers in other
    /// processes journal while the server is away and upload once it is back (Rosen R2),
    /// and this run's lands are held off for the swap.
    pub(crate) async fn update_running(&self) -> Option<ServerUpdate> {
        let wanted = own_server()?;
        let (pid, running) = self.running_server().await?;
        let compared = (running.clone(), wanted.clone());
        let same = tokio::task::spawn_blocking(move || same_build(&compared.0, &compared.1))
            .await
            .ok()?;
        if same {
            return None;
        }
        Some(match self.replace(pid, &running, &wanted).await {
            Ok(()) => ServerUpdate::Replaced {
                build: RosenBuild::of(&wanted),
            },
            Err(reason) => ServerUpdate::Kept {
                running: RosenBuild::of(&running),
                reason,
            },
        })
    }

    /// The process and executable of the server listening on this project's endpoint.
    async fn running_server(&self) -> Option<(u32, PathBuf)> {
        let pid = {
            let client = Connection::dedicated(self.endpoint()).await.ok()?;
            client.status().await.ok()?.pid
        };
        let found = tokio::task::spawn_blocking(move || executable_of(pid))
            .await
            .ok()
            .flatten();
        if found.is_none() {
            // Left alone rather than replaced on a guess: it may well be this build.
            eprintln!(
                "Rosen server on {}: could not read the executable of process {pid}",
                self.endpoint()
            );
        }
        Some((pid, found?))
    }

    /// Stops the server `pid` running from `running` and starts `wanted` in its place. On
    /// failure says why; the old build is started again when the new one would not start,
    /// so the project is never left without a server.
    async fn replace(&self, pid: u32, running: &Path, wanted: &Path) -> Result<(), String> {
        let gate = crate::landing::land_gate(self.endpoint());
        let Ok(_no_lands) = tokio::time::timeout(LAND_WAIT, gate.write()).await else {
            return Err(format!(
                "a land was still in progress after {} seconds",
                LAND_WAIT.as_secs()
            ));
        };
        let asked = self.request_shutdown().await;
        // A request that failed may still have arrived, or the server may have gone
        // already, so the process decides; it is only given long when it was asked.
        let wait = if asked.is_ok() {
            STOP_WAIT
        } else {
            ASK_FAILED_WAIT
        };
        if !exited(pid, running, wait).await {
            return Err(asked.err().unwrap_or_else(|| {
                format!(
                    "the running server did not exit within {} seconds of being asked to stop",
                    STOP_WAIT.as_secs()
                )
            }));
        }
        crate::connection::forget(self.endpoint());
        match self.start_and_wait(wanted).await {
            Ok(()) => self.confirm_running(wanted).await,
            Err(failed) => Err(self.restore(running, failed).await),
        }
    }

    /// Another client can start a server in the moment none listens, and its start wins
    /// the endpoint; only this build answering counts as replaced.
    async fn confirm_running(&self, wanted: &Path) -> Result<(), String> {
        let Some((_, now)) = self.running_server().await else {
            return Err("the server that answers after the restart could not be identified".into());
        };
        let wanted = wanted.to_path_buf();
        let same = tokio::task::spawn_blocking(move || same_build(&now, &wanted)).await;
        if matches!(same, Ok(true)) {
            return Ok(());
        }
        Err("another client started its own server first while this one was stopped".into())
    }

    /// Starts the previous build again after this Velocity's would not start, and says
    /// what happened. An older Rosen can fail to open data a newer one wrote, after a
    /// downgrade, and the project must not be left without a server for that.
    async fn restore(&self, previous: &Path, failed: FailedStart) -> String {
        if failed.still_running {
            // It may yet listen, and two servers must never open one data folder.
            return format!("{}; it is still starting", failed.reason);
        }
        match self.start_and_wait(previous).await {
            Ok(()) => format!("{}; the previous build was started again", failed.reason),
            Err(again) => format!(
                "{}; the previous build did not start again either: {}",
                failed.reason, again.reason
            ),
        }
    }

    /// Rosen's graceful stop: it refuses new work, answers what is in flight and exits.
    async fn request_shutdown(&self) -> Result<(), String> {
        let client = Connection::dedicated(self.endpoint())
            .await
            .map_err(|error| format!("could not reach the running server: {error}"))?;
        client
            .shutdown()
            .await
            .map_err(|error| format!("the running server refused to stop: {error}"))
    }

    async fn start_and_wait(&self, binary: &Path) -> Result<(), FailedStart> {
        let mut child = self.start_binary(binary).map_err(|error| FailedStart {
            reason: error.to_string(),
            still_running: false,
        })?;
        let listening = self.wait_until_listening(&mut child).await;
        listening.map(|_| ()).map_err(|error| FailedStart {
            reason: error.to_string(),
            still_running: matches!(child.try_wait(), Ok(None)),
        })
    }
}

/// Why a server did not start, and whether the process started for it is still running.
struct FailedStart {
    reason: String,
    still_running: bool,
}

/// The `rosen-server` this Velocity insists on: the one built into it, or the one in the
/// folder [`BINARY_FOLDER_ENV`] names. A build that only finds Rosen beside itself or on
/// `PATH` has none of its own, and leaves a running server as it is: a development build
/// sharing a folder's server with the installed app must not swap it out.
fn own_server() -> Option<PathBuf> {
    let own = std::env::var_os(BINARY_FOLDER_ENV)
        .map(PathBuf::from)
        .or_else(crate::bundled::folder)?;
    let used = binary_folder().ok()?;
    (own == used).then(|| used.join(executable("rosen-server")))
}

/// The executable process `pid` runs, when the process exists and can be read.
fn executable_of(pid: u32) -> Option<PathBuf> {
    let mut system = System::new().ok()?;
    let pid = Pid::from_u32(pid);
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    system.process(pid)?.exe().map(Path::to_path_buf)
}

/// Waits, `wait` at most, until process `pid` no longer runs `running`. The endpoint
/// closes before the server has flushed and exited, and the next server must not open
/// its data until then.
async fn exited(pid: u32, running: &Path, wait: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let found = tokio::task::spawn_blocking(move || executable_of(pid)).await;
        // Another executable under the same number is a new process: the server is gone.
        if !matches!(&found, Ok(Some(still)) if same_path(still, running)) {
            return found.is_ok();
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Whether two server executables are one build: the same file, or files with the same
/// bytes, as two folders holding one release are.
fn same_build(running: &Path, wanted: &Path) -> bool {
    same_path(running, wanted) || same_bytes(running, wanted)
}

fn same_path(left: &Path, right: &Path) -> bool {
    comparable(left) == comparable(right)
}

/// A path as it compares on this system: resolved, and without letter case on Windows,
/// where a process reports its executable in whatever case it was started with.
fn comparable(path: &Path) -> String {
    let resolved = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let text = plain(&resolved);
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

/// A path without the `\\?\` prefix Windows gives a resolved or long path.
fn plain(path: &Path) -> String {
    let text = path.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => rest.to_owned(),
        _ => text.into_owned(),
    }
}

fn same_bytes(left: &Path, right: &Path) -> bool {
    let size = |path: &Path| std::fs::metadata(path).map(|found| found.len()).ok();
    if size(left).is_none() || size(left) != size(right) {
        return false;
    }
    matches!((std::fs::read(left), std::fs::read(right)), (Ok(left), Ok(right)) if left == right)
}
