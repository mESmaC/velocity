use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::process_tree::{any_pid_alive, is_previous_sidecar_name, stop_processes_using_image};

#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("Could not write the new version next to {path}: {source}")]
    Stage {
        path: String,
        source: std::io::Error,
    },
    #[error("Could not move the running copy aside ({path}): {source}")]
    MoveAside {
        path: String,
        source: std::io::Error,
    },
    #[error("Could not put the new version in place ({path}); the old copy was restored: {source}")]
    Replace {
        path: String,
        source: std::io::Error,
    },
}

/// `Velocity.exe` → `Velocity.exe.<suffix>`, in the same folder so the renames below never
/// cross a volume.
fn sidecar(executable: &Path, suffix: &str) -> PathBuf {
    let mut name = executable
        .file_name()
        .map(OsString::from)
        .unwrap_or_default();
    name.push(".");
    name.push(suffix);
    executable.with_file_name(name)
}

fn unix_millis() -> u64 {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    u64::try_from(millis).unwrap_or(u64::MAX)
}

/// Picks a fresh `<name>.<prefix>-<unix-ms>` path that does not collide with any locked
/// leftover from an earlier restart (#605).
fn unique_sidecar(executable: &Path, prefix: &str) -> PathBuf {
    let mut stamp = unix_millis();
    loop {
        let candidate = sidecar(executable, &format!("{prefix}-{stamp}"));
        if !candidate.exists() {
            return candidate;
        }
        stamp = stamp.saturating_add(1);
    }
}

/// Replaces the running portable executable with `bytes`, which the caller has already
/// verified against the release signature. Windows lets a running executable be renamed
/// but not overwritten, so the running copy moves to a unique `<name>.old-<unix-ms>` and
/// the new one takes its name (#605). A failed swap puts the running copy back, so the
/// file you launch is never missing. Stale `.old*` copies are removed by
/// [`remove_previous`] on the next start.
pub fn replace_running(executable: &Path, bytes: &[u8]) -> Result<PathBuf, UpdateError> {
    let staged = unique_sidecar(executable, "new");
    let previous = unique_sidecar(executable, "old");
    write_staged(&staged, bytes).map_err(|source| {
        let _ = std::fs::remove_file(&staged);
        UpdateError::Stage {
            path: staged.display().to_string(),
            source,
        }
    })?;
    if let Err(source) = std::fs::rename(executable, &previous) {
        let _ = std::fs::remove_file(&staged);
        return Err(UpdateError::MoveAside {
            path: executable.display().to_string(),
            source,
        });
    }
    if let Err(source) = std::fs::rename(&staged, executable) {
        let _ = std::fs::rename(&previous, executable);
        let _ = std::fs::remove_file(&staged);
        return Err(UpdateError::Replace {
            path: executable.display().to_string(),
            source,
        });
    }
    Ok(previous)
}

/// Restores `previous` back to `executable` when relaunching the newly swapped executable
/// fails, so the working copy is back in place (#605).
pub fn restore_previous(executable: &Path, previous: &Path) -> std::io::Result<()> {
    if executable.exists() {
        let discarded = unique_sidecar(executable, "failed");
        if std::fs::remove_file(executable).is_err() {
            let _ = std::fs::rename(executable, &discarded);
        }
    }
    std::fs::rename(previous, executable)
}

fn write_staged(staged: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::File::create(staged)?;
    file.write_all(bytes)?;
    // The swap must never put a partly written executable in place.
    file.sync_all()
}

/// Deletes every `.old*` copy earlier portable updates moved aside that is no longer
/// locked (#605). Still-locked files are left for the next start. `true` when at least
/// one file was removed.
pub fn remove_previous(executable: &Path) -> bool {
    let (Some(folder), Some(exe_name)) = (
        executable.parent(),
        executable.file_name().and_then(|name| name.to_str()),
    ) else {
        return false;
    };
    let Ok(entries) = std::fs::read_dir(folder) else {
        return false;
    };
    let mut removed = false;
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| is_previous_sidecar_name(exe_name, name))
            && std::fs::remove_file(&path).is_ok()
        {
            removed = true;
        }
    }
    removed
}

/// Set on the copy a portable update starts, so it waits for the copy it replaced.
pub const REPLACED_MARKER: &str = "VELOCITY_REPLACED_PREVIOUS";
const REPLACED_PREVIOUS_PATH: &str = "VELOCITY_REPLACED_PREVIOUS_PATH";
const REPLACED_PREVIOUS_PID: &str = "VELOCITY_REPLACED_PREVIOUS_PID";
const REPLACED_LAUNCHER_PID: &str = "VELOCITY_REPLACED_LAUNCHER_PID";
const RELAUNCH_ALIVE_CHECK: Duration = Duration::from_millis(250);
const FORCE_STOP_AFTER: Duration = Duration::from_secs(2);

/// Starts `executable` with this process's arguments, marked so it waits for this copy to
/// exit before opening its window ([`wait_for_previous`]).
pub fn relaunch(executable: &Path) -> std::io::Result<()> {
    spawn_relaunch(executable, None).map(drop)
}

/// Starts the replaced executable, passing the exact moved-aside `previous` path so the
/// new copy waits for this copy rather than a stale `.old`, and confirms the spawned
/// launcher does not exit immediately before this copy closes (#605).
pub fn relaunch_replaced(executable: &Path, previous: &Path) -> std::io::Result<()> {
    let mut child = spawn_relaunch(executable, Some(previous))?;
    std::thread::sleep(RELAUNCH_ALIVE_CHECK);
    if let Some(status) = child.try_wait()? {
        return Err(std::io::Error::other(format!(
            "Replacement exited immediately before startup completed ({status})"
        )));
    }
    Ok(())
}

fn spawn_relaunch(executable: &Path, previous: Option<&Path>) -> std::io::Result<Child> {
    let mut command = hidden_command(executable);
    command
        .args(super::supervision::application_arguments())
        .env(REPLACED_MARKER, "1")
        .env(REPLACED_PREVIOUS_PID, std::process::id().to_string());
    if let Some(previous) = previous {
        command.env(REPLACED_PREVIOUS_PATH, previous);
    } else {
        command.env_remove(REPLACED_PREVIOUS_PATH);
    }
    if let Some(launcher) = super::supervision::launcher_pid() {
        command.env(REPLACED_LAUNCHER_PID, launcher.to_string());
    } else {
        command.env_remove(REPLACED_LAUNCHER_PID);
    }
    command.spawn()
}

pub(crate) fn clear_replaced_env(command: &mut std::process::Command) {
    command
        .env_remove(REPLACED_MARKER)
        .env_remove(REPLACED_PREVIOUS_PATH)
        .env_remove(REPLACED_PREVIOUS_PID)
        .env_remove(REPLACED_LAUNCHER_PID);
}

/// A `Command` that opens no console window (#232); clippy's `disallowed_methods` rejects a
/// bare `Command::new`. Velocity itself is a windowed app, so its own window is unaffected.
#[allow(clippy::disallowed_methods)]
pub(crate) fn hidden_command(program: impl AsRef<std::ffi::OsStr>) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command
}

/// Waits, up to `limit`, for the replaced copy (and its moved-aside `<name>.old-<unix-ms>`
/// image) to exit before opening the window (#605). Starting the window before then races
/// the old copy's single-instance lock and WebView2 shutting down over the same profile,
/// causing the new copy to exit without showing anything.
pub fn wait_for_previous(executable: &Path, limit: Duration) {
    let previous = std::env::var_os(REPLACED_PREVIOUS_PATH)
        .filter(|path| !path.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| sidecar(executable, "old"));
    let pids = previous_pids();
    let started = Instant::now();
    let mut stopped_lingering = false;
    loop {
        let processes_gone = !any_pid_alive(&pids);
        if processes_gone {
            match std::fs::remove_file(&previous) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    if !stopped_lingering && started.elapsed() >= FORCE_STOP_AFTER {
                        stopped_lingering = true;
                        let _ = stop_processes_using_image(&previous, Duration::from_secs(2));
                    }
                }
                _ => break,
            }
        }
        if started.elapsed() >= limit {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // The old copy's WebView2 browser processes close just after it does.
    std::thread::sleep(Duration::from_millis(500));
}

fn previous_pids() -> Vec<u32> {
    let mut pids = Vec::new();
    for name in [REPLACED_PREVIOUS_PID, REPLACED_LAUNCHER_PID] {
        if let Some(pid) = std::env::var(name)
            .ok()
            .and_then(|value| value.trim().parse::<u32>().ok())
            .filter(|pid| *pid > 0 && *pid != std::process::id())
            && !pids.contains(&pid)
        {
            pids.push(pid);
        }
    }
    pids
}

#[cfg(test)]
mod tests {
    use std::error::Error;

    use super::*;

    #[cfg(windows)]
    fn lock_file_exclusively(path: &Path) -> std::io::Result<std::fs::File> {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .share_mode(0)
            .open(path)
    }

    // Risk (#605): an orphaned child holding a stale `velocity-app.exe.old` image open
    // previously caused `replace_running` to fail with `MoveAside` on every restart. Moving
    // aside to a unique `<name>.old-<unix-ms>` must succeed despite a locked `.old`, and
    // `remove_previous` must clean up every unlocked `.old*` copy while leaving locked ones.
    #[test]
    fn replace_running_succeeds_with_locked_old_and_cleans_unlocked_old_files()
    -> Result<(), Box<dyn Error>> {
        let scratch = std::env::temp_dir().join(format!(
            "velocity-portable-locked-old-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch)?;

        let executable = scratch.join("velocity-app.exe");
        let stale_old = scratch.join("velocity-app.exe.old");
        let unlocked_old = scratch.join("velocity-app.exe.old-1000");
        std::fs::write(&executable, b"running-3572913")?;
        std::fs::write(&stale_old, b"beta-22-locked")?;
        std::fs::write(&unlocked_old, b"older-unlocked")?;

        #[cfg(windows)]
        let held_lock = {
            let lock = lock_file_exclusively(&stale_old)?;
            assert!(
                std::fs::remove_file(&stale_old).is_err(),
                "stale .old should be locked against deletion on Windows"
            );
            lock
        };

        let moved_aside = replace_running(&executable, b"new-8e4f83a")?;
        assert_eq!(std::fs::read(&executable)?, b"new-8e4f83a");
        assert_eq!(std::fs::read(&moved_aside)?, b"running-3572913");
        assert_ne!(moved_aside, stale_old);
        assert!(
            moved_aside
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("velocity-app.exe.old-")),
            "moved-aside copy should use unique .old-<unix-ms> name: {}",
            moved_aside.display()
        );

        assert!(remove_previous(&executable));
        assert!(!unlocked_old.exists(), "unlocked .old-* should be removed");
        assert!(
            !moved_aside.exists(),
            "unlocked new .old-* should be removed"
        );

        #[cfg(windows)]
        {
            assert!(
                stale_old.exists(),
                "locked .old should stay until its holder exits"
            );
            drop(held_lock);
            assert!(remove_previous(&executable));
            assert!(
                !stale_old.exists(),
                "unlocked .old should be removed on the next cleanup"
            );
        }

        let _ = std::fs::remove_dir_all(&scratch);
        Ok(())
    }

    // Risk (#605): if relaunching the newly swapped executable fails, restoring the moved-
    // aside binary must put the existing executable back so Velocity can always start again.
    #[test]
    fn restore_previous_puts_existing_executable_back() -> Result<(), Box<dyn Error>> {
        let scratch =
            std::env::temp_dir().join(format!("velocity-portable-restore-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&scratch);
        std::fs::create_dir_all(&scratch)?;

        let executable = scratch.join("velocity-app.exe");
        std::fs::write(&executable, b"existing-good")?;
        let previous = replace_running(&executable, b"broken-new")?;
        assert_eq!(std::fs::read(&executable)?, b"broken-new");

        restore_previous(&executable, &previous)?;
        assert_eq!(std::fs::read(&executable)?, b"existing-good");
        assert!(!previous.exists());

        let _ = std::fs::remove_dir_all(&scratch);
        Ok(())
    }
}
