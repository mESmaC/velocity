//! Region-aware overlap between two diffs from a common base (F6).
use crate::{command::git, repository::GitError};
use std::collections::BTreeSet;
use std::path::Path;
use velocity_core::landing::{contested_file, needs_rebase, region_overlap};

/// Returns contested paths in both diffs whose changed line regions touch.
/// Binary/rename-only differences conservatively overlap the entire file.
pub fn overlapping_regions(
    path: &Path,
    common: &str,
    merged: &str,
) -> Result<BTreeSet<String>, GitError> {
    let own = regions(
        path,
        &[
            "diff",
            "--no-ext-diff",
            "--no-renames",
            "--unified=0",
            common,
        ],
    )?;
    let incoming = regions(
        path,
        &[
            "diff",
            "--no-ext-diff",
            "--no-renames",
            "--unified=0",
            common,
            merged,
        ],
    )?;
    Ok(own
        .iter()
        .filter(|(file, edits)| {
            needs_rebase(
                false,
                incoming
                    .iter()
                    .any(|(other, changes)| file == other && touches(edits, changes)),
                contested_file(file),
            )
        })
        .map(|(file, _)| file.clone())
        .collect())
}

fn touches(left: &[(u32, u32)], right: &[(u32, u32)]) -> bool {
    left.is_empty()
        || right.is_empty()
        || left.iter().any(|(start, length)| {
            right.iter().any(|(other_start, other_length)| {
                region_overlap(*start, *length, *other_start, *other_length)
            })
        })
}

type Regions = Vec<(String, Vec<(u32, u32)>)>;
fn regions(path: &Path, arguments: &[&str]) -> Result<Regions, GitError> {
    let output = git()
        .arg("-C")
        .arg(path)
        .args(["-c", "core.quotepath=false"])
        .args(arguments)
        .output()
        .map_err(GitError::Spawn)?;
    if !output.status.success() {
        return Err(GitError::Failed {
            operation: "diff",
            path: path.into(),
            stderr: String::from_utf8_lossy(&output.stderr).into(),
        });
    }
    let mut result: Regions = Vec::new();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(header) = line.strip_prefix("diff --git a/") {
            if let Some((_, file)) = header.rsplit_once(" b/") {
                result.push((file.to_owned(), Vec::new()));
            }
        } else if let Some(hunk) = line.strip_prefix("@@ -")
            && let Some(range) = hunk.split_whitespace().next()
        {
            let (start, length) = range.split_once(',').unwrap_or((range, "1"));
            if let (Ok(start), Ok(length), Some((_, regions))) =
                (start.parse(), length.parse(), result.last_mut())
            {
                regions.push((start, length));
            }
        }
    }
    Ok(result)
}

pub fn contested_path(file: &str) -> bool {
    contested_file(file)
}

/// Resolve the common ancestor so both region sets use the same line coordinates.
pub fn edited_overlap(path: &Path, merged: &str) -> Result<BTreeSet<String>, GitError> {
    if crate::resolve_commit(path, merged).is_none() {
        crate::manager::run_git_command(path, ["fetch", "--quiet", "origin", merged]).map_err(
            |error| GitError::Failed {
                operation: "fetch",
                path: path.into(),
                stderr: error.to_string(),
            },
        )?;
    }
    let output = git()
        .arg("-C")
        .arg(path)
        .args(["merge-base", "HEAD", merged])
        .output()
        .map_err(GitError::Spawn)?;
    if !output.status.success() {
        return Err(GitError::Failed {
            operation: "merge-base",
            path: path.into(),
            stderr: String::from_utf8_lossy(&output.stderr).into(),
        });
    }
    overlapping_regions(path, String::from_utf8_lossy(&output.stdout).trim(), merged)
}
