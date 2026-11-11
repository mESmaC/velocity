//! Machine-wide skill pack library: git clone with `core.symlinks=true`, commit pinning,
//! update diff preview, and `PYTHONUTF8=1` script execution (SPEC.md F18).

use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use crate::SkillLibraryError;
use crate::bundles::{SkillPackBundleCounts, bundle_skill_counts};
use crate::catalog::{SkillPackCatalogEntry, built_in_catalog, resolve_pack_source};
use crate::manifest::{
    DiscoveredSkill, SkillDiffCategory, classify_pack_path, discover_pack_skills,
    mark_mirrored_skills,
};

const REGISTRY_FILE_NAME: &str = "skills-library.json";
const PACKS_DIRECTORY_NAME: &str = "packs";

/// One installed and pinned skill pack in Velocity's library (SPEC.md F18).
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledSkillPack {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub source_url: String,
    #[serde(default)]
    pub built_in: bool,
    #[serde(default)]
    pub pinned_commit: String,
    #[serde(default)]
    pub short_commit: String,
    #[serde(default)]
    pub clone_path: String,
    #[serde(default)]
    pub symlinks_enabled: bool,
    #[serde(default)]
    pub skills: Vec<DiscoveredSkill>,
    /// Candidate upstream commit found by `check_skill_pack_update`, waiting for user confirmation.
    #[serde(default)]
    pub pending_update: Option<SkillPackUpdatePreview>,
}

/// Diff of a single file between the pinned commit and a candidate upstream commit (SPEC.md F18).
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillPackDiffFile {
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub category: Option<SkillDiffCategory>,
    #[serde(default)]
    pub additions: u32,
    #[serde(default)]
    pub deletions: u32,
    #[serde(default)]
    pub patch: String,
}

/// Reviewable diff between a pack's current pinned commit and a fetched candidate commit.
/// The pin never moves until the user confirms this exact `candidate_commit` (SPEC.md F18).
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillPackUpdatePreview {
    #[serde(default)]
    pub pack_id: String,
    #[serde(default)]
    pub current_commit: String,
    #[serde(default)]
    pub candidate_commit: String,
    #[serde(default)]
    pub up_to_date: bool,
    #[serde(default)]
    pub skill_markdown_files_changed: u32,
    #[serde(default)]
    pub script_files_changed: u32,
    #[serde(default)]
    pub data_files_changed: u32,
    #[serde(default)]
    pub files: Vec<SkillPackDiffFile>,
    #[serde(default)]
    pub unified_diff: String,
}

/// Snapshot of the machine-wide skill library (built-in catalog + installed pinned packs).
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillLibrarySnapshot {
    #[serde(default)]
    pub library_root: String,
    #[serde(default)]
    pub catalog: Vec<SkillPackCatalogEntry>,
    #[serde(default)]
    pub installed: Vec<InstalledSkillPack>,
    /// Real skill counts per bundle of each installed built-in pack, at its pinned commit.
    #[serde(default)]
    pub bundle_counts: Vec<SkillPackBundleCounts>,
}

/// Output of running a Python tool inside an installed skill pack with `PYTHONUTF8=1`.
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillPythonOutput {
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    #[serde(default)]
    pub exit_code: i32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct PersistedRegistry {
    #[serde(default)]
    installed: Vec<InstalledSkillPack>,
}

/// Velocity's machine-wide skill library rooted under `<app-data>/skills-library`.
#[derive(Clone, Debug)]
pub struct SkillLibrary {
    root: PathBuf,
}

impl SkillLibrary {
    pub fn open(app_data_dir: &Path) -> Result<Self, SkillLibraryError> {
        let root = app_data_dir.join("skills-library");
        fs::create_dir_all(root.join(PACKS_DIRECTORY_NAME)).map_err(|source| {
            SkillLibraryError::Io {
                path: root.clone(),
                source,
            }
        })?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Reads the current library snapshot: the built-in catalog plus all installed packs.
    pub fn snapshot(&self) -> Result<SkillLibrarySnapshot, SkillLibraryError> {
        let registry = self.load_registry()?;
        let mut catalog = built_in_catalog();
        for pack in &registry.installed {
            if !catalog.iter().any(|entry| entry.id == pack.id) {
                catalog.push(SkillPackCatalogEntry::custom(
                    pack.id.clone(),
                    pack.name.clone(),
                    pack.repository.clone(),
                ));
            }
        }
        let bundle_counts = registry
            .installed
            .iter()
            .map(bundle_skill_counts)
            .filter(|counts| !counts.bundles.is_empty())
            .collect();
        Ok(SkillLibrarySnapshot {
            library_root: self.root.display().to_string(),
            catalog,
            installed: registry.installed,
            bundle_counts,
        })
    }

    /// One installed pack from the registry.
    pub fn installed_pack(&self, pack_id: &str) -> Result<InstalledSkillPack, SkillLibraryError> {
        self.load_registry()?
            .installed
            .into_iter()
            .find(|item| item.id == pack_id)
            .ok_or_else(|| SkillLibraryError::PackNotInstalled(pack_id.to_owned()))
    }

    /// Clones a skill pack into the library with `core.symlinks=true` and `core.longpaths=true`
    /// and pins it to `initial_pin` (or the remote's `HEAD` when `None`).
    pub fn install_pack(
        &self,
        pack_or_repo: &str,
        source_override: Option<&str>,
        initial_pin: Option<&str>,
    ) -> Result<InstalledSkillPack, SkillLibraryError> {
        let (catalog_entry, remote_url) = resolve_pack_source(pack_or_repo, source_override)?;
        let clone_dir = self.pack_directory(&catalog_entry.id);
        if clone_dir.exists() {
            fs::remove_dir_all(&clone_dir).map_err(|source| SkillLibraryError::Io {
                path: clone_dir.clone(),
                source,
            })?;
        }
        clone_pack_with_symlinks(&self.root, &remote_url, &clone_dir)?;
        let pinned_commit = checkout_pinned_commit(&clone_dir, initial_pin)?;
        let symlinks_enabled = read_symlinks_config(&clone_dir);
        let skills = discover_skills(&clone_dir, &catalog_entry.name)?;
        let installed = InstalledSkillPack {
            id: catalog_entry.id,
            name: catalog_entry.name,
            repository: catalog_entry.repository,
            source_url: remote_url,
            built_in: catalog_entry.built_in,
            short_commit: short_sha(&pinned_commit),
            pinned_commit,
            clone_path: clone_dir.display().to_string(),
            symlinks_enabled,
            skills,
            pending_update: None,
        };
        self.upsert_installed(installed.clone())?;
        Ok(installed)
    }

    /// Fetches new commits from `origin` without moving the pack's pinned checkout, and builds
    /// the diff of `SKILL.md` files, scripts, and data between the pinned commit and the
    /// fetched candidate commit (SPEC.md F18).
    pub fn check_for_updates(
        &self,
        pack_id: &str,
    ) -> Result<SkillPackUpdatePreview, SkillLibraryError> {
        let mut registry = self.load_registry()?;
        let pack = registry
            .installed
            .iter_mut()
            .find(|item| item.id == pack_id)
            .ok_or_else(|| SkillLibraryError::PackNotInstalled(pack_id.to_owned()))?;
        let clone_dir = PathBuf::from(&pack.clone_path);
        let head_before = resolve_commit(&clone_dir, "HEAD")?;
        run_git(&clone_dir, ["fetch", "--quiet", "origin"])?;
        let candidate_commit = resolve_fetched_candidate(&clone_dir, &head_before)?;
        let preview = build_update_preview(&clone_dir, pack_id, &head_before, &candidate_commit)?;
        pack.pending_update = (!preview.up_to_date).then(|| preview.clone());
        self.save_registry(&registry)?;
        Ok(preview)
    }

    /// Moves a pack's pin to `expected_candidate_commit` only after its update diff was
    /// previewed and matches `expected_candidate_commit` (SPEC.md F18: "A pack's pin moves
    /// only after its diff is shown, and only when you confirm").
    pub fn confirm_update(
        &self,
        pack_id: &str,
        expected_candidate_commit: &str,
    ) -> Result<InstalledSkillPack, SkillLibraryError> {
        let mut registry = self.load_registry()?;
        let pack = registry
            .installed
            .iter_mut()
            .find(|item| item.id == pack_id)
            .ok_or_else(|| SkillLibraryError::PackNotInstalled(pack_id.to_owned()))?;
        let pending = pack
            .pending_update
            .as_ref()
            .ok_or_else(|| SkillLibraryError::UpdateDiffNotReviewed(pack_id.to_owned()))?;
        let resolved_expected = expected_candidate_commit.trim();
        if !commits_match(&pending.candidate_commit, resolved_expected) {
            return Err(SkillLibraryError::CandidateCommitMismatch {
                expected: pending.candidate_commit.clone(),
                provided: resolved_expected.to_owned(),
            });
        }
        let clone_dir = PathBuf::from(&pack.clone_path);
        let pinned_commit = checkout_pinned_commit(&clone_dir, Some(&pending.candidate_commit))?;
        pack.pinned_commit = pinned_commit.clone();
        pack.short_commit = short_sha(&pinned_commit);
        pack.skills = discover_skills(&clone_dir, &pack.name)?;
        pack.pending_update = None;
        let updated = pack.clone();
        self.save_registry(&registry)?;
        Ok(updated)
    }

    /// Runs a Python script inside an installed pack with `PYTHONUTF8=1` and `PYTHONIOENCODING=utf-8`
    /// so UTF-8 data and `SKILL.md` references work identically on Windows (SPEC.md F18).
    pub fn run_pack_python_script(
        &self,
        pack_id: &str,
        relative_script: &str,
        arguments: &[String],
    ) -> Result<SkillPythonOutput, SkillLibraryError> {
        let registry = self.load_registry()?;
        let pack = registry
            .installed
            .iter()
            .find(|item| item.id == pack_id)
            .ok_or_else(|| SkillLibraryError::PackNotInstalled(pack_id.to_owned()))?;
        let clone_dir = PathBuf::from(&pack.clone_path);
        let script_path = clone_dir.join(relative_script);
        run_python_utf8(&clone_dir, &script_path, arguments)
    }

    fn pack_directory(&self, pack_id: &str) -> PathBuf {
        let folder = self.root.join(PACKS_DIRECTORY_NAME);
        let base = fs::canonicalize(&folder).unwrap_or(folder);
        base.join(pack_id)
    }

    fn registry_path(&self) -> PathBuf {
        self.root.join(REGISTRY_FILE_NAME)
    }

    fn load_registry(&self) -> Result<PersistedRegistry, SkillLibraryError> {
        let path = self.registry_path();
        if !path.is_file() {
            return Ok(PersistedRegistry::default());
        }
        let bytes = fs::read(&path).map_err(|source| SkillLibraryError::Io { path, source })?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    fn save_registry(&self, registry: &PersistedRegistry) -> Result<(), SkillLibraryError> {
        let path = self.registry_path();
        let temporary = self.root.join(".skills-library.json.tmp");
        let serialized = serde_json::to_vec_pretty(registry)?;
        fs::write(&temporary, serialized).map_err(|source| SkillLibraryError::Io {
            path: temporary.clone(),
            source,
        })?;
        fs::rename(&temporary, &path).map_err(|source| SkillLibraryError::Io { path, source })
    }

    fn upsert_installed(&self, pack: InstalledSkillPack) -> Result<(), SkillLibraryError> {
        let mut registry = self.load_registry()?;
        if let Some(existing) = registry
            .installed
            .iter_mut()
            .find(|item| item.id == pack.id)
        {
            *existing = pack;
        } else {
            registry.installed.push(pack);
        }
        self.save_registry(&registry)
    }
}

/// Clones a skill pack repository with `core.symlinks=true` and `core.longpaths=true` (SPEC.md F18).
/// If Windows rejects symlink creation during clone because Developer Mode is off, falls back to
/// cloning with `core.symlinks=true` recorded in repository config and materializes in-repo
/// pointer files so `claude-skills` mirror folders still resolve to their target directories.
fn clone_pack_with_symlinks(
    working_dir: &Path,
    remote_url: &str,
    destination: &Path,
) -> Result<(), SkillLibraryError> {
    let destination_str = strip_verbatim_prefix(destination);
    let clone_attempt = run_git(
        working_dir,
        [
            "-c",
            "core.symlinks=true",
            "-c",
            "core.longpaths=true",
            "clone",
            "--no-checkout",
            remote_url,
            &destination_str,
        ],
    );
    clone_attempt?;
    let _ = run_git(destination, ["config", "core.symlinks", "true"]);
    let _ = run_git(destination, ["config", "core.longpaths", "true"]);
    if run_git(
        destination,
        ["-c", "core.symlinks=true", "checkout", "-f", "HEAD"],
    )
    .is_err()
    {
        run_git(destination, ["checkout", "-f", "HEAD"])?;
    }
    materialize_symlink_pointers(destination)?;
    Ok(())
}

fn checkout_pinned_commit(
    clone_dir: &Path,
    target_rev: Option<&str>,
) -> Result<String, SkillLibraryError> {
    let rev = target_rev
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("HEAD");
    let commit = resolve_commit(clone_dir, rev)?;
    if run_git(
        clone_dir,
        [
            "-c",
            "core.symlinks=true",
            "checkout",
            "--detach",
            "-f",
            &commit,
        ],
    )
    .is_err()
    {
        run_git(clone_dir, ["checkout", "--detach", "-f", &commit])?;
    }
    materialize_symlink_pointers(clone_dir)?;
    resolve_commit(clone_dir, "HEAD")
}

/// Ensures git symlink entries (`120000`) inside a pack resolve as real directories or files
/// on Windows even when the OS refuses native symlink creation without elevation: if git wrote
/// a plain pointer file whose text names a relative directory inside the pack, replaces the
/// pointer file with a Windows directory junction (`mklink /J`) or copies the target file.
fn materialize_symlink_pointers(clone_dir: &Path) -> Result<(), SkillLibraryError> {
    let output = run_git(clone_dir, ["ls-files", "-s"])?;
    for line in output.lines() {
        let Some((meta, relative_path)) = line.split_once('\t') else {
            continue;
        };
        if !meta.starts_with("120000 ") {
            continue;
        }
        let link_path = clone_dir.join(relative_path);
        let Ok(metadata) = fs::symlink_metadata(&link_path) else {
            continue;
        };
        if metadata.file_type().is_symlink() || metadata.is_dir() {
            continue;
        }
        let Ok(pointer_text) = fs::read_to_string(&link_path) else {
            continue;
        };
        let target_rel = pointer_text.trim();
        if target_rel.is_empty() {
            continue;
        }
        let Some(parent) = link_path.parent() else {
            continue;
        };
        let target_path = parent.join(target_rel);
        let Ok(canonical_clone) = fs::canonicalize(clone_dir) else {
            continue;
        };
        let Ok(canonical_target) = fs::canonicalize(&target_path) else {
            continue;
        };
        if !canonical_target.starts_with(&canonical_clone) {
            continue;
        }
        let _ = fs::remove_file(&link_path);
        if canonical_target.is_dir() {
            create_directory_link(&link_path, &canonical_target)?;
        } else if canonical_target.is_file() {
            let _ = fs::copy(&canonical_target, &link_path);
        }
    }
    Ok(())
}

#[cfg(windows)]
fn create_directory_link(link_path: &Path, target_dir: &Path) -> Result<(), SkillLibraryError> {
    if std::os::windows::fs::symlink_dir(target_dir, link_path).is_ok() {
        return Ok(());
    }
    use std::os::windows::process::CommandExt;
    let link_str = strip_verbatim_prefix(link_path);
    let target_str = strip_verbatim_prefix(target_dir);
    // The link path comes from the pack's own tree. cmd.exe treats `&`, `|`, `^`, `<`, `>`
    // and parentheses in an unquoted argument as syntax, and Rust quotes only arguments with
    // spaces, so both paths are always quoted. `%` expands and `"` ends a quote even then.
    if [&link_str, &target_str]
        .iter()
        .any(|path| path.contains(['"', '%']))
    {
        return Err(SkillLibraryError::GitCommand {
            command: format!("mklink /J {link_str} {target_str}"),
            stderr: "the pack path contains a character cmd.exe cannot take literally".into(),
        });
    }
    let output = Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .raw_arg(format!("\"{link_str}\""))
        .raw_arg(format!("\"{target_str}\""))
        .output()
        .map_err(|source| SkillLibraryError::Io {
            path: link_path.to_path_buf(),
            source,
        })?;
    if !output.status.success() {
        return Err(SkillLibraryError::GitCommand {
            command: format!("mklink /J {link_str} {target_str}"),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(())
}

#[cfg(not(windows))]
fn create_directory_link(link_path: &Path, target_dir: &Path) -> Result<(), SkillLibraryError> {
    std::os::unix::fs::symlink(target_dir, link_path).map_err(|source| SkillLibraryError::Io {
        path: link_path.to_path_buf(),
        source,
    })
}

fn read_symlinks_config(clone_dir: &Path) -> bool {
    run_git(clone_dir, ["config", "--get", "core.symlinks"])
        .map(|value| value.trim().eq_ignore_ascii_case("true"))
        .unwrap_or(false)
}

fn resolve_commit(clone_dir: &Path, rev: &str) -> Result<String, SkillLibraryError> {
    let commit_ref = format!("{rev}^{{commit}}");
    let output = run_git(clone_dir, ["rev-parse", "--verify", &commit_ref])?;
    Ok(output.trim().to_owned())
}

fn resolve_fetched_candidate(
    clone_dir: &Path,
    fallback: &str,
) -> Result<String, SkillLibraryError> {
    for candidate in ["origin/HEAD", "FETCH_HEAD", "origin/main", "origin/master"] {
        if let Ok(sha) = resolve_commit(clone_dir, candidate) {
            return Ok(sha);
        }
    }
    Ok(fallback.to_owned())
}

fn build_update_preview(
    clone_dir: &Path,
    pack_id: &str,
    current_commit: &str,
    candidate_commit: &str,
) -> Result<SkillPackUpdatePreview, SkillLibraryError> {
    if current_commit == candidate_commit {
        return Ok(SkillPackUpdatePreview {
            pack_id: pack_id.to_owned(),
            current_commit: current_commit.to_owned(),
            candidate_commit: candidate_commit.to_owned(),
            up_to_date: true,
            skill_markdown_files_changed: 0,
            script_files_changed: 0,
            data_files_changed: 0,
            files: Vec::new(),
            unified_diff: String::new(),
        });
    }
    let range = format!("{current_commit}..{candidate_commit}");
    let numstat = run_git(clone_dir, ["diff", "--numstat", &range])?;
    let unified_diff = run_git(clone_dir, ["diff", "--no-color", &range])?;
    let mut files = Vec::new();
    let mut skill_md_count = 0_u32;
    let mut script_count = 0_u32;
    let mut data_count = 0_u32;

    for line in numstat.lines() {
        if let Some((diff_file, category)) = parse_numstat_file(clone_dir, &range, line) {
            match category {
                SkillDiffCategory::SkillMarkdown => skill_md_count += 1,
                SkillDiffCategory::Script => script_count += 1,
                SkillDiffCategory::Data => data_count += 1,
                SkillDiffCategory::Other => {}
            }
            files.push(diff_file);
        }
    }

    Ok(SkillPackUpdatePreview {
        pack_id: pack_id.to_owned(),
        current_commit: current_commit.to_owned(),
        candidate_commit: candidate_commit.to_owned(),
        up_to_date: false,
        skill_markdown_files_changed: skill_md_count,
        script_files_changed: script_count,
        data_files_changed: data_count,
        files,
        unified_diff,
    })
}

fn parse_numstat_file(
    clone_dir: &Path,
    range: &str,
    line: &str,
) -> Option<(SkillPackDiffFile, SkillDiffCategory)> {
    let parts: Vec<&str> = line.split('\t').collect();
    if parts.len() < 3 {
        return None;
    }
    let additions = parts[0].parse::<u32>().unwrap_or(0);
    let deletions = parts[1].parse::<u32>().unwrap_or(0);
    let path = parts[2].to_owned();
    let category = classify_pack_path(&path);
    let patch = run_git(clone_dir, ["diff", "--no-color", range, "--", &path]).unwrap_or_default();
    Some((
        SkillPackDiffFile {
            path,
            category: Some(category),
            additions,
            deletions,
            patch,
        },
        category,
    ))
}

fn commits_match(candidate: &str, provided: &str) -> bool {
    !provided.is_empty()
        && (candidate.eq_ignore_ascii_case(provided)
            || (provided.len() >= 7
                && candidate
                    .to_ascii_lowercase()
                    .starts_with(&provided.to_ascii_lowercase())))
}

fn discover_skills(
    clone_dir: &Path,
    name: &str,
) -> Result<Vec<DiscoveredSkill>, SkillLibraryError> {
    let mut skills = discover_pack_skills(clone_dir, name)?;
    mark_mirrored_skills(&mut skills, &tracked_links(clone_dir)?);
    Ok(skills)
}

/// Paths the pinned commit tracks as symbolic links (mode `120000`). They stay links in the
/// index even where Windows checked them out as folders or junctions.
fn tracked_links(clone_dir: &Path) -> Result<Vec<String>, SkillLibraryError> {
    // `-z` keeps non-ASCII paths unquoted.
    let listing = run_git(clone_dir, ["ls-files", "-s", "-z"])?;
    Ok(listing
        .split('\0')
        .filter(|line| line.starts_with("120000 "))
        .filter_map(|line| line.split_once('\t').map(|(_, path)| path.to_owned()))
        .collect())
}

fn short_sha(commit: &str) -> String {
    commit.chars().take(7).collect()
}

fn strip_verbatim_prefix(path: &Path) -> String {
    let raw = path.to_string_lossy();
    raw.strip_prefix(r"\\?\").unwrap_or(&raw).to_owned()
}

pub(crate) fn run_git<I, S>(working_dir: &Path, arguments: I) -> Result<String, SkillLibraryError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let args: Vec<String> = arguments
        .into_iter()
        .map(|arg| arg.as_ref().to_string_lossy().into_owned())
        .collect();
    let output = Command::new("git")
        .current_dir(working_dir)
        .args(["-c", "core.longpaths=true"])
        .args(&args)
        .output()
        .map_err(|source| SkillLibraryError::Io {
            path: working_dir.to_path_buf(),
            source,
        })?;
    if !output.status.success() {
        return Err(SkillLibraryError::GitCommand {
            command: format!("git {}", args.join(" ")),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Runs a Python script with `PYTHONUTF8=1` (SPEC.md F18: "their Python runs with `PYTHONUTF8=1`").
pub fn run_python_utf8(
    working_dir: &Path,
    script_path: &Path,
    arguments: &[String],
) -> Result<SkillPythonOutput, SkillLibraryError> {
    let script_str = strip_verbatim_prefix(script_path);
    for candidate in ["python", "python3", "py"] {
        let mut command = Command::new(candidate);
        command
            .current_dir(working_dir)
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .arg(&script_str)
            .args(arguments);
        match command.output() {
            Ok(output) => {
                return Ok(SkillPythonOutput {
                    stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
                    exit_code: output.status.code().unwrap_or(-1),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => {
                return Err(SkillLibraryError::Io {
                    path: script_path.to_path_buf(),
                    source,
                });
            }
        }
    }
    Err(SkillLibraryError::PythonNotFound)
}
