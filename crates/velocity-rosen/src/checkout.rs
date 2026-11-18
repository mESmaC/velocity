//! Working folders for checking a Rosen merge result or a landed commit.

use std::path::{Path, PathBuf};

use crate::workspace::session;
use crate::{ProjectServer, RosenError};

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

/// Writes `change`'s current merge result onto the base into `folder`, replacing what an
/// earlier checkout left there. A folder Velocity did not check out is never touched.
///
/// The folder itself stays: it is the reviewing agent's working directory, which Windows
/// won't remove. Rosen writes the result into a new sibling, whose entries then replace the
/// folder's.
///
/// With `prepared`, for a review that builds, the result comes from Rosen's warm pool when
/// it can (Rosen R3): a ready workspace brought to the merge result, with the build output
/// its prepare left, so the review's first build compiles what the change touched and not
/// the whole project. A review that only reads asks for the files alone, and takes nothing
/// from the pool.
pub async fn check_out_merge_result(
    server: &ProjectServer,
    reviewer: &str,
    change: &str,
    folder: &Path,
    prepared: bool,
) -> Result<MergeCheckout, RosenError> {
    let staging = claim_checkout(folder)?;
    let session = session(server, reviewer).await?;
    let checkout = if prepared {
        session
            .check_out_prepared_merge_result(change, &staging)
            .await?
    } else {
        session.check_out_merge_result(change, &staging).await?
    };
    replace_entries(folder, &staging)?;
    Ok(MergeCheckout::from_result(checkout.merge))
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

/// Empties `folder`, moves every entry of `staging` into it and removes `staging`.
fn replace_entries(folder: &Path, staging: &Path) -> Result<(), RosenError> {
    for entry in entries(folder)? {
        let path = entry.path();
        let removed = if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        removed.map_err(|source| io_error("remove", &path, source))?;
    }
    // Rosen leaves no folder when the merge conflicts.
    if !staging.exists() {
        return Ok(());
    }
    for entry in entries(staging)? {
        let target = folder.join(entry.file_name());
        std::fs::rename(entry.path(), &target)
            .map_err(|source| io_error("move", &target, source))?;
    }
    remove_folder(staging)
}

fn entries(folder: &Path) -> Result<Vec<std::fs::DirEntry>, RosenError> {
    std::fs::read_dir(folder)
        .and_then(Iterator::collect)
        .map_err(|source| io_error("read", folder, source))
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

/// Removes a review checkout and its marker, once its agent has stopped.
pub fn remove_checkout(folder: &Path) -> Result<(), RosenError> {
    let marker = checkout_marker(folder);
    if !marker.is_file() {
        return Ok(());
    }
    for path in [folder.to_path_buf(), staging_folder(folder)] {
        if path.exists() {
            remove_folder(&path)?;
        }
    }
    let _ = std::fs::remove_file(marker);
    Ok(())
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
