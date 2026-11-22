//! Concrete Windows path resolution, untracked file copying, the shared compiler cache,
//! and post-take workspace setup at the I/O edge (SPEC.md F5).

use std::ffi::OsStr;
use std::fs;
use std::path::{Component, Path, PathBuf};

use thiserror::Error;

use crate::path_scheme::{JobKind, LocationConfig, effective_root, leaf_for_job};
use crate::setup::{
    SetupAction, SetupPhase, WorktreeSettings, can_start_agent, compiler_cache_env,
    plan_setup_actions, resolve_cargo_target_dir,
};

/// Default comma-separated untracked files copied into a newly created workspace (F5).
pub const DEFAULT_UNTRACKED_FILES: &str = ".env, .env.local";

/// Default command executed in a newly created workspace before the agent starts (F5).
pub const DEFAULT_SETUP_COMMAND: &str = "";

/// Upper-bound pre-check character length for a workspace directory path on Windows.
pub const MAX_WORKTREE_PATH_LENGTH: usize = 254;

#[derive(Debug, Error)]
pub enum WorktreeError {
    #[error("worktree leaf name must not be empty")]
    EmptyLeafName,
    #[error("worktree leaf `{0}` is reserved or generic; folder names must be unique per project")]
    ReservedLeafName(String),
    #[error("project name must be a single normal directory name, got `{0}`")]
    InvalidProjectName(String),
    #[error("worktree drive `{0}` is not a valid Windows drive prefix")]
    InvalidDrivePrefix(String),
    #[error("worktree folder `{0}` must be an absolute path")]
    RelativeLocation(String),
    #[error(
        "worktree path `{path}` was rejected as too long on Windows (pre-check limit: {limit} characters)"
    )]
    PathTooLong { path: PathBuf, limit: usize },
    #[error("untracked file entry `{0}` must be a relative path inside the repository")]
    InvalidUntrackedFilePath(String),
    #[error("failed to copy untracked file `{source_path}` to `{destination_path}`: {source}")]
    CopyUntrackedFile {
        source_path: PathBuf,
        destination_path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to create directory `{path}`: {source}")]
    CreateDirectory {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to write `{path}`: {source}")]
    WriteFile {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("failed to spawn `{program}` in `{working_directory}`: {source}")]
    CommandSpawn {
        program: String,
        working_directory: PathBuf,
        source: std::io::Error,
    },
    #[error("`{command}` failed in `{working_directory}` ({status}): {stderr}")]
    CommandFailed {
        command: String,
        working_directory: PathBuf,
        status: String,
        stderr: String,
    },
}

/// Concrete inputs for workspace placement and post-take setup.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeSetupRequest {
    pub repository_root: PathBuf,
    pub worktree_path: PathBuf,
    pub branch_name: Option<String>,
    pub base_ref: String,
    pub settings: WorktreeSettings,
    pub untracked_files: String,
    pub setup_command: String,
    pub shared_cargo_target_dir: PathBuf,
}

impl WorktreeSetupRequest {
    #[allow(clippy::too_many_arguments)]
    pub fn branched(
        repository_root: PathBuf,
        worktree_path: PathBuf,
        branch_name: String,
        base_ref: String,
        settings: WorktreeSettings,
        untracked_files: String,
        setup_command: String,
        shared_cargo_target_dir: PathBuf,
    ) -> Self {
        Self {
            repository_root,
            worktree_path,
            branch_name: Some(branch_name),
            base_ref,
            settings,
            untracked_files,
            setup_command,
            shared_cargo_target_dir,
        }
    }

    /// A workspace setup request with no branch name (for example, review-swarm or resumed workspaces).
    pub fn detached(
        repository_root: PathBuf,
        worktree_path: PathBuf,
        base_ref: String,
        settings: WorktreeSettings,
        untracked_files: String,
        setup_command: String,
        shared_cargo_target_dir: PathBuf,
    ) -> Self {
        Self {
            repository_root,
            worktree_path,
            branch_name: None,
            base_ref,
            settings,
            untracked_files,
            setup_command,
            shared_cargo_target_dir,
        }
    }
}

/// Summary of an initialized workspace ready for its agent to start.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CreatedWorktree {
    pub worktree_path: PathBuf,
    pub branch_name: Option<String>,
    pub cargo_target_dir: Option<PathBuf>,
    pub copied_files: Vec<PathBuf>,
    pub phase: SetupPhase,
    pub agent_may_start: bool,
    pub transcript_step: String,
}

/// Pre-checks that `worktree_path` does not exceed [`MAX_WORKTREE_PATH_LENGTH`].
pub fn validate_worktree_path_length(worktree_path: &Path) -> Result<(), WorktreeError> {
    let character_count = worktree_path.to_string_lossy().chars().count();
    if character_count > MAX_WORKTREE_PATH_LENGTH {
        return Err(WorktreeError::PathTooLong {
            path: worktree_path.to_path_buf(),
            limit: MAX_WORKTREE_PATH_LENGTH,
        });
    }
    Ok(())
}

/// Strips a leading `vel/` prefix and converts `/` and `\` separators to `-`,
/// matching the reference UI's `wtLeaf` rule (`SPEC.md` F5, `design/main-window.html`).
pub fn strip_velocity_branch_prefix(branch_or_slug: &str) -> String {
    let trimmed = branch_or_slug.trim();
    let without_prefix = trimmed.strip_prefix("vel/").unwrap_or(trimmed);
    without_prefix
        .chars()
        .map(|character| match character {
            '/' | '\\' => '-',
            other => other,
        })
        .collect()
}

fn validate_leaf_folder(leaf: String) -> Result<String, WorktreeError> {
    let trimmed = leaf.trim();
    if trimmed.is_empty() {
        return Err(WorktreeError::EmptyLeafName);
    }
    if trimmed == "." || trimmed == ".." || trimmed.eq_ignore_ascii_case("workspace") {
        return Err(WorktreeError::ReservedLeafName(trimmed.to_owned()));
    }
    Ok(trimmed.to_owned())
}

/// Formats the project-unique folder leaf for a job (`None` for a spec interview).
pub fn format_leaf_folder(job: &JobKind<String, String>) -> Result<Option<String>, WorktreeError> {
    let Some(leaf) = leaf_for_job(job.clone()) else {
        return Ok(None);
    };
    let raw_leaf = match leaf {
        crate::path_scheme::WorktreeLeaf::Branch(slug) => strip_velocity_branch_prefix(&slug),
        crate::path_scheme::WorktreeLeaf::ScoutSweep(agent_id) => {
            let clean_agent = strip_velocity_branch_prefix(&agent_id);
            if clean_agent.is_empty() {
                return Err(WorktreeError::EmptyLeafName);
            }
            format!("{clean_agent}-sweep")
        }
    };
    validate_leaf_folder(raw_leaf).map(Some)
}

fn is_single_normal_segment(segment: &str) -> bool {
    let mut components = Path::new(segment).components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

/// Resolves the effective workspace root directory for a project: `<override>` if set,
/// otherwise `<drive><folder>\<project>` (`<folder>/<project>` on Unix).
pub fn resolve_project_worktree_root(
    drive: &str,
    folder: &str,
    project_name: &str,
    project_override: Option<&Path>,
) -> Result<PathBuf, WorktreeError> {
    if !is_single_normal_segment(project_name) {
        return Err(WorktreeError::InvalidProjectName(project_name.to_owned()));
    }
    let mut global_root = location_root(drive, folder)?;
    global_root.push(project_name);
    let config = LocationConfig {
        global_project_root: global_root,
        project_override: project_override.map(Path::to_path_buf),
    };
    Ok(effective_root(config))
}

/// `<drive>\\<folder>`: Windows splits the location into a drive letter and a folder.
#[cfg(windows)]
fn location_root(drive: &str, folder: &str) -> Result<PathBuf, WorktreeError> {
    let trimmed_drive = drive.trim();
    let drive_bytes = trimmed_drive.as_bytes();
    if drive_bytes.len() != 2 || !drive_bytes[0].is_ascii_alphabetic() || drive_bytes[1] != b':' {
        return Err(WorktreeError::InvalidDrivePrefix(drive.to_owned()));
    }
    let mut root = PathBuf::from(format!("{trimmed_drive}\\"));
    for segment in folder
        .split(['\\', '/'])
        .filter(|segment| !segment.is_empty())
    {
        root.push(segment);
    }
    Ok(root)
}

/// Unix has no drives, so the folder is the whole location and must be absolute. A
/// backslash is an ordinary file name character here, never a separator.
#[cfg(not(windows))]
fn location_root(drive: &str, folder: &str) -> Result<PathBuf, WorktreeError> {
    if !drive.trim().is_empty() {
        return Err(WorktreeError::InvalidDrivePrefix(drive.to_owned()));
    }
    if !folder.starts_with('/') {
        return Err(WorktreeError::RelativeLocation(folder.to_owned()));
    }
    let mut root = PathBuf::from("/");
    for segment in folder.split('/').filter(|segment| !segment.is_empty()) {
        root.push(segment);
    }
    Ok(root)
}

/// Resolves the full workspace directory path for a job (`None` for a spec interview),
/// validating that the planned path fits within [`MAX_WORKTREE_PATH_LENGTH`].
pub fn resolve_worktree_directory(
    drive: &str,
    folder: &str,
    project_name: &str,
    project_override: Option<&Path>,
    job: &JobKind<String, String>,
) -> Result<Option<PathBuf>, WorktreeError> {
    let Some(leaf_folder) = format_leaf_folder(job)? else {
        return Ok(None);
    };
    let root = resolve_project_worktree_root(drive, folder, project_name, project_override)?;
    let planned_path = root.join(leaf_folder);
    validate_worktree_path_length(&planned_path)?;
    Ok(Some(planned_path))
}

/// Formats the transcript step recorded when a workspace is ready (F5).
pub fn creation_transcript_step(worktree_path: &Path) -> String {
    format!("Workspace · {} · ready", worktree_path.display())
}

/// Parses the comma-separated list of untracked files to copy into a new workspace.
pub fn parse_untracked_file_list(configured_list: &str) -> Result<Vec<PathBuf>, WorktreeError> {
    let mut paths = Vec::new();
    for entry in configured_list.split(',') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        let candidate = PathBuf::from(trimmed);
        let has_unsafe_component = candidate
            .components()
            .any(|component| !matches!(component, Component::Normal(_) | Component::CurDir));
        if candidate.is_absolute() || has_unsafe_component {
            return Err(WorktreeError::InvalidUntrackedFilePath(trimmed.to_owned()));
        }
        paths.push(candidate);
    }
    Ok(paths)
}

/// Copies each configured untracked file that exists in `repository_root` into
/// `worktree_path`, preserving relative paths.
pub fn copy_configured_untracked_files(
    repository_root: &Path,
    worktree_path: &Path,
    untracked_files: &[PathBuf],
) -> Result<Vec<PathBuf>, WorktreeError> {
    let mut copied = Vec::new();
    for relative_path in untracked_files {
        let source_path = repository_root.join(relative_path);
        if !source_path.is_file() {
            continue;
        }
        let destination_path = worktree_path.join(relative_path);
        if let Some(parent_directory) = destination_path.parent() {
            fs::create_dir_all(parent_directory).map_err(|source| {
                WorktreeError::CreateDirectory {
                    path: parent_directory.to_path_buf(),
                    source,
                }
            })?;
        }
        fs::copy(&source_path, &destination_path).map_err(|source| {
            WorktreeError::CopyUntrackedFile {
                source_path,
                destination_path,
                source,
            }
        })?;
        copied.push(relative_path.clone());
    }
    Ok(copied)
}

fn run_checked_command<I, S>(
    working_directory: &Path,
    program: &str,
    arguments: I,
    cargo_target_dir: Option<&Path>,
) -> Result<(), WorktreeError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let collected_arguments: Vec<String> = arguments
        .into_iter()
        .map(|argument| argument.as_ref().to_string_lossy().into_owned())
        .collect();
    let mut command = crate::command::hidden_command(program);
    command
        .current_dir(working_directory)
        .args(&collected_arguments);
    // Sharing the build cache means one compiler cache and a target folder per workspace
    // (F5): a shared `CARGO_TARGET_DIR` is locked by cargo, so agents would build one at a time.
    if cargo_target_dir.is_some() {
        command
            .envs(compiler_cache_env())
            .env_remove("CARGO_TARGET_DIR");
    }
    let output = command
        .output()
        .map_err(|source| WorktreeError::CommandSpawn {
            program: program.to_owned(),
            working_directory: working_directory.to_path_buf(),
            source,
        })?;
    if !output.status.success() {
        return Err(WorktreeError::CommandFailed {
            command: format!("{} {}", program, collected_arguments.join(" ")),
            working_directory: working_directory.to_path_buf(),
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        });
    }
    Ok(())
}

fn run_setup_shell_command(
    worktree_path: &Path,
    setup_command: &str,
    cargo_target_dir: Option<&Path>,
) -> Result<(), WorktreeError> {
    let trimmed = setup_command.trim();
    if trimmed.is_empty() {
        return Ok(());
    }
    if cfg!(windows) {
        run_checked_command(
            worktree_path,
            "cmd.exe",
            ["/d", "/s", "/c", trimmed],
            cargo_target_dir,
        )
    } else {
        run_checked_command(worktree_path, "sh", ["-c", trimmed], cargo_target_dir)
    }
}

fn execute_setup_action(
    request: &WorktreeSetupRequest,
    action: SetupAction<&PathBuf>,
    cargo_target_dir: Option<&Path>,
    copied_files: &mut Vec<PathBuf>,
) -> Result<(), WorktreeError> {
    match action {
        SetupAction::CreateWorktreeFromMaster(_) => Ok(()),
        SetupAction::ConfigureSharedCargoTarget(shared_target_dir) => {
            fs::create_dir_all(shared_target_dir).map_err(|source| WorktreeError::CreateDirectory {
                path: shared_target_dir.to_path_buf(),
                source,
            })
        }
        SetupAction::CopyUntrackedFiles(worktree_path) => {
            let parsed_files = parse_untracked_file_list(&request.untracked_files)?;
            *copied_files = copy_configured_untracked_files(
                &request.repository_root,
                worktree_path,
                &parsed_files,
            )?;
            Ok(())
        }
        SetupAction::RunSetupCommand(worktree_path) => {
            run_setup_shell_command(worktree_path, &request.setup_command, cargo_target_dir)
        }
    }
}

/// Runs post-take setup steps (build-cache directory, untracked file copy, and setup command)
/// for a Rosen workspace already placed at `request.worktree_path`. Never invokes `git`.
pub fn finish_worktree_setup(
    request: &WorktreeSetupRequest,
) -> Result<CreatedWorktree, WorktreeError> {
    let actions: Vec<_> = plan_setup_actions(
        request.settings,
        &request.worktree_path,
        &request.shared_cargo_target_dir,
    )
    .into_iter()
    .filter(|action| !matches!(action, SetupAction::CreateWorktreeFromMaster(_)))
    .collect();
    let cargo_target_dir = resolve_cargo_target_dir(
        request.settings.share_build_cache,
        request.shared_cargo_target_dir.clone(),
    );
    let mut copied_files = Vec::new();
    for action in actions {
        execute_setup_action(
            request,
            action,
            cargo_target_dir.as_deref(),
            &mut copied_files,
        )?;
    }
    let phase = SetupPhase::Ready;
    Ok(CreatedWorktree {
        worktree_path: request.worktree_path.clone(),
        branch_name: request.branch_name.clone(),
        cargo_target_dir,
        copied_files,
        phase,
        agent_may_start: can_start_agent(phase),
        transcript_step: creation_transcript_step(&request.worktree_path),
    })
}
