//! Local builds of Velocity's own landed commits (SPEC.md F1 "Local builds", #512): the
//! version a build gets, the builds kept on disk for a restart or a rollback, and what the
//! title bar offers. `src-tauri` runs the build and the restart; nothing here is published,
//! and no build made this way is signed with the release key.
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const FOLDER: &str = "local-builds";
const RECORD: &str = "build.json";
const FAILURE: &str = "failed.json";
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

/// The published release `version` was built on, and the short commit when `version` is a
/// local build's. `0.1.0-beta.13.local.gabc1234` was built on `0.1.0-beta.13`, and
/// `0.2.1-local.gabc1234` on `0.2.0`.
pub fn release_base(version: &str) -> (String, Option<String>) {
    let version = version.split('+').next().unwrap_or(version);
    let published = || (version.to_owned(), None);
    let Some((core, prerelease)) = version.split_once('-') else {
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
    (base, Some(commit.to_owned()))
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LocalBuildPhase {
    Idle,
    /// The commit is green, and its build starts once the machine has memory to spare.
    WaitingForMemory(String),
    Building(String),
    /// The build's executable is being put in place; Velocity is about to exit.
    Restarting(String),
    RestartFailed(String),
}

/// What Velocity knows about its local builds. `src-tauri` drives the transitions;
/// [`LocalBuildState::view`] turns it into what the title bar shows.
#[derive(Clone, Debug)]
pub struct LocalBuildState {
    /// The published release the running copy was built on.
    running_base: String,
    /// The short commit of the running copy, when it is a local build.
    running_commit: Option<String>,
    pub phase: LocalBuildPhase,
    builds: Vec<LocalBuild>,
    failed: Option<FailedBuild>,
}

impl LocalBuildState {
    pub fn new(
        running_version: &str,
        builds: Vec<LocalBuild>,
        failed: Option<FailedBuild>,
    ) -> Self {
        let (running_base, running_commit) = release_base(running_version);
        Self {
            running_base,
            running_commit,
            phase: LocalBuildPhase::Idle,
            builds,
            failed,
        }
    }

    pub fn version_for(&self, commit: &str) -> String {
        local_version(&self.running_base, commit)
    }

    /// The commit to build now, given the project's last green commit: one this copy isn't
    /// running, that isn't built yet and whose build hasn't failed. `None` while a build or
    /// a restart is under way.
    pub fn wanted(&self, green: Option<&str>) -> Option<String> {
        let green = green.filter(|commit| is_commit(commit))?;
        let built = self.offered().any(|build| build.commit == green);
        let failed = self
            .failed
            .as_ref()
            .is_some_and(|failed| failed.commit == green);
        (!self.busy() && !built && !failed && !self.runs(green)).then(|| green.to_owned())
    }

    /// A build or a restart is under way. Neither starts over the other: a restart would
    /// leave the build's compilers running in the folder the next copy builds in.
    pub fn busy(&self) -> bool {
        matches!(
            self.phase,
            LocalBuildPhase::Building(_) | LocalBuildPhase::Restarting(_)
        )
    }

    pub fn built(&mut self, builds: Vec<LocalBuild>) {
        self.builds = builds;
        self.failed = None;
        self.phase = LocalBuildPhase::Idle;
    }

    pub fn build_failed(&mut self, failure: FailedBuild) {
        self.failed = Some(failure);
        self.phase = LocalBuildPhase::Idle;
    }

    /// A build the title bar offers, by its commit.
    pub fn offer(&self, commit: &str) -> Option<&LocalBuild> {
        self.offered().find(|build| build.commit == commit)
    }

    fn runs(&self, commit: &str) -> bool {
        self.running_commit
            .as_deref()
            .is_some_and(|running| commit.starts_with(running))
    }

    /// Builds made on the release this copy runs, newest first. After a published update
    /// the older builds may hold less than the running copy, so they are no longer offered.
    fn offered(&self) -> impl Iterator<Item = &LocalBuild> {
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
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct LocalBuildView {
    /// "Restart into <commit>": the newest build, unless this copy already runs it.
    pub restart: Option<LocalBuildOffer>,
    /// "Restart into previous": the build before the one this copy runs.
    pub previous: Option<LocalBuildOffer>,
    /// One line on a build or restart under way, or the last failure.
    pub status: String,
    /// The whole reason behind a failure, for the status line's tooltip.
    pub detail: String,
    pub failed: bool,
    /// A build or a restart is under way: the buttons wait for it.
    pub busy: bool,
}

const RESTART_NOTE: &str = "Running turns stop and continue by themselves when Velocity is back.";

impl LocalBuildState {
    pub fn view(&self) -> LocalBuildView {
        let mut offered = self.offered();
        let newest = offered.next();
        let running_newest = newest.is_some_and(|build| self.runs(&build.commit));
        let restart = newest.filter(|_| !running_newest).map(|build| {
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
        let previous = offered.next().filter(|_| running_newest).map(|build| {
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
        let (status, detail, failed) = self.status();
        LocalBuildView {
            restart,
            previous,
            status,
            detail,
            failed,
            busy: self.busy(),
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
}
