//! Keeps the shared light-run Cargo target folder free of stale local crate fingerprints
//! when different workspaces of a project take turns building into it (#527).
//!
//! Cargo hashes workspace members by workspace-relative path and checks source mtimes
//! against the fingerprint's timestamp, so a newer build from Workspace A causes
//! Workspace B (whose edited or untouched sources have older mtimes) to silently link
//! Workspace A's compiled crate. While holding `.velocity-light-run.lock`, we record
//! which workspace last built into the target folder and drop `.fingerprint/<pkg>-*` and
//! `build/<pkg>-*` for local packages whenever the workspace changes, keeping registry
//! and git dependency artifacts warm.

use std::collections::HashSet;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde::Deserialize;
use toml_edit::{DocumentMut, Item, Value};

const LAST_WORKSPACE_MARKER: &str = ".velocity-last-workspace";
const FINGERPRINT_HASH_LENGTH: usize = 16;
const MAX_FALLBACK_SCAN_DEPTH: usize = 2;
const MAX_GLOB_SCAN_DEPTH: usize = 3;
const SKIPPED_SCAN_FOLDERS: &[&str] = &[".git", ".rosen", "node_modules", "proofs", "target"];

#[derive(Deserialize)]
struct RosenWorkspaceRecord {
    #[serde(default)]
    change: Option<String>,
}

/// Ensures `target` holds no local-crate fingerprints or build-script outputs from a
/// different workspace. The caller must already hold `.velocity-light-run.lock`.
pub(crate) fn prepare(target: &Path, workspace_root: &Path, directory: &Path) -> io::Result<()> {
    let identity = workspace_identity(workspace_root);
    let marker = target.join(LAST_WORKSPACE_MARKER);
    if std::fs::read_to_string(&marker).ok().as_deref() == Some(identity.as_str()) {
        return Ok(());
    }
    // Clear the marker before invalidating so an interrupted cleanup never leaves the
    // target directory appearing fresh for any workspace on the next run.
    remove_file_if_exists(&marker)?;
    let packages = collect_local_packages(workspace_root, directory);
    if packages.is_empty() {
        return Ok(());
    }
    invalidate_packages(target, &packages)?;
    std::fs::create_dir_all(target)?;
    std::fs::write(&marker, identity)
}

fn workspace_identity(workspace_root: &Path) -> String {
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
}
