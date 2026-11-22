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
    // The marker is what makes `folder` Velocity's: without it, an existing target
    // directory belongs to someone else and is left alone.
    if folder.exists() && !marker.is_file() {
        return Err(RosenError::Io {
            action: "replace",
            path: folder.to_path_buf(),
            source: std::io::Error::other("the folder is not a Rosen review checkout"),
        });
    }
    // `<folder>.rosen-next` is exclusively Rosen/Velocity's staging path: remove any
    // leftover directory, file, or symlink from a crashed run before materialising (#606).
    remove_leftover_path(&staging)?;
    create_folder(folder)?;
    write_marker(&marker)?;
    Ok(staging)
}

/// Removes any leftover `<folder>.rosen-next` staging entry without touching `folder` (#606).
pub(crate) fn remove_staging_if_present(folder: &Path) -> Result<(), RosenError> {
    remove_leftover_path(&staging_folder(folder))
}

/// Where a merge result is written before it replaces what `folder` holds.
pub(crate) fn staging_folder(folder: &Path) -> PathBuf {
    sibling(folder, ".rosen-next")
}

fn remove_leftover_path(path: &Path) -> Result<(), RosenError> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io_error("inspect", path, source)),
    };
    if metadata.is_dir() {
        remove_folder(path)
    } else {
        std::fs::remove_file(path).map_err(|source| io_error("remove", path, source))
    }
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

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    // Risk (#606): a crashed deploy run can leave `<folder>.rosen-next` behind before
    // `<folder>.rosen-review` exists or after a partial materialisation. The next run must
    // remove that leftover staging directory before materialising instead of failing setup.
    #[test]
    fn claim_checkout_removes_leftover_staging_without_marker() -> Result<(), Box<dyn Error>> {
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let scratch = std::env::temp_dir().join(format!(
            "velocity-claim-checkout-{}-{stamp}",
            std::process::id()
        ));
        let folder = scratch.join("deploy-checkout-server");
        let leftover_staging = staging_folder(&folder);
        std::fs::create_dir_all(&leftover_staging)?;
        std::fs::write(leftover_staging.join("partial.txt"), b"stale")?;

        let staging = claim_checkout(&folder)?;

        assert_eq!(staging, leftover_staging);
        assert!(!staging.exists(), "leftover .rosen-next was not removed");
        assert!(folder.is_dir(), "checkout folder was not created");
        assert!(checkout_marker(&folder).is_file(), "marker was not written");
        std::fs::remove_dir_all(&scratch)?;
        Ok(())
    }
}
