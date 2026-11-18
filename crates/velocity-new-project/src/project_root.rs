use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::error::NewProjectError;

/// Check the actual project destination before setup can modify it.
pub fn validate_project_root(path: &Path) -> Result<(), NewProjectError> {
    let drive_root = || NewProjectError::DriveRoot(path.display().to_string());
    if path.parent().is_none() && path.is_absolute() {
        return Err(drive_root());
    }
    let resolved =
        resolve_existing_prefix(path).map_err(|source| NewProjectError::InspectFolder {
            path: path.to_path_buf(),
            source,
        })?;
    if resolved.parent().is_none() {
        return Err(drive_root());
    }

    let refuse = |reason| NewProjectError::ProtectedFolder {
        path: path.to_path_buf(),
        reason,
    };
    if let Some(home) = std::env::home_dir().map(resolve_protected)
        && same_folder(&resolved, &home)
    {
        return Err(refuse("it is your home folder"));
    }
    for protected in system_folders().map(resolve_protected) {
        if resolved
            .ancestors()
            .any(|ancestor| same_folder(ancestor, &protected))
        {
            return Err(refuse("it is in a system folder"));
        }
    }
    Ok(())
}

fn resolve_protected(path: PathBuf) -> PathBuf {
    resolve_existing_prefix(&path).unwrap_or(path)
}

fn resolve_existing_prefix(path: &Path) -> io::Result<PathBuf> {
    let absolute = std::path::absolute(path)?;
    let mut missing = Vec::new();
    let mut candidate = absolute.as_path();
    loop {
        match fs::canonicalize(candidate) {
            Ok(mut resolved) => {
                // Follow aliases in the existing part even when the project is not created yet.
                resolved.extend(missing.into_iter().rev());
                return Ok(resolved);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let Some((parent, name)) = candidate.parent().zip(candidate.file_name()) else {
                    return Err(error);
                };
                missing.push(name);
                candidate = parent;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(windows)]
fn system_folders() -> impl Iterator<Item = PathBuf> {
    [
        "APPDATA",
        "LOCALAPPDATA",
        "ProgramData",
        "ProgramFiles",
        "ProgramFiles(x86)",
        "ProgramW6432",
        "SystemRoot",
        "WINDIR",
    ]
    .into_iter()
    .filter_map(std::env::var_os)
    .map(PathBuf::from)
    .chain(std::env::home_dir().map(|home| home.join("AppData")))
    .filter(|path| path.is_absolute())
}

#[cfg(not(windows))]
fn system_folders() -> impl Iterator<Item = PathBuf> {
    [
        "/etc",
        "/usr",
        "/bin",
        "/sbin",
        "/System",
        "/Library",
        "/Applications",
    ]
    .into_iter()
    .map(PathBuf::from)
}

fn same_folder(left: &Path, right: &Path) -> bool {
    #[cfg(windows)]
    {
        left.as_os_str()
            .as_encoded_bytes()
            .eq_ignore_ascii_case(right.as_os_str().as_encoded_bytes())
    }
    #[cfg(not(windows))]
    {
        left == right
    }
}
