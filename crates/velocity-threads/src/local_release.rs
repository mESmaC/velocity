//! Builds a local release of one of Velocity's own landed commits (SPEC.md F1 "Local
//! builds", #512). The commit is staged in a folder of its own beside the project's
//! post-land checks, so the release `target` and the UI's dependencies are reused from one
//! build to the next, and `scripts/release-local.ps1` runs there as one background turn in
//! the machine's run queue: at low priority, behind landing checks and agents' own builds.

use std::path::{Path, PathBuf};

use velocity_collapse::UpdaterSigningKey;
use velocity_rosen::checker::{self, FailedCommand, Priority};
use velocity_rosen::{ProjectServer, post_land};
use velocity_store::duty::LandingProject;

use crate::launch::Location;

/// The project's folder the releases build in, beside its agents' workspaces.
const BUILD_FOLDER: &str = "local-release";
const SCRIPT: &str = "scripts/release-local.ps1";
/// How much of a failed command's output the reason keeps.
const REASON_LINES: usize = 12;
const ROSEN_WINDOWS_TARGET: &str = "x86_64-pc-windows-msvc";

/// One build: the landed commit, the version it is given and where its downloads go.
pub struct LocalRelease {
    pub commit: String,
    pub version: String,
    pub out_dir: PathBuf,
}

/// A signed local release cut on this machine for publishing to Collapse (#593).
pub struct SignedRelease {
    pub commit: String,
    pub version: String,
    pub out_dir: PathBuf,
    /// Build the Linux `velocity-graph` archive alongside Windows downloads.
    pub build_graph_indexer: bool,
}

/// Every built and signed artifact produced by [`build_signed`] (#593).
#[derive(Clone, Debug)]
pub struct BuiltReleaseAssets {
    pub setup_exe: PathBuf,
    pub setup_sig: PathBuf,
    pub portable_exe: PathBuf,
    pub portable_sig: PathBuf,
    pub msi: Option<PathBuf>,
    pub msi_sig: Option<PathBuf>,
    pub sha256sums: PathBuf,
    pub notes: String,
    pub graph_indexer_archive: Option<PathBuf>,
    pub graph_indexer_sha256: Option<PathBuf>,
}

/// Rosen pin release binaries and metadata built from source on this machine (#561, #593).
#[derive(Clone, Debug)]
pub struct BuiltRosenPinAssets {
    pub rosen_exe: PathBuf,
    pub rosen_server_exe: PathBuf,
    pub commit_file: PathBuf,
    pub sha256sums: PathBuf,
}

/// Stages `release.commit` and builds it. An error is the reason to show: the commit could
/// not be staged, it has no release script, or a command failed.
pub async fn build(
    app_data: &Path,
    project: &LandingProject,
    release: &LocalRelease,
) -> Result<(), String> {
    if !cfg!(windows) {
        return Err("Local builds are made on Windows only".into());
    }
    let folder = Location::new(&project.project, &project.worktrees)
        .project_root()
        .map_err(|error| error.to_string())?
        .join(BUILD_FOLDER);
    let server = ProjectServer::new(app_data, project.project.id);
    post_land::stage_commit(&server, &release.commit, &folder)
        .await
        .map_err(|error| error.to_string())?;
    let commands = commands(release);
    tokio::task::spawn_blocking(move || run(&folder, &commands, &[]))
        .await
        .map_err(|error| error.to_string())?
}

/// Stages `release.commit`, builds and signs Velocity on this machine using `signing_key`
/// from Windows Credential Manager (never logging the key or exposing it to agents), and
/// returns the built artifacts ready to upload to Collapse (#593).
pub async fn build_signed(
    app_data: &Path,
    project: &LandingProject,
    release: &SignedRelease,
    signing_key: &UpdaterSigningKey,
) -> Result<BuiltReleaseAssets, String> {
    if !cfg!(windows) {
        return Err("Signed local releases are built on Windows only".into());
    }
    let folder = Location::new(&project.project, &project.worktrees)
        .project_root()
        .map_err(|error| error.to_string())?
        .join(BUILD_FOLDER);
    let server = ProjectServer::new(app_data, project.project.id);
    post_land::stage_commit(&server, &release.commit, &folder)
        .await
        .map_err(|error| error.to_string())?;
    let commands = signed_commands(release);
    let key = signing_key.clone();
    let out_dir = release.out_dir.clone();
    let version = release.version.clone();
    let build_graph = release.build_graph_indexer;
    tokio::task::spawn_blocking(move || {
        let mut extra_env = vec![("TAURI_SIGNING_PRIVATE_KEY", key.private_key.as_str())];
        if let Some(password) = key.password.as_deref() {
            extra_env.push(("TAURI_SIGNING_PRIVATE_KEY_PASSWORD", password));
        }
        run(&folder, &commands, &extra_env)?;
        collect_signed_assets(&out_dir, &version, build_graph)
    })
    .await
    .map_err(|error| error.to_string())?
}

/// Builds Rosen's Windows binaries at `revision` from source into a per-revision build
/// folder (#561) and packages the exact assets Collapse hosts for a Rosen pin release (#593).
pub async fn build_rosen_pin_binaries(
    app_data: &Path,
    git_url: &str,
    revision: &str,
    out_dir: &Path,
) -> Result<BuiltRosenPinAssets, String> {
    if !cfg!(windows) {
        return Err("Rosen pin binaries are built on Windows only".into());
    }
    let revision = revision.trim().to_ascii_lowercase();
    if revision.len() != 40 || !revision.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("Rosen revision {revision} is not a 40-hex commit"));
    }
    let root = app_data.join("rosen-pin-build");
    let git_url = git_url.to_owned();
    let out_dir = out_dir.to_path_buf();
    tokio::task::spawn_blocking(move || {
        build_rosen_pin_blocking(&root, &git_url, &revision, &out_dir)
    })
    .await
    .map_err(|error| error.to_string())?
}

/// The staged tree has no `ui/node_modules` the first time, and may need new ones later.
fn commands(release: &LocalRelease) -> Vec<String> {
    let out_dir = release.out_dir.to_string_lossy();
    // A trailing backslash would escape the closing quote.
    let out_dir = out_dir.trim_end_matches('\\');
    vec![
        "pnpm -C ui install --frozen-lockfile".to_owned(),
        format!(
            "pwsh -NoProfile -ExecutionPolicy Bypass -File scripts\\release-local.ps1 -Version {} -Commit {} -OutDir \"{out_dir}\"",
            release.version, release.commit
        ),
    ]
}

fn signed_commands(release: &SignedRelease) -> Vec<String> {
    let out_dir = release.out_dir.to_string_lossy();
    let out_dir = out_dir.trim_end_matches('\\');
    let graph_flag = if release.build_graph_indexer {
        " -GraphIndexer"
    } else {
        ""
    };
    vec![
        "pnpm -C ui install --frozen-lockfile".to_owned(),
        format!(
            "pwsh -NoProfile -ExecutionPolicy Bypass -File scripts\\release-local.ps1 -Version {} -Commit {} -OutDir \"{out_dir}\" -Sign{graph_flag}",
            release.version, release.commit
        ),
    ]
}

fn run(folder: &Path, commands: &[String], extra_env: &[(&str, &str)]) -> Result<(), String> {
    checker::apply_staged(folder).map_err(|error| error.to_string())?;
    if !folder.join(SCRIPT).is_file() {
        return Err(format!("This commit has no {SCRIPT}"));
    }
    match checker::run_commands_with_env(folder, commands, Priority::Background, extra_env) {
        Ok(None) => Ok(()),
        Ok(Some(failed)) => Err(reason(&failed)),
        Err(error) => Err(error.to_string()),
    }
}

fn collect_signed_assets(
    out_dir: &Path,
    version: &str,
    expect_graph: bool,
) -> Result<BuiltReleaseAssets, String> {
    let require = |name: &str| -> Result<PathBuf, String> {
        let path = out_dir.join(name);
        if path.is_file() {
            Ok(path)
        } else {
            Err(format!("Signed release build did not produce {name}"))
        }
    };
    let setup_name = format!("Velocity_{version}_x64-setup.exe");
    let portable_name = format!("Velocity_{version}_x64-portable.exe");
    let setup_exe = require(&setup_name)?;
    let setup_sig = require(&format!("{setup_name}.sig"))?;
    let portable_exe = require(&portable_name)?;
    let portable_sig = require(&format!("{portable_name}.sig"))?;
    let sha256sums = require("SHA256SUMS.txt")?;
    let msi_name = format!("Velocity_{version}_x64_en-US.msi");
    let msi_path = out_dir.join(&msi_name);
    let (msi, msi_sig) = if msi_path.is_file() {
        (Some(msi_path), Some(require(&format!("{msi_name}.sig"))?))
    } else {
        (None, None)
    };
    let archive_name = velocity_core::graph_indexer_pin::archive_name(version);
    let checksum_name = velocity_core::graph_indexer_pin::checksum_name(version);
    let archive_path = out_dir.join(&archive_name);
    let checksum_path = out_dir.join(&checksum_name);
    let (graph_indexer_archive, graph_indexer_sha256) = if archive_path.is_file() {
        if !checksum_path.is_file() {
            return Err(format!(
                "Built {archive_name} without its checksum file {checksum_name}"
            ));
        }
        (Some(archive_path), Some(checksum_path))
    } else if expect_graph {
        return Err(format!(
            "Linux velocity-graph archive {archive_name} was not produced (check WSL/Docker)"
        ));
    } else {
        (None, None)
    };
    let notes = std::fs::read_to_string(out_dir.join("RELEASE-NOTES.md"))
        .unwrap_or_else(|_| format!("Velocity {version}"));
    Ok(BuiltReleaseAssets {
        setup_exe,
        setup_sig,
        portable_exe,
        portable_sig,
        msi,
        msi_sig,
        sha256sums,
        notes,
        graph_indexer_archive,
        graph_indexer_sha256,
    })
}

fn build_rosen_pin_blocking(
    root: &Path,
    git_url: &str,
    revision: &str,
    out_dir: &Path,
) -> Result<BuiltRosenPinAssets, String> {
    let build_root = root.join("target");
    let target_dir = build_root.join(revision);
    let install_dir = root.join("bin").join(revision);
    let bin_dir = install_dir.join("bin");
    std::fs::create_dir_all(&build_root)
        .map_err(|error| format!("Could not create Rosen build folder: {error}"))?;
    std::fs::create_dir_all(out_dir)
        .map_err(|error| format!("Could not create Rosen release output folder: {error}"))?;
    // Remove older per-revision build directories so disk stays bounded while preventing
    // stale workspace-member artifacts across pins (#561).
    if let Ok(entries) = std::fs::read_dir(&build_root) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy() != revision {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    let installed_cli = bin_dir.join("rosen.exe");
    let installed_server = bin_dir.join("rosen-server.exe");
    if !(installed_cli.is_file() && installed_server.is_file()) {
        let install_text = install_dir.to_string_lossy();
        let install_text = install_text.trim_end_matches('\\');
        let target_text = target_dir.to_string_lossy();
        let target_text = target_text.trim_end_matches('\\');
        let command = format!(
            "cargo install --locked --git \"{git_url}\" --rev {revision} --root \"{install_text}\" rosen-server rosen-cli"
        );
        let extra_env = [("CARGO_TARGET_DIR", target_text)];
        if let Some(failed) =
            checker::run_commands_with_env(root, &[command], Priority::Background, &extra_env)
                .map_err(|error| error.to_string())?
        {
            return Err(reason(&failed));
        }
    }
    let verify_commands = [
        format!("\"{}\" --version", installed_cli.display()),
        format!("\"{}\" --version", installed_server.display()),
    ];
    if let Some(failed) = checker::run_commands(root, &verify_commands, Priority::Background)
        .map_err(|error| error.to_string())?
    {
        return Err(reason(&failed));
    }
    let rosen_asset = format!("rosen-{ROSEN_WINDOWS_TARGET}.exe");
    let server_asset = format!("rosen-server-{ROSEN_WINDOWS_TARGET}.exe");
    let rosen_exe = out_dir.join(&rosen_asset);
    let rosen_server_exe = out_dir.join(&server_asset);
    std::fs::copy(&installed_cli, &rosen_exe)
        .map_err(|error| format!("Could not copy rosen.exe: {error}"))?;
    std::fs::copy(&installed_server, &rosen_server_exe)
        .map_err(|error| format!("Could not copy rosen-server.exe: {error}"))?;
    let rosen_bytes = std::fs::read(&rosen_exe)
        .map_err(|error| format!("Could not read {rosen_asset}: {error}"))?;
    let server_bytes = std::fs::read(&rosen_server_exe)
        .map_err(|error| format!("Could not read {server_asset}: {error}"))?;
    let sums = format!(
        "{}  {rosen_asset}\n{}  {server_asset}\n",
        velocity_collapse::releases::sha256_hex(&rosen_bytes),
        velocity_collapse::releases::sha256_hex(&server_bytes),
    );
    let commit_file = out_dir.join("rosen-client-commit.txt");
    let sha256sums = out_dir.join("SHA256SUMS.txt");
    std::fs::write(&commit_file, format!("{revision}\n"))
        .map_err(|error| format!("Could not write rosen-client-commit.txt: {error}"))?;
    std::fs::write(&sha256sums, sums)
        .map_err(|error| format!("Could not write SHA256SUMS.txt: {error}"))?;
    Ok(BuiltRosenPinAssets {
        rosen_exe,
        rosen_server_exe,
        commit_file,
        sha256sums,
    })
}

/// The command, how it ended and the end of what it printed.
fn reason(failed: &FailedCommand) -> String {
    let ended = failed
        .exit_code
        .map_or_else(|| "timed out".to_owned(), |code| format!("exited {code}"));
    let lines: Vec<&str> = failed
        .output
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect();
    let tail = lines[lines.len().saturating_sub(REASON_LINES)..].join("\n");
    format!("`{}` {ended}\n{tail}", failed.command)
        .trim_end()
        .to_owned()
}
