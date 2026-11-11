//! Lean setup for warm pool worktrees (SPEC.md F5 Warm pool → Lean): install the project's
//! pinned Lean toolchain through elan and fetch Mathlib's prebuilt files with
//! `lake exe cache get`, so an agent's first `lake build` compiles only the project's own
//! proofs and never Mathlib.
//!
//! Mathlib's cache tool keeps every file it downloads in one machine-wide folder
//! (`~/.cache/mathlib`, or `MATHLIB_CACHE_DIR`), so only the first worktree on a machine downloads Mathlib; later ones
//! unpack from that shared cache. Lake also clones every dependency into each worktree's
//! `.lake/packages`, and Mathlib's history is large, so before Lake runs, each missing
//! package is cloned locally from a checkout of the same repository that already has the
//! pinned commit. Lake then finds it at the right revision and fetches nothing.
//!
//! The decisions (which folders are Lake projects, which toolchain, which packages) are the
//! pure functions at the top; the process edge follows.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

use crate::command::git;
use crate::manager::WorktreeError;
use crate::ready_pool::background_priority;

const TOOLCHAIN_FILE: &str = "lean-toolchain";
const MANIFEST_FILE: &str = "lake-manifest.json";
const LAKEFILES: [&str; 2] = ["lakefile.lean", "lakefile.toml"];
/// Lake's default when a manifest names no `packagesDir`.
const DEFAULT_PACKAGES_DIR: &str = ".lake/packages";
/// The package whose prebuilt files `lake exe cache get` fetches.
const MATHLIB: &str = "mathlib";

/// One git dependency pinned in a `lake-manifest.json`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LakePackage {
    pub name: String,
    pub url: String,
    pub rev: String,
}

/// What a Lake project's manifest pins: where packages live and which git packages at which
/// commits.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LakeManifest {
    pub packages_dir: String,
    pub packages: Vec<LakePackage>,
}

impl LakeManifest {
    /// Whether the project builds on Mathlib, so `lake exe cache get` applies.
    pub fn uses_mathlib(&self) -> bool {
        self.packages.iter().any(|package| package.name == MATHLIB)
    }
}

#[derive(Deserialize)]
struct ManifestFile {
    #[serde(default, rename = "packagesDir")]
    packages_dir: Option<String>,
    #[serde(default)]
    packages: Vec<ManifestPackage>,
}

#[derive(Deserialize)]
struct ManifestPackage {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    rev: String,
}

/// The toolchain a `lean-toolchain` file pins (`leanprover/lean4:v4.35.0`): its first
/// non-blank line. `None` for anything that isn't a plain toolchain name, so a malformed file
/// can never pass an option to elan.
pub fn parse_toolchain(text: &str) -> Option<String> {
    let name = text.lines().map(str::trim).find(|line| !line.is_empty())?;
    let allowed =
        |character: char| character.is_ascii_alphanumeric() || "._-+:/".contains(character);
    let plain = name.len() <= 200 && !name.starts_with('-') && name.chars().all(allowed);
    plain.then(|| name.to_owned())
}

/// The manifest's packages directory and its git packages. Path packages and entries
/// without a name, URL or commit are left out: Lake resolves those itself.
pub fn parse_manifest(text: &str) -> Option<LakeManifest> {
    let file: ManifestFile = serde_json::from_str(text).ok()?;
    let packages_dir = file
        .packages_dir
        .filter(|folder| is_relative_folder(folder))
        .unwrap_or_else(|| DEFAULT_PACKAGES_DIR.to_owned());
    let packages = file
        .packages
        .into_iter()
        .filter(|package| package.kind.as_deref().is_none_or(|kind| kind == "git"))
        .filter(|package| is_plain_name(&package.name) && is_commit(&package.rev))
        .filter(|package| !package.url.is_empty() && !package.url.starts_with('-'))
        .map(|package| LakePackage {
            name: package.name,
            url: package.url,
            rev: package.rev,
        })
        .collect();
    Some(LakeManifest {
        packages_dir,
        packages,
    })
}

/// Which of a checkout's folders to set up as Lake projects: the root (`""`) and each
/// top-level folder (`proofs`), given the top-level folder names and a test for whether a
/// relative folder holds a `lean-toolchain` and a lakefile. Hidden folders are skipped: they
/// hold tool state (`.git`, `.lake`), never a project.
pub fn lake_project_folders(
    top_level_folders: &[String],
    is_lake_project: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut folders: Vec<String> = std::iter::once(String::new())
        .chain(
            top_level_folders
                .iter()
                .filter(|name| !name.starts_with('.'))
                .cloned(),
        )
        .filter(|folder| is_lake_project(folder))
        .collect();
    folders.sort();
    folders
}

fn is_plain_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "._-".contains(character))
}

fn is_commit(rev: &str) -> bool {
    (7..=64).contains(&rev.len()) && rev.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// A relative folder inside the project: no root, drive or `..` step.
fn is_relative_folder(folder: &str) -> bool {
    !folder.is_empty()
        && !folder.starts_with(['/', '\\'])
        && !folder.contains(':')
        && folder
            .split(['/', '\\'])
            .all(|step| !step.is_empty() && step != "..")
}

/// Sets up every Lake project in `worktree`: installs its toolchain, seeds its packages from
/// `sources` (other checkouts of the same repository, tried in order), and runs
/// `lake exe cache get` when it builds on Mathlib. Every project is attempted; the first
/// failure is returned. A checkout with no Lake project does nothing.
pub fn prepare_lean(worktree: &Path, sources: &[PathBuf]) -> Result<(), WorktreeError> {
    let mut first_failure = None;
    for folder in find_lake_projects(worktree) {
        let project = worktree.join(&folder);
        if let Err(error) = prepare_lake_project(&project, &folder, sources) {
            first_failure.get_or_insert(error);
        }
    }
    first_failure.map_or(Ok(()), Err)
}

fn find_lake_projects(worktree: &Path) -> Vec<String> {
    let top_level: Vec<String> = fs::read_dir(worktree)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    lake_project_folders(&top_level, |folder| {
        let project = worktree.join(folder);
        project.join(TOOLCHAIN_FILE).is_file()
            && LAKEFILES.iter().any(|name| project.join(name).is_file())
    })
}

fn prepare_lake_project(
    project: &Path,
    folder: &str,
    sources: &[PathBuf],
) -> Result<(), WorktreeError> {
    let toolchain_text = fs::read_to_string(project.join(TOOLCHAIN_FILE)).unwrap_or_default();
    let toolchain =
        parse_toolchain(&toolchain_text).ok_or_else(|| WorktreeError::CommandFailed {
            command: "read lean-toolchain".into(),
            working_directory: project.to_path_buf(),
            status: "invalid".into(),
            stderr: format!("`{}` names no toolchain", toolchain_text.trim()),
        })?;
    run_tool(project, "elan", &["toolchain", "install", &toolchain])?;
    let Some(manifest) = fs::read_to_string(project.join(MANIFEST_FILE))
        .ok()
        .and_then(|text| parse_manifest(&text))
    else {
        return Ok(());
    };
    let packages = project.join(&manifest.packages_dir);
    for package in &manifest.packages {
        let source_folders = sources.iter().map(|source| {
            source
                .join(folder)
                .join(&manifest.packages_dir)
                .join(&package.name)
        });
        seed_package(&packages.join(&package.name), package, source_folders);
    }
    if manifest.uses_mathlib() {
        run_tool(project, "lake", &["exe", "cache", "get"])?;
    }
    Ok(())
}

/// Clones `package` into `destination` from the first source checkout that has its pinned
/// commit, at that commit and with the manifest's URL as `origin`, so Lake accepts it as
/// its own clone. Best effort: when no source has it, Lake clones it from the network.
fn seed_package(destination: &Path, package: &LakePackage, sources: impl Iterator<Item = PathBuf>) {
    if destination.exists() {
        return;
    }
    for source in sources {
        if !source.join(".git").exists() || !has_commit(&source, &package.rev) {
            continue;
        }
        if clone_at(&source, destination, package).is_ok() {
            return;
        }
        // A half-made clone would make Lake skip its own; clear it before the next source.
        let _ = fs::remove_dir_all(destination);
    }
}

fn has_commit(repository: &Path, rev: &str) -> bool {
    git()
        .current_dir(repository)
        .args(["cat-file", "-e", &format!("{rev}^{{commit}}")])
        .output()
        .is_ok_and(|output| output.status.success())
}

fn clone_at(source: &Path, destination: &Path, package: &LakePackage) -> Result<(), WorktreeError> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| WorktreeError::CreateDirectory {
            path: parent.to_path_buf(),
            source: error,
        })?;
    }
    let from = source.to_string_lossy().into_owned();
    let to = destination.to_string_lossy().into_owned();
    // `--local` hardlinks the objects when both folders are on one drive. Across drives Git
    // for Windows fails with "Improper link" instead of copying, so copy then. No `--shared`:
    // an alternates file would break when the source checkout is deleted.
    let linked = run_git(
        source,
        &["clone", "--quiet", "--local", "--no-checkout", &from, &to],
    );
    if linked.is_err() {
        let _ = fs::remove_dir_all(destination);
        run_git(
            source,
            &[
                "clone",
                "--quiet",
                "--no-hardlinks",
                "--no-checkout",
                &from,
                &to,
            ],
        )?;
    }
    run_git(
        destination,
        &["checkout", "--quiet", "--detach", &package.rev],
    )?;
    run_git(destination, &["remote", "set-url", "origin", &package.url])
}

fn run_git(directory: &Path, arguments: &[&str]) -> Result<(), WorktreeError> {
    let output = git()
        .current_dir(directory)
        .args(arguments)
        .output()
        .map_err(|source| WorktreeError::CommandSpawn {
            program: "git".into(),
            working_directory: directory.to_path_buf(),
            source,
        })?;
    checked("git", arguments, directory, &output)
}

/// Runs elan or lake below normal priority with no console window, like the Cargo prebuild.
fn run_tool(directory: &Path, program: &str, arguments: &[&str]) -> Result<(), WorktreeError> {
    let mut command = Command::new(program);
    command.current_dir(directory).args(arguments);
    background_priority(&mut command);
    let output = command
        .output()
        .map_err(|source| WorktreeError::CommandSpawn {
            program: program.into(),
            working_directory: directory.to_path_buf(),
            source,
        })?;
    checked(program, arguments, directory, &output)
}

fn checked(
    program: &str,
    arguments: &[&str],
    directory: &Path,
    output: &std::process::Output,
) -> Result<(), WorktreeError> {
    if output.status.success() {
        return Ok(());
    }
    Err(WorktreeError::CommandFailed {
        command: format!("{program} {}", arguments.join(" ")),
        working_directory: directory.to_path_buf(),
        status: output.status.to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}
