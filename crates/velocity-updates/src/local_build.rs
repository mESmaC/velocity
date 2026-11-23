//! Local builds of Velocity's own landed commits (SPEC.md F1 "Local builds", #512, #551):
//! the version a build gets, the builds kept on disk for a restart or a rollback, ancestry
//! checks preventing downgrades, and what the title bar offers. `src-tauri` runs the build
//! and the restart; nothing here is published, and no build made this way is signed with
//! the release key.
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// How long auto-restart waits before retrying a commit whose restart failed (#605).
pub const AUTO_RESTART_RETRY_DELAY: Duration = Duration::from_secs(10 * 60);
/// How long a restarted local build must pass startup and renderer heartbeat checks before
/// its rollback guard is cleared (#609).
pub const POST_RESTART_HEALTH_WINDOW: Duration = Duration::from_secs(3 * 60);
/// Maximum wall-clock age for a persisted `pending-restart.json` record before it is treated
/// as stale (for example after an ordinary manual shutdown) (#609).
pub const PENDING_RESTART_MAX_AGE_SECS: u64 = 10 * 60;

/// Live work that holds off an idle auto-restart into a newer local build (#609).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct IdleBlockers {
    /// An agent turn, setup, startup recovery, or duty agent is mid-flight.
    pub turn_in_progress: bool,
    /// A Rosen land, bindings check, or post-land verification check is running.
    pub land_in_progress: bool,
    /// A Rosen deploy step is running or queued.
    pub deploy_in_progress: bool,
}

impl IdleBlockers {
    pub fn is_idle(self) -> bool {
        !self.turn_in_progress && !self.land_in_progress && !self.deploy_in_progress
    }
}

/// Persisted before restarting into a newer local build so startup or heartbeat failure
/// within [`POST_RESTART_HEALTH_WINDOW`] rolls back to the previous build and marks the
/// new commit as failed (#609).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRestart {
    /// The new commit Velocity restarted into.
    pub commit: String,
    /// The commit Velocity was running before the restart, when known.
    #[serde(default)]
    pub previous_commit: Option<String>,
    /// Fallback executable filename under `local-builds/` when the previous copy is not
    /// itself a kept local build (for example a published release).
    #[serde(default)]
    pub rollback_file: Option<String>,
    /// Expected size in bytes of `rollback_file`, verified before restoring it.
    #[serde(default)]
    pub rollback_bytes: u64,
    /// Seconds since the Unix epoch when the restart was initiated.
    #[serde(default)]
    pub started_at: u64,
}

impl PendingRestart {
    /// `true` when `running_commit` identifies the commit this restart targeted.
    pub fn matches_running(&self, running_commit: Option<&str>) -> bool {
        running_commit.is_some_and(|running| commits_match(&self.commit, running))
    }

    /// `true` when the record is older than [`PENDING_RESTART_MAX_AGE_SECS`].
    pub fn is_stale(&self, now_unix_secs: u64) -> bool {
        self.started_at > 0
            && now_unix_secs.saturating_sub(self.started_at) > PENDING_RESTART_MAX_AGE_SECS
    }
}

/// The last failed auto-restart attempt, held so Velocity retries after
/// [`AUTO_RESTART_RETRY_DELAY`] or after a newer local build instead of skipping forever (#605).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FailedAutoRestart {
    pub commit: String,
    pub reason: String,
    pub failed_at: Instant,
}

impl FailedAutoRestart {
    pub fn new(commit: String, reason: String, failed_at: Instant) -> Self {
        Self {
            commit,
            reason,
            failed_at,
        }
    }

    /// `true` while `commit` is still inside its retry backoff window.
    pub fn blocks(&self, commit: &str, now: Instant) -> bool {
        self.commit.eq_ignore_ascii_case(commit)
            && now.saturating_duration_since(self.failed_at) < AUTO_RESTART_RETRY_DELAY
    }
}

const FOLDER: &str = "local-builds";
const RECORD: &str = "build.json";
const FAILURE: &str = "failed.json";
const SETTINGS: &str = "settings.json";
const PENDING_RESTART: &str = "pending-restart.json";
const ROLLBACK_EXECUTABLE: &str = "rollback.exe";
/// What `scripts/release-local.ps1` names the executable it copies to its output folder.
const EXECUTABLE_SUFFIX: &str = "-portable.exe";
/// The newest build and the one before it, which is the rollback.
pub const KEPT_BUILDS: usize = 2;
const SHORT_COMMIT: usize = 7;

#[derive(Debug, thiserror::Error)]
pub enum LocalBuildError {
    #[error("{0} is not a commit ID")]
    InvalidCommit(String),
    #[error("The build left no executable in {folder}")]
    NoExecutable { folder: String },
    #[error("{path} is not the file the build recorded, so it was not started")]
    Changed { path: String },
    #[error("Could not {action} {path}: {source}")]
    Io {
        action: &'static str,
        path: String,
        source: std::io::Error,
    },
}

fn io_error(action: &'static str, path: &Path) -> impl FnOnce(std::io::Error) -> LocalBuildError {
    let path = path.display().to_string();
    move |source| LocalBuildError::Io {
        action,
        path,
        source,
    }
}

/// The first seven characters of a commit ID, as the title bar shows it.
pub fn short_commit(commit: &str) -> &str {
    commit.get(..SHORT_COMMIT).unwrap_or(commit)
}

/// The published release `version` was built on, and the commit when `version` carries one
/// (a `.local.g<commit>` prerelease or `+g<commit>`/`+<40-hex>` build metadata).
/// `0.1.0-beta.13.local.gabc1234` was built on `0.1.0-beta.13`, and
/// `0.2.1-local.gabc1234` on `0.2.0`.
pub fn release_base(version: &str) -> (String, Option<String>) {
    let (without_meta, metadata) = match version.split_once('+') {
        Some((head, meta)) => (head, Some(meta)),
        None => (version, None),
    };
    let meta_commit = metadata.and_then(metadata_commit);
    let published = || (without_meta.to_owned(), meta_commit.clone());
    let Some((core, prerelease)) = without_meta.split_once('-') else {
        return published();
    };
    let identifiers: Vec<&str> = prerelease.split('.').collect();
    let [earlier @ .., "local", commit] = identifiers.as_slice() else {
        return published();
    };
    let Some(commit) = commit.strip_prefix('g').filter(|commit| is_commit(commit)) else {
        return published();
    };
    let base = if earlier.is_empty() {
        with_patch(core, |patch| patch.checked_sub(1)).unwrap_or_else(|| core.to_owned())
    } else {
        format!("{core}-{}", earlier.join("."))
    };
    (base, Some(commit.to_ascii_lowercase()))
}

fn metadata_commit(metadata: &str) -> Option<String> {
    for part in metadata.split('.') {
        if let Some(hex) = part.strip_prefix('g')
            && (SHORT_COMMIT..=40).contains(&hex.len())
            && is_commit(hex)
        {
            return Some(hex.to_ascii_lowercase());
        }
        if part.len() == 40 && is_commit(part) {
            return Some(part.to_ascii_lowercase());
        }
    }
    None
}

/// The version a local build of `commit` gets when the running copy has `running_version`.
/// It sorts above the published release it was built on and below the next one, so the
/// update check never offers the release it already contains and still offers the next. A
/// numbered release has no prerelease to extend, so its patch number goes up by one first.
/// The `g` keeps a commit that is all digits a valid prerelease identifier.
pub fn local_version(running_version: &str, commit: &str) -> String {
    let (base, _) = release_base(running_version);
    let short = short_commit(commit);
    if base.contains('-') {
        return format!("{base}.local.g{short}");
    }
    let next = with_patch(&base, |patch| patch.checked_add(1)).unwrap_or(base);
    format!("{next}-local.g{short}")
}

fn with_patch(core: &str, change: impl FnOnce(u64) -> Option<u64>) -> Option<String> {
    let (earlier, patch) = core.rsplit_once('.')?;
    let patch = change(patch.parse().ok()?)?;
    Some(format!("{earlier}.{patch}"))
}

fn is_commit(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn normalize_commit(text: &str) -> Option<String> {
    let trimmed = text.trim().to_ascii_lowercase();
    let valid_len = (SHORT_COMMIT..=40).contains(&trimmed.len());
    (valid_len && is_commit(&trimmed)).then_some(trimmed)
}

fn commits_match(left: &str, right: &str) -> bool {
    if left.len() < SHORT_COMMIT || right.len() < SHORT_COMMIT {
        return false;
    }
    let left = left.to_ascii_lowercase();
    let right = right.to_ascii_lowercase();
    left == right || left.starts_with(&right) || right.starts_with(&left)
}

/// How a candidate local-build commit relates to the running build's commit (#551).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CommitRelation {
    /// The candidate strictly descends from the running build's commit.
    Descendant,
    /// The candidate is the same commit as the running build.
    Same,
    /// The candidate is a strict ancestor of the running build (a downgrade).
    Ancestor,
    /// Both commits are known, and neither is an ancestor of the other.
    Diverged,
    /// The running build's commit is unknown, or Rosen could not verify ancestry.
    #[default]
    Unknown,
}

/// A finished local build, as its folder's record holds it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalBuild {
    pub commit: String,
    pub version: String,
    /// Seconds since the Unix epoch when the build finished.
    #[serde(default)]
    pub built_at: u64,
    /// The executable's size, checked again before a restart starts it.
    #[serde(default)]
    pub executable_bytes: u64,
}

/// The last commit whose build failed. It is not built again: the next green commit is.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedBuild {
    pub commit: String,
    #[serde(default)]
    pub reason: String,
}

fn default_auto_restart() -> bool {
    true
}

/// Saved local-build preferences (`local-builds/settings.json`) (#601, #609).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalBuildSettings {
    /// Restart into the newest local build automatically when waiting issues need its
    /// agent tooling, or when Velocity is idle with no turn, land, deploy or local build
    /// in progress.
    #[serde(default = "default_auto_restart")]
    pub auto_restart: bool,
}

impl Default for LocalBuildSettings {
    fn default() -> Self {
        Self {
            auto_restart: default_auto_restart(),
        }
    }
}

/// The builds on disk, one folder per commit under the app data folder.
#[derive(Clone, Debug)]
pub struct LocalBuilds {
    folder: PathBuf,
}

impl LocalBuilds {
    pub fn new(app_data: &Path) -> Self {
        Self {
            folder: app_data.join(FOLDER),
        }
    }

    /// Where the build of `commit` keeps its downloads: the executable and the setup.
    pub fn folder_of(&self, commit: &str) -> PathBuf {
        self.folder.join(commit)
    }

    /// An empty folder for the build of `commit` to copy its downloads to.
    pub fn prepare(&self, commit: &str) -> Result<PathBuf, LocalBuildError> {
        if !is_commit(commit) {
            return Err(LocalBuildError::InvalidCommit(commit.to_owned()));
        }
        let folder = self.folder_of(commit);
        match std::fs::remove_dir_all(&folder) {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                return Err(io_error("empty", &folder)(error));
            }
            _ => {}
        }
        std::fs::create_dir_all(&folder).map_err(io_error("create", &folder))?;
        Ok(folder)
    }

    /// Records the build [`Self::prepare`] made room for, once its executable is there, and
    /// removes every build but the newest [`KEPT_BUILDS`].
    pub fn finish(&self, commit: &str, version: &str) -> Result<LocalBuild, LocalBuildError> {
        let folder = self.folder.join(commit);
        let executable = executable_in(&folder)?;
        let size = std::fs::metadata(&executable).map_err(io_error("read", &executable))?;
        let build = LocalBuild {
            commit: commit.to_owned(),
            version: version.to_owned(),
            built_at: unix_seconds(),
            executable_bytes: size.len(),
        };
        // Written whole or not at all: a folder without a record is an unfinished build.
        let (record, next) = (folder.join(RECORD), folder.join("build.json.next"));
        let text = serde_json::to_string_pretty(&build)
            .map_err(|error| io_error("write", &record)(error.into()))?;
        std::fs::write(&next, text).map_err(io_error("write", &next))?;
        std::fs::rename(&next, &record).map_err(io_error("write", &record))?;
        let _ = std::fs::remove_file(self.folder.join(FAILURE));
        self.prune();
        Ok(build)
    }

    /// The finished builds, newest first.
    pub fn list(&self) -> Vec<LocalBuild> {
        let mut builds: Vec<LocalBuild> = self
            .folders()
            .into_iter()
            .filter_map(|folder| {
                let build: LocalBuild = read_json(&folder.join(RECORD))?;
                // The folder is named for the commit; a record that disagrees is not trusted.
                (folder.file_name()?.to_str()? == build.commit).then_some(build)
            })
            .collect();
        builds.sort_by(|left, right| {
            (right.built_at, &right.commit).cmp(&(left.built_at, &left.commit))
        });
        builds
    }

    /// The build's executable, read whole. A file whose size is not the recorded one was
    /// changed or cut short after the build, and is never started.
    pub fn read_executable(&self, build: &LocalBuild) -> Result<Vec<u8>, LocalBuildError> {
        let executable = executable_in(&self.folder.join(&build.commit))?;
        let bytes = std::fs::read(&executable).map_err(io_error("read", &executable))?;
        if bytes.is_empty() || bytes.len() as u64 != build.executable_bytes {
            return Err(LocalBuildError::Changed {
                path: executable.display().to_string(),
            });
        }
        Ok(bytes)
    }

    pub fn failure(&self) -> Option<FailedBuild> {
        read_json(&self.folder.join(FAILURE))
    }

    pub fn record_failure(&self, failure: &FailedBuild) -> Result<(), LocalBuildError> {
        let path = self.folder.join(FAILURE);
        let next = self.folder.join("failed.json.next");
        std::fs::create_dir_all(&self.folder).map_err(io_error("create", &self.folder))?;
        let text = serde_json::to_string_pretty(failure)
            .map_err(|error| io_error("write", &path)(error.into()))?;
        std::fs::write(&next, text).map_err(io_error("write", &next))?;
        std::fs::rename(&next, &path).map_err(io_error("write", &path))
    }

    /// Marks `failure.commit` as failed and removes its kept local-build folder (if any) so
    /// Velocity neither rebuilds nor auto-restarts into it again (#609).
    pub fn mark_build_failed(&self, failure: &FailedBuild) -> Result<(), LocalBuildError> {
        self.record_failure(failure)?;
        let folder = self.folder_of(&failure.commit);
        let _ = std::fs::remove_dir_all(&folder);
        Ok(())
    }

    pub fn pending_restart(&self) -> Option<PendingRestart> {
        read_json(&self.folder.join(PENDING_RESTART))
    }

    /// Records the pending local-build restart guard before swapping executables (#609).
    /// When `previous_commit` is not already a verified kept local build, copies
    /// `current_executable` to `local-builds/rollback.exe` so rollback still works from a
    /// published release.
    pub fn arm_pending_restart(
        &self,
        commit: &str,
        previous_commit: Option<&str>,
        current_executable: Option<&Path>,
    ) -> Result<PendingRestart, LocalBuildError> {
        std::fs::create_dir_all(&self.folder).map_err(io_error("create", &self.folder))?;
        let has_kept_previous = previous_commit.is_some_and(|prev| {
            self.list().iter().any(|build| {
                commits_match(&build.commit, prev) && !commits_match(&build.commit, commit)
            })
        });
        let (rollback_file, rollback_bytes) = if has_kept_previous {
            let _ = std::fs::remove_file(self.folder.join(ROLLBACK_EXECUTABLE));
            (None, 0)
        } else if let Some(executable) = current_executable {
            let dest = self.folder.join(ROLLBACK_EXECUTABLE);
            let next = self.folder.join("rollback.exe.next");
            let bytes = std::fs::copy(executable, &next).map_err(io_error("copy", &next))?;
            std::fs::rename(&next, &dest).map_err(io_error("write", &dest))?;
            (Some(ROLLBACK_EXECUTABLE.to_owned()), bytes)
        } else {
            (None, 0)
        };
        let pending = PendingRestart {
            commit: commit.to_owned(),
            previous_commit: previous_commit.map(str::to_owned),
            rollback_file,
            rollback_bytes,
            started_at: unix_seconds(),
        };
        let path = self.folder.join(PENDING_RESTART);
        let next = self.folder.join("pending-restart.json.next");
        let text = serde_json::to_string_pretty(&pending)
            .map_err(|error| io_error("write", &path)(error.into()))?;
        std::fs::write(&next, text).map_err(io_error("write", &next))?;
        std::fs::rename(&next, &path).map_err(io_error("write", &path))?;
        Ok(pending)
    }

    pub fn clear_pending_restart(&self) {
        for name in [
            PENDING_RESTART,
            "pending-restart.json.next",
            ROLLBACK_EXECUTABLE,
            "rollback.exe.next",
        ] {
            let _ = std::fs::remove_file(self.folder.join(name));
        }
    }

    /// Reads the previous executable to restore when a restarted build fails startup or
    /// heartbeat verification (#609).
    pub fn read_rollback_executable(
        &self,
        pending: &PendingRestart,
    ) -> Result<Vec<u8>, LocalBuildError> {
        if let Some(name) = pending.rollback_file.as_deref() {
            let path = self.folder.join(name);
            let bytes = std::fs::read(&path).map_err(io_error("read", &path))?;
            if bytes.is_empty() || bytes.len() as u64 != pending.rollback_bytes {
                return Err(LocalBuildError::Changed {
                    path: path.display().to_string(),
                });
            }
            return Ok(bytes);
        }
        let builds = self.list();
        if let Some(previous) = pending.previous_commit.as_deref()
            && let Some(build) = builds
                .iter()
                .find(|build| commits_match(&build.commit, previous))
        {
            return self.read_executable(build);
        }
        if let Some(build) = builds
            .iter()
            .find(|build| !commits_match(&build.commit, &pending.commit))
        {
            return self.read_executable(build);
        }
        Err(LocalBuildError::NoExecutable {
            folder: self.folder.display().to_string(),
        })
    }

    pub fn settings(&self) -> LocalBuildSettings {
        read_json(&self.folder.join(SETTINGS)).unwrap_or_default()
    }

    pub fn save_settings(&self, settings: &LocalBuildSettings) -> Result<(), LocalBuildError> {
        std::fs::create_dir_all(&self.folder).map_err(io_error("create", &self.folder))?;
        let path = self.folder.join(SETTINGS);
        let next = self.folder.join("settings.json.next");
        let text = serde_json::to_string_pretty(settings)
            .map_err(|error| io_error("write", &path)(error.into()))?;
        std::fs::write(&next, text).map_err(io_error("write", &next))?;
        std::fs::rename(&next, &path).map_err(io_error("write", &path))
    }

    /// Unfinished folders go too. A folder that can't be removed is tried again after the
    /// next build.
    fn prune(&self) {
        let kept: Vec<String> = self
            .list()
            .into_iter()
            .take(KEPT_BUILDS)
            .map(|build| build.commit)
            .collect();
        for folder in self.folders() {
            let name = folder.file_name().and_then(|name| name.to_str());
            if !name.is_some_and(|name| kept.iter().any(|commit| commit == name)) {
                let _ = std::fs::remove_dir_all(&folder);
            }
        }
    }

    fn folders(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.folder) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .map(|entry| entry.path())
            .collect()
    }
}

fn executable_in(folder: &Path) -> Result<PathBuf, LocalBuildError> {
    let missing = || LocalBuildError::NoExecutable {
        folder: folder.display().to_string(),
    };
    std::fs::read_dir(folder)
        .map_err(|_| missing())?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(EXECUTABLE_SUFFIX))
        })
        .ok_or_else(missing)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Option<T> {
    serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
}

pub(crate) fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// A confirmation prompt shown when installing a local build whose ancestry against the
/// running build cannot be verified (for example when the running build's commit is
/// unknown) (#551).
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct LocalBuildConfirm {
    pub commit: String,
    /// The running build's commit, or `"unknown (<version>)"` when unrecorded.
    pub running_commit: String,
    /// The target build's short commit and version.
    pub target_commit: String,
    /// Explanation naming both commits before asking whether to restart anyway.
    pub message: String,
}

/// Whether an install/restart request for a local build should proceed, ask first, or
/// refuse (#551).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum InstallDecision {
    Allow,
    Confirm(LocalBuildConfirm),
    Refuse(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalBuildPhase {
    Idle,
    /// The commit is green, and its build starts once the machine has memory to spare.
    WaitingForMemory(String),
    Building(String),
    /// Waiting for confirmation because the running build's commit or ancestry is unknown.
    ConfirmRestart(LocalBuildConfirm),
    /// The build's executable is being put in place; Velocity is about to exit.
    Restarting(String),
    RestartFailed(String),
}

/// What Velocity knows about its local builds. `src-tauri` drives the transitions;
/// [`LocalBuildState::view`] turns it into what the title bar shows.
#[derive(Clone, Debug)]
pub struct LocalBuildState {
    /// The running binary's version string, shown if its commit is unknown.
    running_version: String,
    /// The published release the running copy was built on.
    running_base: String,
    /// The commit of the running copy (from build metadata or its local-build version).
    running_commit: Option<String>,
    pub phase: LocalBuildPhase,
    builds: Vec<LocalBuild>,
    /// Verified ancestry of each kept build against `running_commit`.
    relations: BTreeMap<String, CommitRelation>,
    failed: Option<FailedBuild>,
    /// Whether Velocity automatically restarts into the newest local build (#601).
    auto_restart: bool,
    /// Open issues waiting for Velocity to restart into a build with new agent tools (#601).
    waiting_issues: u32,
    /// Tooltip naming the issues waiting for restart (#601).
    waiting_detail: String,
}

impl LocalBuildState {
    pub fn new(
        running_version: &str,
        build_commit: Option<&str>,
        builds: Vec<LocalBuild>,
        failed: Option<FailedBuild>,
    ) -> Self {
        let (running_base, version_commit) = release_base(running_version);
        let running_commit = build_commit.and_then(normalize_commit).or(version_commit);
        Self {
            running_version: running_version.to_owned(),
            running_base,
            running_commit,
            phase: LocalBuildPhase::Idle,
            builds,
            relations: BTreeMap::new(),
            failed,
            auto_restart: default_auto_restart(),
            waiting_issues: 0,
            waiting_detail: String::new(),
        }
    }

    pub fn running_commit(&self) -> Option<&str> {
        self.running_commit.as_deref()
    }

    pub fn auto_restart(&self) -> bool {
        self.auto_restart
    }

    pub fn set_auto_restart(&mut self, enabled: bool) {
        self.auto_restart = enabled;
    }

    pub fn set_waiting_issues(&mut self, count: u32, detail: String) {
        self.waiting_issues = count;
        self.waiting_detail = detail;
    }

    /// The newest kept build on the running release base that strictly descends from the
    /// running copy's commit.
    pub fn newest_descendant_commit(&self) -> Option<&str> {
        self.same_base_builds()
            .next()
            .filter(|build| self.relation_of(&build.commit) == CommitRelation::Descendant)
            .map(|build| build.commit.as_str())
    }

    /// Returns the commit to auto-restart into when auto-restart is enabled, the newest
    /// local build strictly descends from the running copy, nothing is busy or awaiting
    /// confirmation, this commit is not inside its 10-minute failure backoff window (#605),
    /// and either a waiting issue needs a land contained in it or Velocity is idle with no
    /// turn, land, deploy or local build in progress (#601, #609).
    pub fn should_auto_restart(
        &self,
        waiting_need_satisfied: bool,
        idle_ready: bool,
        failed_auto: Option<&FailedAutoRestart>,
        now: Instant,
    ) -> Option<String> {
        if !self.auto_restart
            || self.busy()
            || matches!(self.phase, LocalBuildPhase::ConfirmRestart(_))
            || !(waiting_need_satisfied || idle_ready)
        {
            return None;
        }
        let commit = self.newest_descendant_commit()?;
        if failed_auto.is_some_and(|failed| failed.blocks(commit, now)) {
            return None;
        }
        Some(commit.to_owned())
    }

    pub fn builds(&self) -> &[LocalBuild] {
        &self.builds
    }

    pub fn version_for(&self, commit: &str) -> String {
        local_version(&self.running_base, commit)
    }

    pub fn set_relations(&mut self, relations: BTreeMap<String, CommitRelation>) {
        self.relations = relations;
    }

    /// Updates a single commit's verified relation when a live check yields a definitive
    /// result (never overwriting a verified relation with `Unknown`) (#692).
    pub fn record_relation(&mut self, commit: &str, relation: CommitRelation) {
        if relation != CommitRelation::Unknown {
            self.relations.insert(commit.to_owned(), relation);
        }
    }

    /// The commit to build now, given the project's last green commit and its verified
    /// ancestry against the running build (#551): only a commit that strictly descends
    /// from the running build's commit, is not built yet and whose build hasn't failed.
    /// `None` while a build, restart or confirmation prompt is under way.
    pub fn wanted(&self, green: Option<&str>, relation: CommitRelation) -> Option<String> {
        if relation != CommitRelation::Descendant {
            return None;
        }
        let green = green.filter(|commit| is_commit(commit))?;
        let built = self.same_base_builds().any(|build| build.commit == green);
        let failed = self
            .failed
            .as_ref()
            .is_some_and(|failed| failed.commit == green);
        let confirming = matches!(self.phase, LocalBuildPhase::ConfirmRestart(_));
        (!self.busy() && !confirming && !built && !failed && !self.runs(green))
            .then(|| green.to_owned())
    }

    /// A build or a restart is under way. Neither starts over the other: a restart would
    /// leave the build's compilers running in the folder the next copy builds in.
    pub fn busy(&self) -> bool {
        matches!(
            self.phase,
            LocalBuildPhase::Building(_) | LocalBuildPhase::Restarting(_)
        )
    }

    pub fn built(&mut self, builds: Vec<LocalBuild>, relations: BTreeMap<String, CommitRelation>) {
        self.builds = builds;
        self.relations = relations;
        self.failed = None;
        self.phase = LocalBuildPhase::Idle;
    }

    pub fn build_failed(&mut self, failure: FailedBuild) {
        self.failed = Some(failure);
        self.phase = LocalBuildPhase::Idle;
    }

    /// Any kept build on the running release base matching `commit`.
    pub fn kept_build(&self, commit: &str) -> Option<&LocalBuild> {
        self.same_base_builds().find(|build| build.commit == commit)
    }

    /// Whether `commit` is the kept previous local build offered for explicit rollback
    /// while this copy is running the newest local build.
    pub fn is_previous_offer(&self, commit: &str) -> bool {
        let mut builds = self.same_base_builds();
        let Some(newest) = builds.next() else {
            return false;
        };
        if !self.runs(&newest.commit) {
            return false;
        }
        builds
            .next()
            .is_some_and(|previous| previous.commit == commit && previous.commit != newest.commit)
    }

    /// Decides whether installing `commit` is permitted (#551). Never installs an ancestor
    /// (or the same/diverged commit) on a forward restart. When the running build's commit
    /// or ancestry is unknown, asks first and names both commits.
    pub fn install_decision(
        &self,
        commit: &str,
        relation: CommitRelation,
        confirmed: bool,
        rollback: bool,
    ) -> InstallDecision {
        let Some(build) = self.kept_build(commit) else {
            return InstallDecision::Refuse(format!(
                "Local build {} is no longer available.",
                short_commit(commit)
            ));
        };
        if rollback && self.is_previous_offer(commit) {
            return InstallDecision::Allow;
        }
        let target_short = short_commit(&build.commit);
        let running_label = self.running_commit_label();
        match relation {
            CommitRelation::Descendant => InstallDecision::Allow,
            CommitRelation::Ancestor => InstallDecision::Refuse(format!(
                "Refused to install {target_short}: it is an ancestor of the running build {running_label}."
            )),
            CommitRelation::Same => InstallDecision::Refuse(format!(
                "Refused to install {target_short}: Velocity is already running {running_label}."
            )),
            CommitRelation::Diverged => InstallDecision::Refuse(format!(
                "Refused to install {target_short}: it does not descend from the running build {running_label}."
            )),
            CommitRelation::Unknown if confirmed => InstallDecision::Allow,
            CommitRelation::Unknown => {
                let target_commit = format!("{target_short} ({})", build.version);
                let message = if self.running_commit.is_none() {
                    format!(
                        "The running build's commit is {running_label}, so Velocity cannot verify \
                         that {target_commit} is newer. Installing an older build can downgrade \
                         Velocity. Restart into {target_short} anyway?"
                    )
                } else {
                    format!(
                        "Velocity could not verify whether {target_commit} descends from the \
                         running build {running_label} in Rosen history. Installing an older \
                         build can downgrade Velocity. Restart into {target_short} anyway?"
                    )
                };
                InstallDecision::Confirm(LocalBuildConfirm {
                    commit: build.commit.clone(),
                    running_commit: running_label,
                    target_commit,
                    message,
                })
            }
        }
    }

    fn running_commit_label(&self) -> String {
        match &self.running_commit {
            Some(commit) => short_commit(commit).to_owned(),
            None => format!("unknown ({})", self.running_version),
        }
    }

    pub fn runs(&self, commit: &str) -> bool {
        self.running_commit
            .as_deref()
            .is_some_and(|running| commits_match(commit, running))
    }

    fn relation_of(&self, commit: &str) -> CommitRelation {
        if self.runs(commit) {
            return CommitRelation::Same;
        }
        self.relations
            .get(commit)
            .copied()
            .unwrap_or(CommitRelation::Unknown)
    }

    /// Whether `commit` should be offered as an explicit confirmation-first action (#692).
    /// Shown only when ancestry is unknown—either immediately because the running copy
    /// carries no commit metadata, or once Rosen has checked and returned `Unknown`.
    /// Never shown before an initial ancestry check on a copy with a known commit, and
    /// never shown for verified descendants, ancestors, diverged or same commits.
    fn needs_unverified_offer(&self, commit: &str) -> bool {
        if self.relation_of(commit) != CommitRelation::Unknown {
            return false;
        }
        self.running_commit.is_none()
            || matches!(self.relations.get(commit), Some(CommitRelation::Unknown))
    }

    /// Builds made on the release this copy runs, newest first, skipping any commit marked
    /// failed (#609). After a published update the older builds may hold less than the
    /// running copy, so they are no longer offered.
    fn same_base_builds(&self) -> impl Iterator<Item = &LocalBuild> {
        let failed = self.failed.as_ref().map(|record| record.commit.as_str());
        self.builds.iter().filter(move |build| {
            release_base(&build.version).0 == self.running_base
                && failed.is_none_or(|commit| !commits_match(&build.commit, commit))
        })
    }
}

/// One restart the title bar offers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct LocalBuildOffer {
    pub commit: String,
    pub label: String,
    /// The button's tooltip: which build, when it was made and what restarting does.
    pub title: String,
}

/// The title bar's local-build controls, ready to render. Empty on every copy that has no
/// local build and isn't making one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct LocalBuildView {
    /// "Restart into <commit>": the newest build, only when it strictly descends from the
    /// running build's commit.
    pub restart: Option<LocalBuildOffer>,
    /// "Confirm restart into <commit>": the newest kept build on the running release when
    /// the running build's commit or ancestry could not be verified, opening the explicit
    /// confirmation prompt without presenting it as a verified descendant restart (#692).
    pub unverified_restart: Option<LocalBuildOffer>,
    /// "Restart into previous": the build before the one this copy runs.
    pub previous: Option<LocalBuildOffer>,
    /// Confirmation prompt when installing a build whose ancestry could not be verified.
    pub confirm: Option<LocalBuildConfirm>,
    /// One line on a build or restart under way, or the last failure.
    pub status: String,
    /// The whole reason behind a failure, for the status line's tooltip.
    pub detail: String,
    pub failed: bool,
    /// A build or a restart is under way: the buttons wait for it.
    pub busy: bool,
    /// Open issues waiting for Velocity to restart into a build with new agent tooling (#601).
    pub waiting_issues: u32,
    /// "N issues waiting for restart" shown next to "Restart into <commit>" (#601).
    pub waiting_label: String,
    /// Tooltip describing the issues waiting for restart (#601).
    pub waiting_detail: String,
    /// Whether automatic restart into local builds is enabled (default on) (#601).
    pub auto_restart: bool,
}

impl Default for LocalBuildView {
    fn default() -> Self {
        Self {
            restart: None,
            unverified_restart: None,
            previous: None,
            confirm: None,
            status: String::new(),
            detail: String::new(),
            failed: false,
            busy: false,
            waiting_issues: 0,
            waiting_label: String::new(),
            waiting_detail: String::new(),
            auto_restart: default_auto_restart(),
        }
    }
}

fn waiting_label(count: u32) -> String {
    match count {
        0 => String::new(),
        1 => "1 issue waiting for restart".to_owned(),
        count => format!("{count} issues waiting for restart"),
    }
}

const RESTART_NOTE: &str = "Running turns stop and continue by themselves when Velocity is back.";

impl LocalBuildState {
    pub fn view(&self) -> LocalBuildView {
        let mut builds = self.same_base_builds();
        let newest = builds.next();
        let running_newest = newest.is_some_and(|build| self.runs(&build.commit));
        let restart = newest.and_then(|build| self.restart_offer(build));
        let unverified_restart = newest.and_then(|build| self.unverified_restart_offer(build));
        let previous = builds
            .next()
            .filter(|build| {
                running_newest && newest.is_some_and(|current| current.commit != build.commit)
            })
            .map(previous_offer);
        let confirm = match &self.phase {
            LocalBuildPhase::ConfirmRestart(prompt) => Some(prompt.clone()),
            _ => None,
        };
        let (status, detail, failed) = self.status();
        LocalBuildView {
            restart,
            unverified_restart,
            previous,
            confirm,
            status,
            detail,
            failed,
            busy: self.busy(),
            waiting_issues: self.waiting_issues,
            waiting_label: waiting_label(self.waiting_issues),
            waiting_detail: self.waiting_detail.clone(),
            auto_restart: self.auto_restart,
        }
    }

    fn restart_offer(&self, build: &LocalBuild) -> Option<LocalBuildOffer> {
        (self.relation_of(&build.commit) == CommitRelation::Descendant).then(|| {
            let short = short_commit(&build.commit);
            LocalBuildOffer {
                commit: build.commit.clone(),
                label: format!("Restart into {short}"),
                title: format!(
                    "Restart Velocity into its local build of {short}, built {}. {RESTART_NOTE}",
                    built_clock(build.built_at)
                ),
            }
        })
    }

    fn unverified_restart_offer(&self, build: &LocalBuild) -> Option<LocalBuildOffer> {
        self.needs_unverified_offer(&build.commit).then(|| {
            let short = short_commit(&build.commit);
            LocalBuildOffer {
                commit: build.commit.clone(),
                label: format!("Confirm restart into {short}"),
                title: format!(
                    "Review and confirm restarting Velocity into local build {short} (built {}): \
                     ancestry against {} could not be verified. {RESTART_NOTE}",
                    built_clock(build.built_at),
                    self.running_commit_label()
                ),
            }
        })
    }

    fn status(&self) -> (String, String, bool) {
        let plain = |text: String| (text, String::new(), false);
        match (&self.phase, &self.failed) {
            (LocalBuildPhase::WaitingForMemory(commit), _) => plain(format!(
                "Waiting for memory to build {}",
                short_commit(commit)
            )),
            (LocalBuildPhase::Building(commit), _) => {
                plain(format!("Building {}…", short_commit(commit)))
            }
            (LocalBuildPhase::ConfirmRestart(_), _) => plain(String::new()),
            (LocalBuildPhase::Restarting(commit), _) => {
                plain(format!("Restarting into {}…", short_commit(commit)))
            }
            (LocalBuildPhase::RestartFailed(message), _) => {
                ("Restart failed".to_owned(), message.clone(), true)
            }
            (LocalBuildPhase::Idle, Some(failed)) => (
                format!("Build of {} failed", short_commit(&failed.commit)),
                failed.reason.clone(),
                true,
            ),
            (LocalBuildPhase::Idle, None) => plain(String::new()),
        }
    }
}

fn previous_offer(build: &LocalBuild) -> LocalBuildOffer {
    let short = short_commit(&build.commit);
    LocalBuildOffer {
        commit: build.commit.clone(),
        label: "Restart into previous".to_owned(),
        title: format!(
            "Go back to the local build of {short}, built {}. {RESTART_NOTE}",
            built_clock(build.built_at)
        ),
    }
}

/// "19 Nov 14:05" in local time.
fn built_clock(built_at: u64) -> String {
    i64::try_from(built_at)
        .ok()
        .and_then(|seconds| chrono::DateTime::from_timestamp(seconds, 0))
        .map(|time| {
            time.with_timezone(&chrono::Local)
                .format("%d %b %H:%M")
                .to_string()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    // Risk: the restart and the rollback read what an earlier run of Velocity wrote. A
    // record that didn't survive, or pruning that removed the wrong folder, would leave
    // the title bar offering a build that is gone, or no way back.
    #[test]
    fn finished_builds_survive_a_reload_and_only_the_newest_two_are_kept()
    -> Result<(), Box<dyn Error>> {
        let scratch =
            std::env::temp_dir().join(format!("velocity-local-builds-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        let builds = LocalBuilds::new(&scratch);
        let commits = ["a".repeat(40), "b".repeat(40), "c".repeat(40)];
        for (index, commit) in commits.iter().enumerate() {
            let version = local_version("0.1.0-beta.13", commit);
            let folder = builds.prepare(commit)?;
            let executable = folder.join(format!("Velocity_{version}_x64-portable.exe"));
            std::fs::write(&executable, vec![7; index + 1])?;
            let mut build = builds.finish(commit, &version)?;
            // Finish times a second apart, which three builds in one test never are.
            build.built_at = 1_000 + index as u64;
            std::fs::write(folder.join(RECORD), serde_json::to_string(&build)?)?;
        }
        builds.prune();

        let reloaded = LocalBuilds::new(&scratch).list();
        let kept: Vec<&str> = reloaded.iter().map(|build| build.commit.as_str()).collect();
        let newest = reloaded.first().ok_or("no build survived")?;
        let bytes = builds.read_executable(newest)?;
        let oldest_left = scratch.join(FOLDER).join(&commits[0]).exists();
        std::fs::remove_dir_all(&scratch)?;

        assert_eq!(kept, [commits[2].as_str(), commits[1].as_str()]);
        assert_eq!(newest.version, "0.1.0-beta.13.local.gccccccc");
        assert_eq!(bytes, vec![7; 3]);
        assert!(!oldest_left, "the build before the last two was kept");
        Ok(())
    }

    // Risk (#551): a stale local Rosen server or a running release without an embedded
    // commit could cause Velocity to build, offer or install an ancestor commit and
    // downgrade itself over a newer build.
    #[test]
    fn local_builds_never_build_offer_or_install_ancestors_and_ask_when_running_commit_unknown() {
        let running = "954c50a".to_owned() + &"0".repeat(33);
        let older = "4a126a1".to_owned() + &"0".repeat(33);
        let newer = "def5678".to_owned() + &"0".repeat(33);
        let older_build = LocalBuild {
            commit: older.clone(),
            version: local_version("0.1.0-beta.19", &older),
            built_at: 100,
            executable_bytes: 12,
        };
        let mut state = LocalBuildState::new(
            "0.1.0-beta.19",
            Some(&running),
            vec![older_build.clone()],
            None,
        );
        assert_eq!(state.running_commit(), Some(running.as_str()));
        assert_eq!(state.wanted(Some(&older), CommitRelation::Ancestor), None);
        assert_eq!(state.wanted(Some(&running), CommitRelation::Same), None);
        assert_eq!(state.wanted(Some(&newer), CommitRelation::Unknown), None);
        assert_eq!(state.wanted(Some(&newer), CommitRelation::Diverged), None);
        assert_eq!(
            state.wanted(Some(&newer), CommitRelation::Descendant),
            Some(newer.clone())
        );

        // Before an initial ancestry check on a copy with a known commit, neither offer shows.
        assert!(state.view().restart.is_none());
        assert!(state.view().unverified_restart.is_none());

        state.set_relations(BTreeMap::from([(older.clone(), CommitRelation::Ancestor)]));
        assert!(state.view().restart.is_none());
        assert!(state.view().unverified_restart.is_none());
        assert!(matches!(
            state.install_decision(&older, CommitRelation::Ancestor, true, false),
            InstallDecision::Refuse(message)
                if message.contains("4a126a1") && message.contains("954c50a")
        ));

        // When Rosen explicitly reports Unknown for a known running commit, only the
        // confirmation-first offer appears (never an ordinary restart or auto-restart).
        state.set_relations(BTreeMap::from([(older.clone(), CommitRelation::Unknown)]));
        assert!(state.view().restart.is_none());
        assert!(state.newest_descendant_commit().is_none());
        assert!(matches!(
            state.view().unverified_restart,
            Some(ref offer)
                if offer.commit == older
                    && offer.label == "Confirm restart into 4a126a1"
                    && offer.title.contains("954c50a")
        ));

        // Risk (#692): a legacy running release without commit metadata still needs a
        // title-bar path to request the confirmation dialog naming both commits, while
        // preserving strict descendant gating on automatic builds, auto-restarts and
        // ordinary verified restart offers.
        let unknown = LocalBuildState::new("0.1.0-beta.19", None, vec![older_build], None);
        assert_eq!(unknown.running_commit(), None);
        assert_eq!(unknown.wanted(Some(&newer), CommitRelation::Unknown), None);
        assert_eq!(unknown.newest_descendant_commit(), None);
        assert_eq!(
            unknown.should_auto_restart(true, true, None, Instant::now()),
            None
        );
        let unknown_view = unknown.view();
        assert!(unknown_view.restart.is_none());
        assert!(matches!(
            unknown_view.unverified_restart,
            Some(ref offer)
                if offer.commit == older
                    && offer.label == "Confirm restart into 4a126a1"
                    && offer.title.contains("unknown (0.1.0-beta.19)")
        ));
        let Some(unverified) = unknown_view.unverified_restart else {
            panic!("legacy running release must expose confirmation-first local build offer");
        };
        assert!(matches!(
            unknown.install_decision(&unverified.commit, CommitRelation::Unknown, false, false),
            InstallDecision::Confirm(prompt)
                if prompt.running_commit.contains("unknown")
                    && prompt.running_commit.contains("0.1.0-beta.19")
                    && prompt.target_commit.contains("4a126a1")
                    && prompt.message.contains("unknown")
                    && prompt.message.contains("4a126a1")
        ));
        assert_eq!(
            unknown.install_decision(&unverified.commit, CommitRelation::Unknown, true, false),
            InstallDecision::Allow
        );
    }

    // Risk (#601): auto-restart preference must default on and survive save/reload, and
    // auto-restart must fire only for verified descendant builds when enabled and either a
    // waiting issue needs it or agents have been idle, never repeating a failed commit.
    #[test]
    fn auto_restart_settings_survive_reload_and_gate_descendant_restarts()
    -> Result<(), Box<dyn Error>> {
        let scratch = std::env::temp_dir().join(format!(
            "velocity-local-build-settings-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&scratch);
        let builds = LocalBuilds::new(&scratch);
        assert!(builds.settings().auto_restart);
        builds.save_settings(&LocalBuildSettings {
            auto_restart: false,
        })?;
        assert!(!LocalBuilds::new(&scratch).settings().auto_restart);
        builds.save_settings(&LocalBuildSettings { auto_restart: true })?;
        assert!(LocalBuilds::new(&scratch).settings().auto_restart);
        let _ = std::fs::remove_dir_all(&scratch);

        let running = "1111111".to_owned() + &"0".repeat(33);
        let newer = "a795720".to_owned() + &"0".repeat(33);
        let newest_build = LocalBuild {
            commit: newer.clone(),
            version: local_version("0.1.0-beta.20", &newer),
            built_at: 200,
            executable_bytes: 16,
        };
        let mut state =
            LocalBuildState::new("0.1.0-beta.20", Some(&running), vec![newest_build], None);
        state.set_relations(BTreeMap::from([(
            newer.clone(),
            CommitRelation::Descendant,
        )]));
        state.set_waiting_issues(2, "#589, #590 waiting for restart".into());
        let view = state.view();
        assert_eq!(view.waiting_issues, 2);
        assert_eq!(view.waiting_label, "2 issues waiting for restart");
        assert!(view.auto_restart);
        let now = Instant::now();
        assert_eq!(state.should_auto_restart(false, false, None, now), None);
        assert_eq!(
            state.should_auto_restart(true, false, None, now),
            Some(newer.clone())
        );
        assert_eq!(
            state.should_auto_restart(false, true, None, now),
            Some(newer.clone())
        );
        let failed = FailedAutoRestart::new(newer.clone(), "MoveAside".into(), now);
        assert_eq!(
            state.should_auto_restart(true, true, Some(&failed), now),
            None
        );
        assert_eq!(
            state.should_auto_restart(
                true,
                true,
                Some(&failed),
                now + AUTO_RESTART_RETRY_DELAY - Duration::from_secs(1),
            ),
            None
        );
        assert_eq!(
            state.should_auto_restart(true, true, Some(&failed), now + AUTO_RESTART_RETRY_DELAY),
            Some(newer.clone())
        );
        // A newer local build retries immediately without waiting out the old commit's backoff.
        let even_newer = "b896831".to_owned() + &"0".repeat(33);
        let even_newer_build = LocalBuild {
            commit: even_newer.clone(),
            version: local_version("0.1.0-beta.20", &even_newer),
            built_at: 300,
            executable_bytes: 16,
        };
        state.built(
            vec![even_newer_build],
            BTreeMap::from([(even_newer.clone(), CommitRelation::Descendant)]),
        );
        assert_eq!(
            state.should_auto_restart(true, true, Some(&failed), now),
            Some(even_newer)
        );
        state.set_auto_restart(false);
        assert_eq!(state.should_auto_restart(true, true, None, now), None);
        Ok(())
    }

    // Risk (#609): the post-restart rollback guard must survive save/reload, resolve the
    // previous executable both from a kept local build and from a fallback copy of a
    // published release, and marking a rolled-back commit as failed must prevent Velocity
    // from offering, rebuilding, or auto-restarting into that broken commit again.
    #[test]
    fn pending_restart_persists_and_marked_failed_commit_is_not_restarted_into()
    -> Result<(), Box<dyn Error>> {
        assert!(IdleBlockers::default().is_idle());
        assert!(
            !IdleBlockers {
                turn_in_progress: true,
                ..IdleBlockers::default()
            }
            .is_idle()
        );
        assert!(
            !IdleBlockers {
                land_in_progress: true,
                ..IdleBlockers::default()
            }
            .is_idle()
        );
        assert!(
            !IdleBlockers {
                deploy_in_progress: true,
                ..IdleBlockers::default()
            }
            .is_idle()
        );

        let scratch = std::env::temp_dir().join(format!(
            "velocity-local-build-pending-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch)?;
        let builds = LocalBuilds::new(&scratch);

        // Case 1: previous copy is a published release (not in local-builds/), so arming
        // copies the running executable into rollback.exe.
        let running_exe = scratch.join("velocity-app.exe");
        std::fs::write(&running_exe, b"published-beta-22")?;
        let bad_commit = "786d72e".to_owned() + &"1".repeat(33);
        let bad_version = local_version("0.1.0-beta.22", &bad_commit);
        let bad_folder = builds.prepare(&bad_commit)?;
        std::fs::write(
            bad_folder.join(format!("Velocity_{bad_version}_x64-portable.exe")),
            b"bad-786d72e",
        )?;
        let _ = builds.finish(&bad_commit, &bad_version)?;

        let armed = builds.arm_pending_restart(&bad_commit, Some("b9169e5"), Some(&running_exe))?;
        let reloaded = LocalBuilds::new(&scratch)
            .pending_restart()
            .ok_or("pending restart did not survive reload")?;
        assert_eq!(reloaded, armed);
        assert!(reloaded.matches_running(Some("786d72e")));
        assert!(!reloaded.matches_running(Some("b9169e5")));
        assert_eq!(
            builds.read_rollback_executable(&reloaded)?,
            b"published-beta-22"
        );

        // Marking the rolled-back commit failed removes its folder, writes failed.json, and
        // blocks rebuild / offer / auto-restart.
        let failure = FailedBuild {
            commit: bad_commit.clone(),
            reason: "Restarted build missed heartbeats".into(),
        };
        builds.mark_build_failed(&failure)?;
        builds.clear_pending_restart();
        assert!(builds.pending_restart().is_none());
        assert!(!scratch.join(FOLDER).join(ROLLBACK_EXECUTABLE).exists());
        assert_eq!(builds.failure(), Some(failure.clone()));
        assert!(builds.list().is_empty());

        let mut state = LocalBuildState::new(
            "0.1.0-beta.22",
            Some("b9169e5"),
            vec![LocalBuild {
                commit: bad_commit.clone(),
                version: bad_version,
                built_at: 100,
                executable_bytes: 11,
            }],
            Some(failure),
        );
        state.set_relations(BTreeMap::from([(
            bad_commit.clone(),
            CommitRelation::Descendant,
        )]));
        assert_eq!(state.newest_descendant_commit(), None);
        assert_eq!(
            state.should_auto_restart(true, true, None, Instant::now()),
            None
        );
        assert_eq!(
            state.wanted(Some(&bad_commit), CommitRelation::Descendant),
            None
        );
        assert!(state.view().failed);
        assert!(state.view().status.contains("786d72e"));

        // Case 2: previous copy IS a kept local build; arming uses that build directly.
        let prev_commit = "b9169e5".to_owned() + &"2".repeat(33);
        let prev_version = local_version("0.1.0-beta.22", &prev_commit);
        let prev_folder = builds.prepare(&prev_commit)?;
        std::fs::write(
            prev_folder.join(format!("Velocity_{prev_version}_x64-portable.exe")),
            b"kept-prev-b9169e5",
        )?;
        let _ = builds.finish(&prev_commit, &prev_version)?;
        let next_commit = "999aaaa".to_owned() + &"3".repeat(33);
        let armed_kept =
            builds.arm_pending_restart(&next_commit, Some(&prev_commit), Some(&running_exe))?;
        assert!(armed_kept.rollback_file.is_none());
        assert_eq!(
            builds.read_rollback_executable(&armed_kept)?,
            b"kept-prev-b9169e5"
        );

        let _ = std::fs::remove_dir_all(&scratch);
        Ok(())
    }
}
