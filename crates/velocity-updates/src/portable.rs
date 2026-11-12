use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

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

/// Replaces the running portable executable with `bytes`, which the caller has already
/// verified against the release signature. Windows lets a running executable be renamed
/// but not overwritten, so the running copy moves to `<name>.old` and the new one takes
/// its name. A failed swap puts the running copy back, so the file you launch is never
/// missing. The `.old` copy is removed by [`remove_previous`] on the next start.
pub fn replace_running(executable: &Path, bytes: &[u8]) -> Result<(), UpdateError> {
    let staged = sidecar(executable, "new");
    let previous = sidecar(executable, "old");
    write_staged(&staged, bytes).map_err(|source| UpdateError::Stage {
        path: staged.display().to_string(),
        source,
    })?;
    // A leftover from an earlier update would block the rename below.
    let _ = std::fs::remove_file(&previous);
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
    Ok(())
}

fn write_staged(staged: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::File::create(staged)?;
    file.write_all(bytes)?;
    // The swap must never put a partly written executable in place.
    file.sync_all()
}

/// Deletes the copy the last portable update moved aside. The previous process may still
/// be exiting, so a locked file is left for the next start. `true` when one was removed.
pub fn remove_previous(executable: &Path) -> bool {
    std::fs::remove_file(sidecar(executable, "old")).is_ok()
}
