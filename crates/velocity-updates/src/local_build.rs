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

/// Saved local-build preferences (`local-builds/settings.json`) (#601).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalBuildSettings {
    /// Restart into the newest local build automatically when waiting issues need its
    /// agent tooling, or when no agent turn has been running for 2 minutes.
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
        std::fs::create_dir_all(&self.folder).map_err(io_error("create", &self.folder))?;
        let text = serde_json::to_string_pretty(failure)
            .map_err(|error| io_error("write", &path)(error.into()))?;
        std::fs::write(&path, text).map_err(io_error("write", &path))
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

fn unix_seconds() -> u64 {
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
    /// and either a waiting issue needs a land contained in it or no agent turn has been
    /// running for 2 minutes (#601).
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

    fn runs(&self, commit: &str) -> bool {
        self.running_commit.as_deref().is_some_and(|running| {
            commit.eq_ignore_ascii_case(running) || commit.starts_with(running)
        })
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

    /// Builds made on the release this copy runs, newest first. After a published update
    /// the older builds may hold less than the running copy, so they are no longer offered.
    fn same_base_builds(&self) -> impl Iterator<Item = &LocalBuild> {
        self.builds
            .iter()
            .filter(|build| release_base(&build.version).0 == self.running_base)
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
        let restart = newest
            .filter(|build| self.relation_of(&build.commit) == CommitRelation::Descendant)
            .map(|build| {
                let short = short_commit(&build.commit);
                LocalBuildOffer {
                    commit: build.commit.clone(),
                    label: format!("Restart into {short}"),
                    title: format!(
                        "Restart Velocity into its local build of {short}, built {}. {RESTART_NOTE}",
                        built_clock(build.built_at)
                    ),
                }
            });
        let previous = builds
            .next()
            .filter(|build| {
                running_newest && newest.is_some_and(|current| current.commit != build.commit)
            })
            .map(|build| {
                let short = short_commit(&build.commit);
                LocalBuildOffer {
                    commit: build.commit.clone(),
                    label: "Restart into previous".to_owned(),
                    title: format!(
                        "Go back to the local build of {short}, built {}. {RESTART_NOTE}",
                        built_clock(build.built_at)
                    ),
                }
            });
        let confirm = match &self.phase {
            LocalBuildPhase::ConfirmRestart(prompt) => Some(prompt.clone()),
            _ => None,
        };
        let (status, detail, failed) = self.status();
        LocalBuildView {
            restart,
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

        state.set_relations(BTreeMap::from([(older.clone(), CommitRelation::Ancestor)]));
        assert!(state.view().restart.is_none());
        assert!(matches!(
            state.install_decision(&older, CommitRelation::Ancestor, true, false),
            InstallDecision::Refuse(message)
                if message.contains("4a126a1") && message.contains("954c50a")
        ));

        let unknown = LocalBuildState::new("0.1.0-beta.19", None, vec![older_build], None);
        assert_eq!(unknown.running_commit(), None);
        assert!(unknown.view().restart.is_none());
        assert!(matches!(
            unknown.install_decision(&older, CommitRelation::Unknown, false, false),
            InstallDecision::Confirm(prompt)
                if prompt.running_commit.contains("unknown")
                    && prompt.running_commit.contains("0.1.0-beta.19")
                    && prompt.target_commit.contains("4a126a1")
                    && prompt.message.contains("unknown")
                    && prompt.message.contains("4a126a1")
        ));
        assert_eq!(
            unknown.install_decision(&older, CommitRelation::Unknown, true, false),
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
}
