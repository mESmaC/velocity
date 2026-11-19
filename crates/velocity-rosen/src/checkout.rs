//! Working folders for checking a Rosen merge result or a landed commit.

use std::path::{Path, PathBuf};

use crate::RosenError;

/// A change's merge result, checked out to build in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MergeCheckout {
    pub head: String,
    pub base: String,
    /// Paths needing the author's repair: conflicts or generated files to regenerate.
    pub conflicts: Vec<String>,
}

impl MergeCheckout {
    /// Never certify a base copy of a generated file as the change's merged artifact.
    pub(crate) fn from_result(merge: rosen_client::protocol::MergeResultAnswer) -> Self {
        let mut conflicts = merge.conflicts;
        conflicts.extend(merge.regenerate);
        conflicts.sort();
        conflicts.dedup();
        Self {
            head: merge.head.to_string(),
            base: merge.base.to_string(),
            conflicts,
        }
    }
}

/// Makes `folder` a checkout of Velocity's, with nothing staged beside it, and returns
/// where the next merge result is staged.
pub(crate) fn claim_checkout(folder: &Path) -> Result<PathBuf, RosenError> {
    let marker = checkout_marker(folder);
    let staging = staging_folder(folder);
    // The marker is what makes the folder and its staging sibling Velocity's: without it,
    // either one belongs to someone else and is left alone.
    for path in [folder, staging.as_path()] {
        if path.exists() && !marker.is_file() {
            return Err(RosenError::Io {
                action: "replace",
                path: path.to_path_buf(),
                source: std::io::Error::other("the folder is not a Rosen review checkout"),
            });
        }
    }
    if staging.exists() {
        remove_folder(&staging)?;
    }
    create_folder(folder)?;
    write_marker(&marker)?;
    Ok(staging)
}

/// Where a merge result is written before it replaces what `folder` holds.
pub(crate) fn staging_folder(folder: &Path) -> PathBuf {
    sibling(folder, ".rosen-next")
}

fn create_folder(folder: &Path) -> Result<(), RosenError> {
    std::fs::create_dir_all(folder).map_err(|source| io_error("create", folder, source))
}

fn io_error(action: &'static str, path: &Path, source: std::io::Error) -> RosenError {
    RosenError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

/// Beside the folder, not in it: the folder is the build tree the checks run on.
fn checkout_marker(folder: &Path) -> PathBuf {
    sibling(folder, ".rosen-review")
}

pub(crate) fn sibling(folder: &Path, suffix: &str) -> PathBuf {
    let mut name = folder.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    folder.with_file_name(name)
}

fn write_marker(marker: &Path) -> Result<(), RosenError> {
    std::fs::write(marker, b"").map_err(|source| RosenError::Io {
        action: "write",
        path: marker.to_path_buf(),
        source,
    })
}

fn remove_folder(folder: &Path) -> Result<(), RosenError> {
    std::fs::remove_dir_all(folder).map_err(|source| RosenError::Io {
        action: "remove",
        path: folder.to_path_buf(),
        source,
    })
}
