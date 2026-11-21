//! Rosen inspection and Cargo workspace discovery for `preflight` (#537).

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use rosen_client::Endpoint;
pub use rosen_client::protocol::OverlapRecord;
use rosen_client::protocol::{DiffRequest, OverlapQuery};
use rosen_client::snapshot::WorkspaceMarker;
use rosen_core::PathPattern;
use toml_edit::{DocumentMut, Item, Value};
use velocity_core::preflight::{WorkspaceCrate, normalize_path};

use crate::RosenError;
use crate::connection::Connection;
use crate::landing::LandRefusal;

const MAX_GLOB_SCAN_DEPTH: usize = 3;
const SKIPPED_SCAN_FOLDERS: &[&str] = &[".git", ".rosen", "node_modules", "proofs", "target"];

/// One changed path in a Rosen diff against its parent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffFile {
    pub path: String,
    /// True when the file is present in the new tree (added or modified).
    pub exists: bool,
}

/// Authoritative Rosen state collected at the start of a preflight run.
pub struct PreflightSnapshot {
    pub change: String,
    pub files: Vec<DiffFile>,
    pub sensitive: Vec<PathPattern>,
    pub land_refusal: Option<LandRefusal>,
    pub unreadable: Vec<String>,
    pub overlaps: Vec<OverlapRecord>,
    pub client: Arc<Connection>,
}

/// Flushes local workspace edits to Rosen and inspects the change's diff, base sensitive
/// paths, merge/regeneration status, and active overlaps.
pub async fn inspect(endpoint: &str, worktree: &Path) -> Result<PreflightSnapshot, RosenError> {
    let marker = WorkspaceMarker::read(worktree).map_err(|source| RosenError::Io {
        action: "read the Rosen marker of",
        path: worktree.to_path_buf(),
        source,
    })?;
    let change = marker.change.to_string();
    let client = crate::connection::requests(&Endpoint::new(endpoint)).await?;
    let report = client.snapshot_workspace(worktree, None).await?;
    let diff_request = DiffRequest {
        to: Some(change.clone()),
        context: Some(0),
        ..DiffRequest::default()
    };
    let overlap_query = OverlapQuery {
        change: Some(change.clone()),
        paths: Vec::new(),
    };
    let (diff, config, land_check, overlaps) = tokio::try_join!(
        client.diff(diff_request),
        client.project_config(None),
        client.land_check(&change),
        client.overlaps(overlap_query),
    )?;
    let files = diff
        .files
        .into_iter()
        .map(|file| DiffFile {
            path: file.path,
            exists: file.new.is_some(),
        })
        .collect();
    let land_refusal = land_check
        .refusal
        .map(|refusal| match refusal.reason.as_str() {
            "regeneration_required" => LandRefusal::Regenerate {
                paths: refusal.paths,
            },
            "merge_conflict" | "conflicts" => LandRefusal::Conflict {
                paths: refusal.paths,
                message: refusal.message,
            },
            _ => LandRefusal::Other {
                code: refusal.reason,
                message: refusal.message,
            },
        });
    Ok(PreflightSnapshot {
        change,
        files,
        sensitive: config.config.sensitive,
        land_refusal,
        unreadable: report.unreadable,
        overlaps: overlaps.overlaps,
        client,
    })
}

/// Discovers Cargo workspace packages (name, relative directory, edition) from manifests
/// on disk without spawning `cargo metadata`.
pub fn discover_workspace_crates(worktree: &Path) -> (Vec<WorkspaceCrate>, String) {
    let Some(root_document) = read_manifest(worktree) else {
        let tauri = worktree.join("src-tauri");
        if let Some(document) = read_manifest(&tauri)
            && let Some(item) = package_from_manifest(&document, "src-tauri", "2021")
        {
            let edition = item.edition.clone();
            return (vec![item], edition);
        }
        return (Vec::new(), String::from("2024"));
    };
    let workspace_edition = root_document
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("edition"))
        .and_then(Item::as_str)
        .unwrap_or("2021")
        .to_owned();
    let mut crates = Vec::new();
    let mut seen = BTreeSet::new();
    if let Some(root_package) = package_from_manifest(&root_document, "", &workspace_edition) {
        seen.insert(String::new());
        crates.push(root_package);
    }
    let excludes = string_array(&root_document, "exclude");
    for member in string_array(&root_document, "members") {
        for folder in expand_member(worktree, &member) {
            let relative = relative_directory(worktree, &folder);
            if excludes
                .iter()
                .any(|pattern| matches_member(&relative, pattern))
            {
                continue;
            }
            if !seen.insert(relative.clone()) {
                continue;
            }
            if let Some(document) = read_manifest(&folder)
                && let Some(item) = package_from_manifest(&document, &relative, &workspace_edition)
            {
                crates.push(item);
            }
        }
    }
    (crates, workspace_edition)
}

fn read_manifest(folder: &Path) -> Option<DocumentMut> {
    let text = std::fs::read_to_string(folder.join("Cargo.toml")).ok()?;
    text.parse::<DocumentMut>().ok()
}

fn package_from_manifest(
    document: &DocumentMut,
    directory: &str,
    workspace_edition: &str,
) -> Option<WorkspaceCrate> {
    let package = document.get("package")?.as_table_like()?;
    let name = package.get("name")?.as_str()?.to_owned();
    let edition = match package.get("edition") {
        Some(item) if item.as_str().is_some() => {
            item.as_str().unwrap_or(workspace_edition).to_owned()
        }
        Some(item)
            if item
                .as_table_like()
                .and_then(|table| table.get("workspace"))
                .and_then(Item::as_bool)
                == Some(true) =>
        {
            workspace_edition.to_owned()
        }
        _ => String::from("2015"),
    };
    Some(WorkspaceCrate {
        name,
        directory: directory.to_owned(),
        edition,
    })
}

fn string_array(document: &DocumentMut, key: &str) -> Vec<String> {
    document
        .get("workspace")
        .and_then(|workspace| workspace.get(key))
        .and_then(Item::as_array)
        .map(|array| {
            array
                .iter()
                .filter_map(Value::as_str)
                .map(normalize_path)
                .collect()
        })
        .unwrap_or_default()
}

fn expand_member(worktree: &Path, member: &str) -> Vec<PathBuf> {
    if !member.contains(['*', '?', '[']) {
        let folder = worktree.join(member);
        return folder
            .join("Cargo.toml")
            .is_file()
            .then_some(folder)
            .into_iter()
            .collect();
    }
    let base = glob_prefix_folder(worktree, member);
    let mut found = Vec::new();
    scan_manifests(&base, MAX_GLOB_SCAN_DEPTH, &mut found);
    found
        .into_iter()
        .filter(|folder| matches_member(&relative_directory(worktree, folder), member))
        .collect()
}

fn glob_prefix_folder(root: &Path, pattern: &str) -> PathBuf {
    let mut prefix = root.to_path_buf();
    for component in Path::new(pattern).components() {
        let Component::Normal(segment) = component else {
            continue;
        };
        if segment.to_string_lossy().contains(['*', '?', '[']) {
            break;
        }
        prefix.push(segment);
    }
    prefix
}

fn scan_manifests(base: &Path, max_depth: usize, found: &mut Vec<PathBuf>) {
    let mut pending = vec![(base.to_path_buf(), 0usize)];
    while let Some((folder, depth)) = pending.pop() {
        if folder.join("Cargo.toml").is_file() {
            found.push(folder.clone());
        }
        if depth >= max_depth {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let skipped = entry
                .file_name()
                .to_str()
                .is_some_and(|name| SKIPPED_SCAN_FOLDERS.contains(&name));
            if !skipped {
                pending.push((entry.path(), depth + 1));
            }
        }
    }
}

fn relative_directory(worktree: &Path, folder: &Path) -> String {
    folder
        .strip_prefix(worktree)
        .ok()
        .map(|relative| normalize_path(&relative.to_string_lossy()))
        .unwrap_or_default()
}

fn matches_member(path: &str, pattern: &str) -> bool {
    let pattern = pattern.trim_end_matches('/');
    let path_parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
    let pattern_parts: Vec<&str> = pattern.split('/').filter(|part| !part.is_empty()).collect();
    match_segments(&pattern_parts, &path_parts)
}

fn match_segments(pattern: &[&str], path: &[&str]) -> bool {
    match (pattern.first(), path.first()) {
        (None, None) => true,
        (Some(&"**"), _) => {
            match_segments(&pattern[1..], path)
                || (!path.is_empty() && match_segments(pattern, &path[1..]))
        }
        (Some(head), Some(segment)) => {
            match_wildcards(head, segment) && match_segments(&pattern[1..], &path[1..])
        }
        _ => false,
    }
}

fn match_wildcards(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star = None;
    let mut match_ti = 0usize;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            match_ti = ti;
            pi += 1;
        } else if let Some(star_index) = star {
            pi = star_index + 1;
            match_ti += 1;
            ti = match_ti;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|character| *character == '*')
}
