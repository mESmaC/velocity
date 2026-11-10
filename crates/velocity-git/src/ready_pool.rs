//! Ready worktrees for the warm pool (SPEC.md F5 Warm pool): detached checkouts at the tip of
//! `master` under `<project root>\.ready-<n>`, made from the project's one clone, with the
//! configured files copied and the project prebuilt, so a new thread only moves one into
//! place and creates its branch.
//!
//! The setup command is not run here. It runs after the take, at the thread's own path:
//! `pnpm install` links packages with absolute junctions on Windows, and a junction made in
//! the pool would point back into the pool folder once the worktree moves. Cargo's `target`
//! folder has no such paths and stays fresh across the move, so the prebuild is kept.
//!
//! Each slot records its base commit and whether its prebuild finished in a `velocity-ready`
//! file in its git admin folder, outside the checkout, so agents never see it and a restart
//! can tell a finished slot from one interrupted mid-setup.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use velocity_core::ready_pool::{slot_folder, slot_index};

use crate::command::git;
use crate::manager::{
    WorktreeError, WorktreeSetupRequest, copy_configured_untracked_files,
    parse_untracked_file_list, run_git_command, validate_worktree_path_length,
};

const MARK_FILE: &str = "velocity-ready";

/// What a slot's mark file says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadyMark {
    pub base: String,
    pub built: bool,
}

/// A slot found on disk: its index, its folder, and its mark (`None` when setup never
/// finished, so the slot must be made again).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoundSlot {
    pub index: u32,
    pub path: PathBuf,
    pub mark: Option<ReadyMark>,
}

/// A ready slot a thread is taking, and the tip to bring it to first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadyTake {
    pub slot: PathBuf,
    pub tip: Option<String>,
}

/// Slot `index`'s folder in the project's worktree root.
pub fn ready_slot_path(project_root: &Path, index: u32) -> PathBuf {
    project_root.join(slot_folder(index))
}

/// The repository's registered worktrees that are pool slots of `project_root`.
pub fn scan_ready_slots(
    repository_root: &Path,
    project_root: &Path,
) -> Result<Vec<FoundSlot>, WorktreeError> {
    let wanted_parent = fs::canonicalize(project_root).ok();
    let mut found: Vec<FoundSlot> = registered_worktrees(repository_root)?
        .into_iter()
        .filter_map(|path| {
            let index = slot_index(&path.file_name()?.to_string_lossy())?;
            let parent = fs::canonicalize(path.parent()?).ok();
            (wanted_parent.is_some() && parent == wanted_parent).then_some((index, path))
        })
        .map(|(index, path)| FoundSlot {
            index,
            mark: read_mark(&path),
            path,
        })
        .collect();
    found.sort_by_key(|slot| slot.index);
    Ok(found)
}

/// Makes the slot at `request.worktree_path`: a detached checkout of `request.base_ref` (the
/// tip commit) with the configured files copied. A slot left over at that path is removed
/// first; a folder there that is not a slot worktree is never touched.
pub fn create_ready_worktree(request: &WorktreeSetupRequest) -> Result<(), WorktreeError> {
    let path = &request.worktree_path;
    validate_worktree_path_length(path)?;
    if path.exists() {
        remove_ready_worktree(&request.repository_root, path)?;
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| WorktreeError::CreateDirectory {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let text = path.to_string_lossy().into_owned();
    // `--force` only re-adds a slot whose folder vanished while git still lists it.
    run_git_command(
        &request.repository_root,
        [
            "worktree",
            "add",
            "--force",
            "--detach",
            text.as_str(),
            &request.base_ref,
        ],
    )?;
    if request.settings.copy_untracked_files {
        let files = parse_untracked_file_list(&request.untracked_files)?;
        copy_configured_untracked_files(&request.repository_root, path, &files)?;
    }
    write_mark(path, &request.base_ref, false)
}

/// Moves a ready slot's checkout forward to `tip` after a merge. Its prebuild is redone.
pub fn catch_up_ready_worktree(slot: &Path, tip: &str) -> Result<(), WorktreeError> {
    run_git_command(slot, ["checkout", "--quiet", "--detach", tip])?;
    write_mark(slot, tip, false)
}

/// Builds the slot at its base so an agent's first build compiles only what it changes:
/// `cargo build --workspace` when the checkout is a Cargo project, in the slot's own `target`
/// folder, with the shared compiler cache, below normal priority so agents' own builds go
/// first. The slot is marked built either way: a build that fails at the tip of `master`
/// fails for the agent too, and retrying it would only keep the machine busy.
pub fn prebuild_ready_worktree(slot: &Path) -> Result<(), WorktreeError> {
    let base = read_mark(slot)
        .map(|mark| mark.base)
        .ok_or_else(|| unmarked(slot))?;
    let built = if slot.join("Cargo.toml").is_file() {
        run_prebuild(slot)
    } else {
        Ok(())
    };
    write_mark(slot, &base, true)?;
    built
}

/// Takes `ready.slot` for a thread: catches it up to the tip, moves it to the thread's path
/// and creates the thread's branch there. A take that fails after the move removes the moved
/// checkout (it holds no work yet), so the caller can make the worktree the usual way.
pub fn take_ready_worktree(
    ready: &ReadyTake,
    request: &WorktreeSetupRequest,
) -> Result<(), WorktreeError> {
    validate_worktree_path_length(&request.worktree_path)?;
    let slot = &ready.slot;
    let mark = read_mark(slot).ok_or_else(|| unmarked(slot))?;
    // The mark goes first: from here on the folder is no longer a slot to hand out.
    clear_mark(slot);
    if let Some(tip) = ready.tip.as_deref().filter(|tip| *tip != mark.base) {
        run_git_command(slot, ["checkout", "--quiet", "--detach", tip])?;
    }
    if let Some(parent) = request.worktree_path.parent() {
        fs::create_dir_all(parent).map_err(|source| WorktreeError::CreateDirectory {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    let source = slot.to_string_lossy().into_owned();
    let destination = request.worktree_path.to_string_lossy().into_owned();
    run_git_command(
        &request.repository_root,
        ["worktree", "move", source.as_str(), destination.as_str()],
    )?;
    let Some(branch) = &request.branch_name else {
        return Ok(());
    };
    let switched = run_git_command(&request.worktree_path, ["switch", "--quiet", "-c", branch]);
    if switched.is_err() {
        let _ = run_git_command(
            &request.repository_root,
            ["worktree", "remove", "--force", destination.as_str()],
        );
    }
    switched
}

/// Removes one pool slot (its own entry only, never a repository-wide prune, F5 Guards).
/// Refuses any folder that is not a registered slot worktree with a detached checkout, so
/// a thread's worktree can never be removed through the pool.
pub fn remove_ready_worktree(repository_root: &Path, slot: &Path) -> Result<(), WorktreeError> {
    let is_slot = slot
        .file_name()
        .and_then(|name| slot_index(&name.to_string_lossy()))
        .is_some();
    let wanted = fs::canonicalize(slot).ok();
    let registered = wanted.is_some()
        && registered_worktrees(repository_root)?
            .iter()
            .any(|path| fs::canonicalize(path).ok() == wanted);
    if !is_slot || !registered || has_branch(slot) {
        return Err(WorktreeError::CommandFailed {
            command: "remove ready worktree".into(),
            working_directory: repository_root.to_path_buf(),
            status: "refused".into(),
            stderr: format!("{} is not a ready worktree of this project", slot.display()),
        });
    }
    let text = slot.to_string_lossy().into_owned();
    run_git_command(
        repository_root,
        ["worktree", "remove", "--force", text.as_str()],
    )
}

/// The environment that gives builds the shared compiler cache (F5): `RUSTC_WRAPPER=sccache`
/// when sccache is installed and no wrapper is set already. Agents and prebuilds use the
/// same answer, so a prebuilt `target` folder stays fresh for the agent's first build.
pub fn compiler_cache_env() -> Vec<(String, String)> {
    let wrapper_set = std::env::var_os("RUSTC_WRAPPER").is_some_and(|value| !value.is_empty());
    if wrapper_set || find_on_path("sccache").is_none() {
        return Vec::new();
    }
    Vec::from([("RUSTC_WRAPPER".to_owned(), "sccache".to_owned())])
}

fn find_on_path(program: &str) -> Option<PathBuf> {
    let names: Vec<String> = if cfg!(windows) {
        Vec::from([format!("{program}.exe"), format!("{program}.cmd")])
    } else {
        Vec::from([program.to_owned()])
    };
    std::env::split_paths(&std::env::var_os("PATH")?)
        .flat_map(|folder| names.iter().map(move |name| folder.join(name)))
        .find(|candidate| candidate.is_file())
}

fn run_prebuild(slot: &Path) -> Result<(), WorktreeError> {
    let mut command = Command::new("cargo");
    command
        .current_dir(slot)
        .args(["build", "--workspace", "--quiet"])
        .envs(compiler_cache_env())
        .env_remove("CARGO_TARGET_DIR");
    background_priority(&mut command);
    let output = command
        .output()
        .map_err(|source| WorktreeError::CommandSpawn {
            program: "cargo".into(),
            working_directory: slot.to_path_buf(),
            source,
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(WorktreeError::CommandFailed {
        command: "cargo build --workspace".into(),
        working_directory: slot.to_path_buf(),
        status: output.status.to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

/// Below-normal priority and no console window: the prebuild is for later, agents' builds
/// are for now.
#[cfg(windows)]
fn background_priority(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;
    command.creation_flags(CREATE_NO_WINDOW | BELOW_NORMAL_PRIORITY_CLASS);
}

#[cfg(not(windows))]
fn background_priority(_command: &mut Command) {}

fn registered_worktrees(repository_root: &Path) -> Result<Vec<PathBuf>, WorktreeError> {
    let output = git()
        .current_dir(repository_root)
        .args(["worktree", "list", "--porcelain"])
        .output()
        .map_err(|source| WorktreeError::CommandSpawn {
            program: "git".into(),
            working_directory: repository_root.to_path_buf(),
            source,
        })?;
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.strip_prefix("worktree "))
        .map(PathBuf::from)
        .collect())
}

fn has_branch(slot: &Path) -> bool {
    git()
        .current_dir(slot)
        .args(["symbolic-ref", "--quiet", "HEAD"])
        .output()
        .is_ok_and(|output| output.status.success())
}

/// The slot's admin folder in the main repository (`.git/worktrees/<name>`).
fn admin_folder(slot: &Path) -> Option<PathBuf> {
    let output = git()
        .current_dir(slot)
        .args(["rev-parse", "--absolute-git-dir"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let folder = PathBuf::from(text.trim());
    folder.is_dir().then_some(folder)
}

fn read_mark(slot: &Path) -> Option<ReadyMark> {
    let text = fs::read_to_string(admin_folder(slot)?.join(MARK_FILE)).ok()?;
    let mut lines = text.lines();
    let base = lines.next()?.trim().to_owned();
    let built = lines.next()?.trim() == "built";
    let valid = (7..=64).contains(&base.len()) && base.bytes().all(|byte| byte.is_ascii_hexdigit());
    valid.then_some(ReadyMark { base, built })
}

fn write_mark(slot: &Path, base: &str, built: bool) -> Result<(), WorktreeError> {
    let folder = admin_folder(slot).ok_or_else(|| unmarked(slot))?;
    let path = folder.join(MARK_FILE);
    let state = if built { "built" } else { "unbuilt" };
    fs::write(&path, format!("{base}\n{state}\n"))
        .map_err(|source| WorktreeError::WriteFile { path, source })
}

fn clear_mark(slot: &Path) {
    if let Some(folder) = admin_folder(slot) {
        let _ = fs::remove_file(folder.join(MARK_FILE));
    }
}

fn unmarked(slot: &Path) -> WorktreeError {
    WorktreeError::CommandFailed {
        command: "read ready worktree".into(),
        working_directory: slot.to_path_buf(),
        status: "not ready".into(),
        stderr: format!("{} has no finished pool setup", slot.display()),
    }
}
