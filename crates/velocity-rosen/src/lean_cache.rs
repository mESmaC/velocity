//! On-demand Lean / Mathlib cache preparation for agent workspaces (#546).
//!
//! Both `queue_run` (`velocity-acp`) and the agent shell's `lake` shim (`agent_run`) use
//! this module so workspaces that never touch `proofs/` carry no `proofs/.lake`, while the
//! first `lake` build unpacks Mathlib's prebuilt cache instead of compiling Mathlib.

use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;

use velocity_core::lean_cache::{
    MATHLIB_CACHE_STAMP, MATHLIB_OLEAN_RELATIVE, inspect_lake_invocation, is_lake_program,
    mathlib_cache_current,
};

use crate::agent_run::{
    copy_launcher, executable_file, filter_environment, real_program, toolchain_path,
};
use crate::process::background_command;

const TOOLCHAIN_FILE: &str = "lean-toolchain";
const MANIFEST_FILE: &str = "lake-manifest.json";
const LAKEFILES: [&str; 2] = ["lakefile.lean", "lakefile.toml"];
const MATHLIB: &str = "mathlib";

/// A Lake project whose Mathlib cache must be unpacked before `lake` builds or elaborates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingMathlibCache {
    pub project: PathBuf,
    pub rev: String,
}

/// Whether `program` names the `lake` executable.
pub fn is_lake(program: &str) -> bool {
    is_lake_program(program)
}

/// Finds the nearest Lake project enclosing `start` (without walking above `stop_at`, or
/// above the nearest `.rosen`/`.git` workspace root when `stop_at` is `None`).
pub fn find_lake_project(start: &Path, stop_at: Option<&Path>) -> Option<PathBuf> {
    let limit = stop_at.map(Path::to_path_buf).or_else(|| {
        start
            .ancestors()
            .find(|path| path.join(".rosen").exists() || path.join(".git").exists())
            .map(Path::to_path_buf)
    });
    for directory in start.ancestors() {
        if is_lake_project(directory) {
            return Some(directory.to_path_buf());
        }
        if limit.as_deref().is_some_and(|root| directory == root) {
            break;
        }
    }
    None
}

/// Returns the Lake project and pinned Mathlib revision when running `lake` with `arguments`
/// in `working_directory` requires `lake exe cache get` first.
pub fn pending_mathlib_cache(
    working_directory: &Path,
    workspace_root: Option<&Path>,
    arguments: &[String],
) -> Option<PendingMathlibCache> {
    let invocation = inspect_lake_invocation(arguments);
    if !invocation.needs_cache {
        return None;
    }
    let start = match invocation.project_dir {
        Some(directory) => working_directory.join(directory),
        None => working_directory.to_path_buf(),
    };
    let project = find_lake_project(&start, workspace_root)?;
    let text = fs::read_to_string(project.join(MANIFEST_FILE)).ok()?;
    let manifest = velocity_git::parse_manifest(&text)?;
    let mathlib = manifest
        .packages
        .iter()
        .find(|package| package.name == MATHLIB)?;
    let olean = project
        .join(&manifest.packages_dir)
        .join(MATHLIB_OLEAN_RELATIVE);
    let stamped = fs::read_to_string(project.join(MATHLIB_CACHE_STAMP)).ok();
    if mathlib_cache_current(stamped.as_deref(), &mathlib.rev, olean.is_file()) {
        return None;
    }
    Some(PendingMathlibCache {
        project,
        rev: mathlib.rev.clone(),
    })
}

/// Records that `lake exe cache get` completed for `rev`, so subsequent builds skip it.
pub fn record_mathlib_cache_ready(project: &Path, rev: &str) {
    let stamp = project.join(MATHLIB_CACHE_STAMP);
    if let Some(parent) = stamp.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(stamp, rev);
}

/// Places a `lake` shim beside Velocity's other agent command shims when `lake` is installed.
pub(crate) fn install_lake_shim(folder: &Path) {
    if crate::server::program_folder("lake").is_none() {
        return;
    }
    let source = folder.join(executable_file("cargo"));
    let destination = folder.join(executable_file("lake"));
    let _ = copy_launcher(&source, &destination);
}

/// Agent-shell `lake` shim: unpacks Mathlib's prebuilt cache on demand before forwarding to
/// the real `lake` executable with Velocity's command shims stripped from `PATH` (so `lake`'s
/// internal `git` package clones never hit an agent `git` shim).
pub(crate) fn run_lake(arguments: &[OsString], passthrough: &[String]) -> io::Result<ExitStatus> {
    let path = toolchain_path()?;
    let program = real_program("lake", &path).ok_or_else(|| {
        io::Error::other("Could not find lake outside Velocity's managed commands")
    })?;
    let directory = std::env::current_dir()?;
    let text_arguments: Vec<String> = arguments
        .iter()
        .map(|value| value.to_string_lossy().into_owned())
        .collect();
    if let Some(pending) = pending_mathlib_cache(&directory, None, &text_arguments) {
        let mut fetch = background_command(&program);
        filter_environment(&mut fetch, passthrough);
        let status = fetch
            .current_dir(&pending.project)
            .args(["exe", "cache", "get"])
            .env("PATH", &path)
            .env_remove(crate::agent_run::BLOCK_VCS_ENV)
            .status()?;
        if !status.success() {
            return Ok(status);
        }
        record_mathlib_cache_ready(&pending.project, &pending.rev);
    }
    let mut command = background_command(program);
    filter_environment(&mut command, passthrough);
    command
        .args(arguments)
        .env("PATH", path)
        .env_remove(crate::agent_run::BLOCK_VCS_ENV)
        .status()
}

fn is_lake_project(directory: &Path) -> bool {
    directory.join(TOOLCHAIN_FILE).is_file()
        && LAKEFILES.iter().any(|name| directory.join(name).is_file())
}
