//! Keeps pooled light-run Cargo target folders free of stale local crate fingerprints
//! when different workspaces of a project take turns building into them (#527, #682, #737).
//!
//! Cargo hashes workspace members by workspace-relative path and checks source mtimes
//! against the fingerprint's timestamp, so a newer build from Workspace A causes
//! Workspace B (whose edited or untouched sources have older mtimes) to silently link
//! Workspace A's compiled crate. While holding `.velocity-light-run.lock`, we record
//! which workspace last built into that target folder (along with its Rosen parent tree
//! and touched local packages when known). Switching workspaces on the same base with
//! unchanged manifests/lockfiles invalidates only `touched(previous) ∪ touched(incoming)`,
//! letting Cargo recompile downstream dependents while keeping untouched upstream crates
//! and build scripts warm; any base mismatch, manifest/lockfile edit, or unknown scope
//! falls back to invalidating all local packages while keeping registry and git
//! dependency artifacts warm.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashSet};
use std::io;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use rosen_client::Endpoint;
use rosen_client::protocol::DiffRequest;
use rosen_client::snapshot::WorkspaceMarker;
use serde::{Deserialize, Serialize};
use toml_edit::{DocumentMut, Item, Value};
use velocity_core::preflight::{WorkspaceCrate, normalize_path, owning_crate};

const LAST_WORKSPACE_MARKER: &str = ".velocity-last-workspace";
const FINGERPRINT_HASH_LENGTH: usize = 16;
const MAX_FALLBACK_SCAN_DEPTH: usize = 2;
const MAX_GLOB_SCAN_DEPTH: usize = 3;
const SCOPE_INSPECT_TIMEOUT: Duration = Duration::from_secs(5);
const SKIPPED_SCAN_FOLDERS: &[&str] = &[".git", ".rosen", "node_modules", "proofs", "target"];

#[derive(Deserialize)]
struct RosenWorkspaceRecord {
    #[serde(default)]
    change: Option<String>,
}

/// Scoped Rosen change metadata for a workspace using a pooled light-run target (#737).
///
/// When both the previous target occupant and the incoming workspace share the same
/// Rosen parent tree (`base`), neither touched Cargo manifests/lockfiles/build config,
/// and both have known touched local packages, [`prepare_with_scope`] only needs to
/// invalidate `touched_packages(previous) ∪ touched_packages(incoming)`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceScope {
    /// Rosen parent tree ID (`diff.from`) that `touched_packages` is relative to.
    pub base: String,
    /// Local Cargo package names modified in this change relative to `base`.
    #[serde(default)]
    pub touched_packages: BTreeSet<String>,
    /// True when Cargo manifests, lockfiles, toolchain/Cargo config, or unowned Rust
    /// build inputs changed relative to `base`, forcing full local-crate invalidation.
    #[serde(default)]
    pub manifests_changed: bool,
}

impl WorkspaceScope {
    /// Whether this scope can participate in narrowed invalidation on a matching base.
    pub fn is_scoped_eligible(&self) -> bool {
        !self.base.trim().is_empty() && !self.manifests_changed
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct LastWorkspaceRecord {
    identity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<WorkspaceScope>,
}

/// Builds a [`WorkspaceScope`] from a Rosen change's parent tree (`diff.from`), changed
/// repository-relative paths, and discovered workspace crates (#737).
///
/// Returns `None` when `base` is empty or no workspace crates were discovered (for example
/// nested non-root Cargo layouts), causing callers to fall back to full invalidation.
pub fn scope_from_changed_paths(
    base: &str,
    changed_paths: &[&str],
    crates: &[WorkspaceCrate],
) -> Option<WorkspaceScope> {
    let base = base.trim();
    if base.is_empty() || crates.is_empty() {
        return None;
    }
    let mut touched_packages = BTreeSet::new();
    let mut manifests_changed = false;
    for raw_path in changed_paths {
        let path = normalize_path(raw_path);
        if is_workspace_build_input(&path) {
            manifests_changed = true;
        }
        if let Some(owner) = owning_crate(&path, crates) {
            touched_packages.insert(owner.name.clone());
        } else if path.ends_with(".rs") {
            manifests_changed = true;
        }
    }
    Some(WorkspaceScope {
        base: base.to_owned(),
        touched_packages,
        manifests_changed,
    })
}

fn is_workspace_build_input(path: &str) -> bool {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    matches!(
        file_name,
        "Cargo.toml"
            | "Cargo.lock"
            | "rust-toolchain"
            | "rust-toolchain.toml"
            | "clippy.toml"
            | ".clippy.toml"
            | "rosen.toml"
    ) || path == ".cargo"
        || path.starts_with(".cargo/")
        || path.contains("/.cargo/")
        || path.starts_with("proofs/fixtures/")
        || path == "ui/src/lib/theme/tokens.css"
}

/// Flushes local workspace edits and derives a [`WorkspaceScope`] from Rosen's diff
/// against the change's parent tree (#737). Returns `None` for non-Rosen workspaces,
/// workspaces with unreadable files or a pending server rebase, or on any Rosen/timeout
/// error so callers safely fall back to full local-crate invalidation.
pub async fn inspect_workspace_scope(endpoint: &str, worktree: &Path) -> Option<WorkspaceScope> {
    tokio::time::timeout(
        SCOPE_INSPECT_TIMEOUT,
        inspect_workspace_scope_inner(endpoint, worktree),
    )
    .await
    .ok()
    .flatten()
}

async fn inspect_workspace_scope_inner(endpoint: &str, worktree: &Path) -> Option<WorkspaceScope> {
    let marker = WorkspaceMarker::read(worktree).ok()?;
    let change = marker.change.to_string();
    let client = crate::connection::requests(&Endpoint::new(endpoint))
        .await
        .ok()?;
    let report = client.snapshot_workspace(worktree, None).await.ok()?;
    if !report.unreadable.is_empty() {
        return None;
    }
    let diff = client
        .diff(DiffRequest {
            to: Some(change),
            context: Some(0),
            ..DiffRequest::default()
        })
        .await
        .ok()?;
    if report.tree != diff.to {
        return None;
    }
    let (crates, _) = crate::preflight::discover_workspace_crates(worktree);
    let changed_paths: Vec<&str> = diff.files.iter().map(|file| file.path.as_str()).collect();
    scope_from_changed_paths(&diff.from.to_string(), &changed_paths, &crates)
}

/// Blocking wrapper for CLI shims (`cargo check -p` / `rosen run --`) to resolve a
/// workspace's Rosen change scope (#737).
pub(crate) fn inspect_workspace_scope_blocking(
    endpoint: &str,
    worktree: &Path,
) -> Option<WorkspaceScope> {
    if !worktree.join(".rosen").join("workspace.json").is_file() {
        return None;
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    runtime.block_on(inspect_workspace_scope(endpoint, worktree))
}

/// Ensures `target` holds no local-crate fingerprints or build-script outputs from a
/// different workspace, falling back to full local-crate invalidation.
#[cfg(test)]
pub(crate) fn prepare(target: &Path, workspace_root: &Path, directory: &Path) -> io::Result<()> {
    prepare_with_scope(target, workspace_root, directory, None)
}

/// Ensures `target` holds no stale local-crate fingerprints or build-script outputs from
/// a different workspace (#527, #737). The caller must already hold `.velocity-light-run.lock`.
pub(crate) fn prepare_with_scope(
    target: &Path,
    workspace_root: &Path,
    directory: &Path,
    scope: Option<&WorkspaceScope>,
) -> io::Result<()> {
    let identity = workspace_identity(workspace_root);
    let incoming_scope = scope.filter(|item| !item.base.trim().is_empty());
    let marker = target.join(LAST_WORKSPACE_MARKER);
    let previous = read_last_workspace(&marker);
    if let Some(previous) = previous.as_ref()
        && previous.identity == identity
    {
        return update_same_workspace_marker(&marker, previous, incoming_scope);
    }
    // Clear the marker before invalidating so an interrupted cleanup never leaves the
    // target directory appearing fresh for any workspace on the next run. Overwriting in
    // place (rather than unlinking) avoids Windows delete-pending sharing errors when
    // another process reads the marker for pool affinity ranking (#682).
    if marker.exists() {
        std::fs::write(&marker, "")?;
    }
    if let Some(packages) = scoped_invalidation_packages(
        previous.as_ref().and_then(|record| record.scope.as_ref()),
        incoming_scope,
    ) {
        if !packages.is_empty() {
            invalidate_packages(target, &packages)?;
        }
        std::fs::create_dir_all(target)?;
        return write_last_workspace(
            &marker,
            &LastWorkspaceRecord {
                identity,
                scope: incoming_scope.cloned(),
            },
        );
    }
    let packages = collect_local_packages(workspace_root, directory);
    if packages.is_empty() {
        return Ok(());
    }
    invalidate_packages(target, &packages)?;
    std::fs::create_dir_all(target)?;
    write_last_workspace(
        &marker,
        &LastWorkspaceRecord {
            identity,
            scope: incoming_scope.cloned(),
        },
    )
}

fn update_same_workspace_marker(
    marker: &Path,
    previous: &LastWorkspaceRecord,
    incoming_scope: Option<&WorkspaceScope>,
) -> io::Result<()> {
    let merged_scope = merge_same_workspace_scope(previous.scope.as_ref(), incoming_scope);
    if merged_scope == previous.scope {
        touch_marker(marker);
        return Ok(());
    }
    write_last_workspace(
        marker,
        &LastWorkspaceRecord {
            identity: previous.identity.clone(),
            scope: merged_scope,
        },
    )
}

/// Keeps the union of touched packages across consecutive runs in the same workspace on
/// the same base, or clears the scope if the base moved, manifests changed, or either run
/// lacked scope metadata (forcing full invalidation on the next workspace switch).
fn merge_same_workspace_scope(
    previous: Option<&WorkspaceScope>,
    incoming: Option<&WorkspaceScope>,
) -> Option<WorkspaceScope> {
    let (Some(previous), Some(incoming)) = (previous, incoming) else {
        return None;
    };
    if !previous.is_scoped_eligible()
        || !incoming.is_scoped_eligible()
        || previous.base != incoming.base
    {
        return None;
    }
    let mut touched_packages = previous.touched_packages.clone();
    touched_packages.extend(incoming.touched_packages.iter().cloned());
    Some(WorkspaceScope {
        base: previous.base.clone(),
        touched_packages,
        manifests_changed: false,
    })
}

fn scoped_invalidation_packages(
    previous: Option<&WorkspaceScope>,
    incoming: Option<&WorkspaceScope>,
) -> Option<HashSet<String>> {
    let (Some(previous), Some(incoming)) = (previous, incoming) else {
        return None;
    };
    if !previous.is_scoped_eligible()
        || !incoming.is_scoped_eligible()
        || previous.base != incoming.base
    {
        return None;
    }
    Some(
        previous
            .touched_packages
            .iter()
            .chain(incoming.touched_packages.iter())
            .cloned()
            .collect(),
    )
}

fn read_last_workspace(marker: &Path) -> Option<LastWorkspaceRecord> {
    let contents = std::fs::read_to_string(marker).ok()?;
    if contents.trim().is_empty() {
        return None;
    }
    if let Ok(record) = serde_json::from_str::<LastWorkspaceRecord>(&contents)
        && !record.identity.trim().is_empty()
    {
        return Some(record);
    }
    // Legacy plain-text marker written by older Velocity versions or tests (#527, #682).
    Some(LastWorkspaceRecord {
        identity: contents,
        scope: None,
    })
}

fn write_last_workspace(marker: &Path, record: &LastWorkspaceRecord) -> io::Result<()> {
    let serialized = serde_json::to_string(record).map_err(io::Error::other)?;
    std::fs::write(marker, serialized)
}

/// Orders candidate target directories for `workspace_identity` (#682):
/// 1. Directories whose `.velocity-last-workspace` already matches the caller (newest first)
///    so back-to-back runs keep local crate fingerprints warm.
/// 2. Unclaimed or empty-marker directories (in slot order) to avoid evicting another
///    workspace's warm target.
/// 3. Other workspaces' directories, least recently used first (by marker mtime).
pub(crate) fn rank_targets(targets: &[PathBuf], workspace_identity: &str) -> Vec<PathBuf> {
    if targets.len() <= 1 {
        return targets.to_vec();
    }
    let mut ranked: Vec<(usize, TargetSlotKind)> = targets
        .iter()
        .enumerate()
        .map(|(index, target)| (index, inspect_target(target, workspace_identity)))
        .collect();
    ranked.sort_by(compare_candidates);
    ranked
        .into_iter()
        .map(|(index, _)| targets[index].clone())
        .collect()
}

enum TargetSlotKind {
    AffinityMatch(Option<SystemTime>),
    Unclaimed,
    OtherWorkspace(Option<SystemTime>),
}

fn inspect_target(target: &Path, workspace_identity: &str) -> TargetSlotKind {
    let marker = target.join(LAST_WORKSPACE_MARKER);
    let Some(record) = read_last_workspace(&marker) else {
        return TargetSlotKind::Unclaimed;
    };
    let modified = std::fs::metadata(&marker)
        .ok()
        .and_then(|metadata| metadata.modified().ok());
    if record.identity == workspace_identity {
        TargetSlotKind::AffinityMatch(modified)
    } else {
        TargetSlotKind::OtherWorkspace(modified)
    }
}

fn compare_candidates(left: &(usize, TargetSlotKind), right: &(usize, TargetSlotKind)) -> Ordering {
    match (&left.1, &right.1) {
        (TargetSlotKind::AffinityMatch(left_time), TargetSlotKind::AffinityMatch(right_time)) => {
            right_time.cmp(left_time).then_with(|| left.0.cmp(&right.0))
        }
        (TargetSlotKind::AffinityMatch(_), _) => Ordering::Less,
        (_, TargetSlotKind::AffinityMatch(_)) => Ordering::Greater,
        (TargetSlotKind::Unclaimed, TargetSlotKind::Unclaimed) => left.0.cmp(&right.0),
        (TargetSlotKind::Unclaimed, TargetSlotKind::OtherWorkspace(_)) => Ordering::Less,
        (TargetSlotKind::OtherWorkspace(_), TargetSlotKind::Unclaimed) => Ordering::Greater,
        (TargetSlotKind::OtherWorkspace(left_time), TargetSlotKind::OtherWorkspace(right_time)) => {
            left_time.cmp(right_time).then_with(|| left.0.cmp(&right.0))
        }
    }
}

fn touch_marker(marker: &Path) {
    if let Ok(file) = std::fs::OpenOptions::new().write(true).open(marker) {
        let _ = file.set_modified(SystemTime::now());
    }
}

pub(crate) fn workspace_identity(workspace_root: &Path) -> String {
    let canonical =
        std::fs::canonicalize(workspace_root).unwrap_or_else(|_| workspace_root.to_path_buf());
    match rosen_change_id(workspace_root) {
        Some(change) => format!("{}\nchange:{change}\n", canonical.display()),
        None => format!("{}\n", canonical.display()),
    }
}

fn rosen_change_id(workspace_root: &Path) -> Option<String> {
    let text =
        std::fs::read_to_string(workspace_root.join(".rosen").join("workspace.json")).ok()?;
    let record: RosenWorkspaceRecord = serde_json::from_str(&text).ok()?;
    record.change.filter(|value| !value.trim().is_empty())
}

fn collect_local_packages(workspace_root: &Path, directory: &Path) -> HashSet<String> {
    let mut packages = HashSet::new();
    for root in cargo_roots(workspace_root, directory) {
        collect_lockfile_local_packages(&root, &mut packages);
        collect_manifest_local_packages(&root, &mut packages);
    }
    packages
}

fn cargo_roots(workspace_root: &Path, directory: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut candidate = Some(directory);
    while let Some(folder) = candidate {
        if folder.join("Cargo.toml").is_file() {
            push_unique(&mut roots, folder.to_path_buf());
        }
        if same_path(folder, workspace_root) {
            break;
        }
        candidate = folder.parent();
    }
    if workspace_root.join("Cargo.toml").is_file() {
        push_unique(&mut roots, workspace_root.to_path_buf());
    }
    if roots.is_empty() {
        let tauri_root = workspace_root.join("src-tauri");
        if tauri_root.join("Cargo.toml").is_file() {
            push_unique(&mut roots, tauri_root);
        } else {
            scan_manifests_within(workspace_root, MAX_FALLBACK_SCAN_DEPTH, &mut roots);
        }
    }
    roots
}

fn push_unique(roots: &mut Vec<PathBuf>, path: PathBuf) {
    if !roots.contains(&path) {
        roots.push(path);
    }
}

fn same_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    matches!(
        (std::fs::canonicalize(left), std::fs::canonicalize(right)),
        (Ok(first), Ok(second)) if first == second
    )
}

fn scan_manifests_within(base: &Path, max_depth: usize, roots: &mut Vec<PathBuf>) {
    let mut pending = vec![(base.to_path_buf(), 0usize)];
    while let Some((folder, depth)) = pending.pop() {
        if folder.join("Cargo.toml").is_file() {
            push_unique(roots, folder.clone());
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
            let name = entry.file_name();
            let skipped = name
                .to_str()
                .is_some_and(|value| SKIPPED_SCAN_FOLDERS.contains(&value));
            if !skipped {
                pending.push((entry.path(), depth + 1));
            }
        }
    }
}

/// In `Cargo.lock`, registry and git dependencies carry a `source = ...` entry while
/// workspace members and local path dependencies omit `source`.
fn collect_lockfile_local_packages(root: &Path, packages: &mut HashSet<String>) {
    let Ok(text) = std::fs::read_to_string(root.join("Cargo.lock")) else {
        return;
    };
    let Ok(document) = text.parse::<DocumentMut>() else {
        return;
    };
    let Some(entries) = document.get("package").and_then(Item::as_array_of_tables) else {
        return;
    };
    for entry in entries.iter() {
        if entry.get("source").is_some() {
            continue;
        }
        if let Some(name) = entry.get("name").and_then(Item::as_str) {
            packages.insert(name.to_owned());
        }
    }
}

fn collect_manifest_local_packages(root: &Path, packages: &mut HashSet<String>) {
    let Some(document) = read_manifest(root) else {
        return;
    };
    insert_manifest_package_name(&document, packages);
    if let Some(members) = document
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(Item::as_array)
    {
        for member in members.iter().filter_map(Value::as_str) {
            collect_member_packages(root, member, packages);
        }
    }
    collect_path_dependency_packages(root, &document, packages);
}

fn read_manifest(folder: &Path) -> Option<DocumentMut> {
    let text = std::fs::read_to_string(folder.join("Cargo.toml")).ok()?;
    text.parse::<DocumentMut>().ok()
}

fn insert_manifest_package_name(document: &DocumentMut, packages: &mut HashSet<String>) {
    if let Some(name) = document
        .get("package")
        .and_then(|package| package.get("name"))
        .and_then(Item::as_str)
    {
        packages.insert(name.to_owned());
    }
}

fn collect_member_packages(root: &Path, member: &str, packages: &mut HashSet<String>) {
    if member.contains(['*', '?', '[']) {
        let base = glob_prefix_folder(root, member);
        let mut manifests = Vec::new();
        scan_manifests_within(&base, MAX_GLOB_SCAN_DEPTH, &mut manifests);
        for folder in manifests {
            if let Some(document) = read_manifest(&folder) {
                insert_manifest_package_name(&document, packages);
            }
        }
        return;
    }
    let folder = root.join(member);
    if let Some(document) = read_manifest(&folder) {
        insert_manifest_package_name(&document, packages);
        collect_path_dependency_packages(&folder, &document, packages);
    }
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

fn collect_path_dependency_packages(
    manifest_folder: &Path,
    document: &DocumentMut,
    packages: &mut HashSet<String>,
) {
    let sections = [
        document
            .get("workspace")
            .and_then(|workspace| workspace.get("dependencies")),
        document.get("dependencies"),
        document.get("dev-dependencies"),
        document.get("build-dependencies"),
    ];
    for table in sections
        .into_iter()
        .flatten()
        .filter_map(Item::as_table_like)
    {
        for (_, item) in table.iter() {
            let Some(path) = item
                .as_table_like()
                .and_then(|dependency| dependency.get("path"))
                .and_then(Item::as_str)
            else {
                continue;
            };
            let dependency_folder = manifest_folder.join(path);
            if let Some(dependency_manifest) = read_manifest(&dependency_folder) {
                insert_manifest_package_name(&dependency_manifest, packages);
            }
        }
    }
}

fn invalidate_packages(target: &Path, packages: &HashSet<String>) -> io::Result<()> {
    let entries = match std::fs::read_dir(target) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        if has_cargo_profile_layout(&path) {
            invalidate_profile_directory(&path, packages)?;
            continue;
        }
        invalidate_target_triple_directory(&path, packages)?;
    }
    Ok(())
}

fn invalidate_target_triple_directory(
    triple_directory: &Path,
    packages: &HashSet<String>,
) -> io::Result<()> {
    let entries = match std::fs::read_dir(triple_directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        if has_cargo_profile_layout(&path) {
            invalidate_profile_directory(&path, packages)?;
        }
    }
    Ok(())
}

fn has_cargo_profile_layout(folder: &Path) -> bool {
    folder.join(".fingerprint").is_dir() || folder.join("build").is_dir()
}

fn invalidate_profile_directory(
    profile_directory: &Path,
    packages: &HashSet<String>,
) -> io::Result<()> {
    remove_matching_entries(&profile_directory.join(".fingerprint"), packages)?;
    remove_matching_entries(&profile_directory.join("build"), packages)
}

fn remove_matching_entries(parent: &Path, packages: &HashSet<String>) -> io::Result<()> {
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !matches_package(name, packages) {
            continue;
        }
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            remove_dir_all_if_exists(&path)?;
        } else {
            remove_file_if_exists(&path)?;
        }
    }
    Ok(())
}

/// Cargo names per-unit directories in `.fingerprint/` and `build/` as
/// `<package-name>-<16 hex digits>`.
fn matches_package(entry_name: &str, packages: &HashSet<String>) -> bool {
    let Some((package, hash)) = entry_name.rsplit_once('-') else {
        return false;
    };
    hash.len() == FINGERPRINT_HASH_LENGTH
        && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        && packages.contains(package)
}

fn remove_file_if_exists(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn remove_dir_all_if_exists(path: &Path) -> io::Result<()> {
    match std::fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::path::Path;

    use tempfile::tempdir;

    use super::{LAST_WORKSPACE_MARKER, collect_local_packages, prepare};

    fn write_file(path: &Path, contents: &str) -> Result<(), Box<dyn Error>> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, contents)?;
        Ok(())
    }

    fn seed_workspace(root: &Path, change_id: &str) -> Result<(), Box<dyn Error>> {
        write_file(
            &root.join(".rosen").join("workspace.json"),
            &format!(r#"{{"schema":"rosen.workspace/v1","change":"{change_id}"}}"#),
        )?;
        write_file(
            &root.join("Cargo.toml"),
            r#"[workspace]
members = ["crates/*"]
"#,
        )?;
        write_file(
            &root.join("crates").join("demo-core").join("Cargo.toml"),
            r#"[package]
name = "demo-core"
version = "0.1.0"
edition = "2024"
"#,
        )?;
        write_file(
            &root.join("Cargo.lock"),
            r#"version = 4

[[package]]
name = "demo-core"
version = "0.1.0"

[[package]]
name = "demo-extra"
version = "0.1.0"

[[package]]
name = "serde"
version = "1.0.0"
source = "registry+https://github.com/rust-lang/crates.io-index"

[[package]]
name = "rosen-client"
version = "0.1.0"
source = "git+https://github.com/mESmaC/rosen#abc"
"#,
        )?;
        Ok(())
    }

    fn seed_target_entry(target: &Path, relative: &str) -> Result<(), Box<dyn Error>> {
        write_file(&target.join(relative).join("stamp"), "built")
    }

    // Risk (#527): Cargo hashes workspace members by workspace-relative path and checks
    // source mtimes against the shared target's fingerprint, so a build in Workspace A
    // causes Workspace B to reuse Workspace A's compiled crate instead of recompiling.
    #[test]
    fn switching_workspaces_invalidates_local_crates_but_preserves_dependencies()
    -> Result<(), Box<dyn Error>> {
        let scratch = tempdir()?;
        let workspace_a = scratch.path().join("issue-511");
        let workspace_b = scratch.path().join("issue-527");
        let target = scratch.path().join(".velocity-agent-target");
        seed_workspace(&workspace_a, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")?;
        seed_workspace(&workspace_b, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")?;

        prepare(&target, &workspace_a, &workspace_a)?;
        seed_target_entry(&target, "debug/.fingerprint/demo-core-0a78db7edc06e08a")?;
        seed_target_entry(&target, "debug/build/demo-core-0a78db7edc06e08a")?;
        seed_target_entry(&target, "debug/.fingerprint/demo-extra-1111222233334444")?;
        seed_target_entry(
            &target,
            "x86_64-pc-windows-msvc/debug/.fingerprint/demo-core-24af54a4cef85dcd",
        )?;
        seed_target_entry(&target, "debug/.fingerprint/serde-53fb7d8c4a37260a")?;
        seed_target_entry(&target, "debug/build/serde-fdfaac1d3080c5ec")?;
        seed_target_entry(&target, "debug/.fingerprint/rosen-client-078561dcbbf21b1f")?;

        // Repeating in the same workspace keeps local fingerprints warm.
        prepare(&target, &workspace_a, &workspace_a)?;
        assert!(
            target
                .join("debug/.fingerprint/demo-core-0a78db7edc06e08a")
                .is_dir()
        );

        // Switching workspaces invalidates local crates across profiles/triples while
        // keeping registry and git dependency artifacts untouched.
        prepare(&target, &workspace_b, &workspace_b)?;
        for removed in [
            "debug/.fingerprint/demo-core-0a78db7edc06e08a",
            "debug/build/demo-core-0a78db7edc06e08a",
            "debug/.fingerprint/demo-extra-1111222233334444",
            "x86_64-pc-windows-msvc/debug/.fingerprint/demo-core-24af54a4cef85dcd",
        ] {
            assert!(!target.join(removed).exists(), "still exists: {removed}");
        }
        for kept in [
            "debug/.fingerprint/serde-53fb7d8c4a37260a",
            "debug/build/serde-fdfaac1d3080c5ec",
            "debug/.fingerprint/rosen-client-078561dcbbf21b1f",
        ] {
            assert!(target.join(kept).is_dir(), "should remain: {kept}");
        }
        Ok(())
    }

    // Risk (#527): A workspace recreated at the same folder path for a new Rosen change
    // must still invalidate local fingerprints from the previous change built there.
    #[test]
    fn new_rosen_change_at_same_path_invalidates_local_crates() -> Result<(), Box<dyn Error>> {
        let scratch = tempdir()?;
        let workspace = scratch.path().join("issue-527");
        let target = scratch.path().join(".velocity-agent-target");
        seed_workspace(&workspace, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")?;

        prepare(&target, &workspace, &workspace)?;
        seed_target_entry(&target, "debug/.fingerprint/demo-core-0a78db7edc06e08a")?;
        write_file(
            &workspace.join(".rosen").join("workspace.json"),
            r#"{"schema":"rosen.workspace/v1","change":"cccccccccccccccccccccccccccccccc"}"#,
        )?;
        prepare(&target, &workspace, &workspace)?;
        assert!(
            !target
                .join("debug/.fingerprint/demo-core-0a78db7edc06e08a")
                .exists()
        );
        assert!(target.join(LAST_WORKSPACE_MARKER).is_file());
        Ok(())
    }

    // Risk (#527): Projects without a root Cargo.toml (e.g. Tauri `src-tauri/` layouts)
    // or with newly added crates not yet written to Cargo.lock must still discover all
    // local packages so their fingerprints are invalidated on workspace switches.
    #[test]
    fn discovers_local_packages_from_tauri_and_lockless_manifests() -> Result<(), Box<dyn Error>> {
        let scratch = tempdir()?;
        let root = scratch.path().join("tauri-project");
        write_file(
            &root.join("src-tauri").join("Cargo.toml"),
            r#"[package]
name = "tauri-demo"
version = "0.1.0"
edition = "2024"

[dependencies]
helper-lib = { path = "../helper" }
"#,
        )?;
        write_file(
            &root.join("helper").join("Cargo.toml"),
            r#"[package]
name = "helper-lib"
version = "0.1.0"
edition = "2024"
"#,
        )?;
        let packages = collect_local_packages(&root, &root);
        assert!(packages.contains("tauri-demo"));
        assert!(packages.contains("helper-lib"));
        Ok(())
    }

    // Risk (#682): pooling targets only avoids local-crate fingerprint thrashing when a
    // caller prefers its own matching target, then an unclaimed target, then the least
    // recently used other workspace's target.
    #[test]
    fn rank_targets_prefers_affinity_then_unclaimed_then_lru() -> Result<(), Box<dyn Error>> {
        use std::time::{Duration, SystemTime};

        let scratch = tempdir()?;
        let workspace_a = scratch.path().join("issue-a");
        let workspace_b = scratch.path().join("issue-b");
        let workspace_c = scratch.path().join("issue-c");
        seed_workspace(&workspace_a, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")?;
        seed_workspace(&workspace_b, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")?;
        seed_workspace(&workspace_c, "cccccccccccccccccccccccccccccccc")?;

        let target_0 = scratch.path().join(".velocity-agent-target");
        let target_1 = scratch.path().join(".velocity-agent-target-1");
        let targets = vec![target_0.clone(), target_1.clone()];

        let id_a = super::workspace_identity(&workspace_a);
        let id_b = super::workspace_identity(&workspace_b);
        let id_c = super::workspace_identity(&workspace_c);

        // Fresh project: slot 0 first.
        assert_eq!(
            super::rank_targets(&targets, &id_a),
            vec![target_0.clone(), target_1.clone()]
        );

        // After A uses slot 0, B prefers unclaimed slot 1 rather than clobbering A.
        prepare(&target_0, &workspace_a, &workspace_a)?;
        assert_eq!(
            super::rank_targets(&targets, &id_a),
            vec![target_0.clone(), target_1.clone()]
        );
        assert_eq!(
            super::rank_targets(&targets, &id_b),
            vec![target_1.clone(), target_0.clone()]
        );

        // After B uses slot 1, each workspace prefers its own warm slot.
        prepare(&target_1, &workspace_b, &workspace_b)?;
        assert_eq!(
            super::rank_targets(&targets, &id_a),
            vec![target_0.clone(), target_1.clone()]
        );
        assert_eq!(
            super::rank_targets(&targets, &id_b),
            vec![target_1.clone(), target_0.clone()]
        );

        // With explicit marker mtimes (slot 0 newer than slot 1), workspace C evicts LRU slot 1.
        let now = SystemTime::now();
        let older = now.checked_sub(Duration::from_secs(120)).unwrap_or(now);
        std::fs::OpenOptions::new()
            .write(true)
            .open(target_0.join(LAST_WORKSPACE_MARKER))?
            .set_modified(now)?;
        std::fs::OpenOptions::new()
            .write(true)
            .open(target_1.join(LAST_WORKSPACE_MARKER))?
            .set_modified(older)?;
        assert_eq!(
            super::rank_targets(&targets, &id_c),
            vec![target_1, target_0]
        );
        Ok(())
    }

    fn make_scope(base: &str, touched: &[&str], manifests_changed: bool) -> super::WorkspaceScope {
        super::WorkspaceScope {
            base: base.to_owned(),
            touched_packages: touched.iter().map(|&name| name.to_owned()).collect(),
            manifests_changed,
        }
    }

    // Risk (#737): switching workspaces on the same Rosen base with unchanged manifests
    // must invalidate only `touched(previous) ∪ touched(incoming)` so untouched upstream
    // crates and build scripts stay warm while touched crates are rebuilt cleanly.
    #[test]
    fn same_base_switch_invalidates_only_union_of_touched_packages() -> Result<(), Box<dyn Error>> {
        let scratch = tempdir()?;
        let workspace_a = scratch.path().join("issue-a");
        let workspace_b = scratch.path().join("issue-b");
        let target = scratch.path().join(".velocity-agent-target");
        seed_workspace(&workspace_a, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")?;
        seed_workspace(&workspace_b, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")?;

        let base = "1111111111111111111111111111111111111111";
        let scope_a = make_scope(base, &["demo-core"], false);
        let scope_b = make_scope(base, &["demo-extra"], false);

        super::prepare_with_scope(&target, &workspace_a, &workspace_a, Some(&scope_a))?;
        seed_target_entry(&target, "debug/.fingerprint/demo-core-0a78db7edc06e08a")?;
        seed_target_entry(&target, "debug/build/demo-core-0a78db7edc06e08a")?;
        seed_target_entry(&target, "debug/.fingerprint/demo-extra-1111222233334444")?;
        seed_target_entry(&target, "debug/build/demo-extra-1111222233334444")?;
        seed_target_entry(
            &target,
            "debug/.fingerprint/demo-untouched-5555666677778888",
        )?;
        seed_target_entry(&target, "debug/build/demo-untouched-5555666677778888")?;
        seed_target_entry(&target, "debug/.fingerprint/serde-53fb7d8c4a37260a")?;

        super::prepare_with_scope(&target, &workspace_b, &workspace_b, Some(&scope_b))?;
        for removed in [
            "debug/.fingerprint/demo-core-0a78db7edc06e08a",
            "debug/build/demo-core-0a78db7edc06e08a",
            "debug/.fingerprint/demo-extra-1111222233334444",
            "debug/build/demo-extra-1111222233334444",
        ] {
            assert!(
                !target.join(removed).exists(),
                "should invalidate: {removed}"
            );
        }
        for kept in [
            "debug/.fingerprint/demo-untouched-5555666677778888",
            "debug/build/demo-untouched-5555666677778888",
            "debug/.fingerprint/serde-53fb7d8c4a37260a",
        ] {
            assert!(target.join(kept).is_dir(), "should stay warm: {kept}");
        }
        Ok(())
    }

    // Risk (#737): edits added or reverted across consecutive runs in the same workspace
    // must keep the union of touched packages in the marker so a later workspace switch
    // never inherits stale artifacts from an earlier run in the previous workspace.
    #[test]
    fn same_workspace_runs_accumulate_touched_packages_across_edits() -> Result<(), Box<dyn Error>>
    {
        let scratch = tempdir()?;
        let workspace_a = scratch.path().join("issue-a");
        let workspace_b = scratch.path().join("issue-b");
        let target = scratch.path().join(".velocity-agent-target");
        seed_workspace(&workspace_a, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")?;
        seed_workspace(&workspace_b, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")?;

        let base = "1111111111111111111111111111111111111111";
        super::prepare_with_scope(
            &target,
            &workspace_a,
            &workspace_a,
            Some(&make_scope(base, &["demo-core"], false)),
        )?;
        seed_target_entry(&target, "debug/.fingerprint/demo-core-0a78db7edc06e08a")?;
        seed_target_entry(&target, "debug/.fingerprint/demo-extra-1111222233334444")?;
        seed_target_entry(
            &target,
            "debug/.fingerprint/demo-untouched-5555666677778888",
        )?;

        // Second run in A also touches demo-extra; third run reverts demo-extra.
        super::prepare_with_scope(
            &target,
            &workspace_a,
            &workspace_a,
            Some(&make_scope(base, &["demo-core", "demo-extra"], false)),
        )?;
        super::prepare_with_scope(
            &target,
            &workspace_a,
            &workspace_a,
            Some(&make_scope(base, &["demo-core"], false)),
        )?;
        assert!(
            target
                .join("debug/.fingerprint/demo-extra-1111222233334444")
                .is_dir()
        );

        // Switching to B on the same base must still invalidate demo-extra.
        super::prepare_with_scope(
            &target,
            &workspace_b,
            &workspace_b,
            Some(&make_scope(base, &[], false)),
        )?;
        assert!(
            !target
                .join("debug/.fingerprint/demo-core-0a78db7edc06e08a")
                .exists()
        );
        assert!(
            !target
                .join("debug/.fingerprint/demo-extra-1111222233334444")
                .exists()
        );
        assert!(
            target
                .join("debug/.fingerprint/demo-untouched-5555666677778888")
                .is_dir()
        );
        Ok(())
    }

    // Risk (#737): a base change or manifest/lockfile edit on either the previous or incoming
    // workspace must fall back to invalidating every local package to preserve soundness.
    #[test]
    fn base_change_or_manifest_edit_falls_back_to_full_invalidation() -> Result<(), Box<dyn Error>>
    {
        let scratch = tempdir()?;
        let workspace_a = scratch.path().join("issue-a");
        let workspace_b = scratch.path().join("issue-b");
        let workspace_c = scratch.path().join("issue-c");
        let target = scratch.path().join(".velocity-agent-target");
        seed_workspace(&workspace_a, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")?;
        seed_workspace(&workspace_b, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")?;
        seed_workspace(&workspace_c, "cccccccccccccccccccccccccccccccc")?;

        let base_1 = "1111111111111111111111111111111111111111";
        let base_2 = "2222222222222222222222222222222222222222";
        let extra = "debug/.fingerprint/demo-extra-1111222233334444";

        // Different base falls back to full invalidation, then records the new base.
        super::prepare_with_scope(
            &target,
            &workspace_a,
            &workspace_a,
            Some(&make_scope(base_1, &["demo-core"], false)),
        )?;
        seed_target_entry(&target, extra)?;
        super::prepare_with_scope(
            &target,
            &workspace_b,
            &workspace_b,
            Some(&make_scope(base_2, &["demo-core"], false)),
        )?;
        assert!(!target.join(extra).exists());

        // Incoming manifest change falls back to full invalidation and marks target ineligible.
        seed_target_entry(&target, extra)?;
        super::prepare_with_scope(
            &target,
            &workspace_c,
            &workspace_c,
            Some(&make_scope(base_2, &["demo-core"], true)),
        )?;
        assert!(!target.join(extra).exists());

        // Subsequent switch away from a manifest-changed workspace also does full invalidation.
        seed_target_entry(&target, extra)?;
        super::prepare_with_scope(
            &target,
            &workspace_b,
            &workspace_b,
            Some(&make_scope(base_2, &["demo-core"], false)),
        )?;
        assert!(!target.join(extra).exists());
        Ok(())
    }

    // Risk (#737): a same-workspace base move, unknown scope, or legacy plain-text marker
    // must clear scoped eligibility and fall back to full local-crate invalidation.
    #[test]
    fn same_workspace_rebase_unknown_or_legacy_marker_falls_back_to_full_invalidation()
    -> Result<(), Box<dyn Error>> {
        let scratch = tempdir()?;
        let workspace_a = scratch.path().join("issue-a");
        let workspace_b = scratch.path().join("issue-b");
        let target = scratch.path().join(".velocity-agent-target");
        seed_workspace(&workspace_a, "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa")?;
        seed_workspace(&workspace_b, "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb")?;

        let base_1 = "1111111111111111111111111111111111111111";
        let base_2 = "2222222222222222222222222222222222222222";
        let extra = "debug/.fingerprint/demo-extra-1111222233334444";

        // Same workspace moving base clears its scope so the next switch invalidates all.
        super::prepare_with_scope(
            &target,
            &workspace_b,
            &workspace_b,
            Some(&make_scope(base_2, &["demo-core"], false)),
        )?;
        seed_target_entry(&target, extra)?;
        super::prepare_with_scope(
            &target,
            &workspace_b,
            &workspace_b,
            Some(&make_scope(base_1, &["demo-core"], false)),
        )?;
        assert!(target.join(extra).is_dir());
        super::prepare_with_scope(
            &target,
            &workspace_a,
            &workspace_a,
            Some(&make_scope(base_1, &["demo-core"], false)),
        )?;
        assert!(!target.join(extra).exists());

        // Unknown incoming scope falls back to full invalidation.
        seed_target_entry(&target, extra)?;
        super::prepare_with_scope(&target, &workspace_b, &workspace_b, None)?;
        assert!(!target.join(extra).exists());

        // Legacy plain-text marker written by older builds falls back to full invalidation.
        write_file(
            &target.join(LAST_WORKSPACE_MARKER),
            &super::workspace_identity(&workspace_a),
        )?;
        seed_target_entry(&target, extra)?;
        super::prepare_with_scope(
            &target,
            &workspace_b,
            &workspace_b,
            Some(&make_scope(base_1, &["demo-core"], false)),
        )?;
        assert!(!target.join(extra).exists());
        Ok(())
    }

    // Risk (#737): misclassifying manifest, lockfile, Cargo config, or unowned Rust edits as
    // crate-scoped would leave stale local artifacts across workspace switches.
    #[test]
    fn scope_from_changed_paths_maps_crates_and_detects_workspace_inputs() {
        use velocity_core::preflight::WorkspaceCrate;

        let crates = vec![
            WorkspaceCrate {
                name: "demo-core".into(),
                directory: "crates/demo-core".into(),
                edition: "2024".into(),
            },
            WorkspaceCrate {
                name: "demo-app".into(),
                directory: "src-tauri".into(),
                edition: "2024".into(),
            },
        ];

        let scoped = super::scope_from_changed_paths(
            "abc123",
            &[
                "crates/demo-core/src/lib.rs",
                "ui/src/App.svelte",
                "spec/note.md",
            ],
            &crates,
        );
        assert_eq!(scoped, Some(make_scope("abc123", &["demo-core"], false)));

        for wide_input in [
            "Cargo.toml",
            "crates/demo-core/Cargo.toml",
            "Cargo.lock",
            ".cargo/config.toml",
            "rust-toolchain.toml",
            "clippy.toml",
            "rosen.toml",
            "proofs/fixtures/claim-lock.tsv",
            "ui/src/lib/theme/tokens.css",
            "outside/src/lib.rs",
        ] {
            let scope = super::scope_from_changed_paths("abc123", &[wide_input], &crates);
            assert!(
                scope.as_ref().is_some_and(|item| item.manifests_changed),
                "expected manifests_changed for {wide_input}: {scope:?}"
            );
        }

        assert!(
            super::scope_from_changed_paths("", &["crates/demo-core/src/lib.rs"], &crates)
                .is_none()
        );
        assert!(
            super::scope_from_changed_paths("abc123", &["crates/demo-core/src/lib.rs"], &[])
                .is_none()
        );
    }
}
