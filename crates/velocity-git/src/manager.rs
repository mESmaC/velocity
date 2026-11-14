//! Concrete Windows path resolution, untracked file copying, the shared compiler cache for
//! setup commands, and `git worktree` command execution at the I/O edge (SPEC.md F5).

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::thread;
use std::time::Duration;

use thiserror::Error;

use crate::lifecycle::GitCommand;
use crate::path_scheme::{JobKind, LocationConfig, effective_root, leaf_for_job};
use crate::setup::{
    SetupAction, SetupPhase, WorktreeSettings, can_start_agent, plan_setup_actions,
    resolve_cargo_target_dir,
};

/// Default comma-separated untracked files copied into a newly created worktree (F5).
pub const DEFAULT_UNTRACKED_FILES: &str = ".env, .env.local";

/// Default command executed in a newly created worktree before the agent starts (F5).
pub const DEFAULT_SETUP_COMMAND: &str = "";

/// Upper-bound pre-check character length for a worktree directory path so `<worktree>\.git`
/// stays within Windows `MAX_PATH` (260 characters including the null terminator, i.e. 259
/// usable characters minus 5 for `\.git` = 254 characters).
///
/// Git's own internal `$GIT_DIR` buffer can reject slightly shorter paths (around 215+
/// characters on Windows depending on repository and leaf names); `create_git_worktree_from_base`
/// maps those `git worktree add` failures (`'$GIT_DIR' too big` or `Filename too long`) to
/// [`WorktreeError::PathTooLong`] as well.
pub const MAX_WORKTREE_PATH_LENGTH: usize = 254;

/// Retry backoff delays (in milliseconds) when `git worktree remove` hits a transient
/// Windows file handle lock (`design/main-window.html`).
const WINDOWS_REMOVE_BACKOFF_MILLISECONDS: [u64; 3] = [50, 200, 800];

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

/// Concrete inputs required to create and set up a worktree from `master`.
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

    /// A worktree with no branch, checked out detached at `base_ref` (a scout's sweep).
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

/// Summary of a created and initialized worktree that is ready for its agent to start.
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

/// Pre-checks that `worktree_path` does not exceed [`MAX_WORKTREE_PATH_LENGTH`] before running
/// any filesystem or git command. Because git's internal `$GIT_DIR` limit can still reject
/// paths below [`MAX_WORKTREE_PATH_LENGTH`], `git worktree add` stderr is also mapped via
/// [`map_worktree_add_error`].
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

/// Strips a leading `vel/` branch prefix and converts `/` and `\` separators to `-`,
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
        crate::path_scheme::WorktreeLeaf::DutyReview(pr_number) => format!("review-pr-{pr_number}"),
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

/// Resolves the effective worktree root directory for a project: `<override>` if set,
/// otherwise `<drive><folder>\<project>`.
pub fn resolve_project_worktree_root(
    drive: &str,
    folder: &str,
    project_name: &str,
    project_override: Option<&Path>,
) -> Result<PathBuf, WorktreeError> {
    if !is_single_normal_segment(project_name) {
        return Err(WorktreeError::InvalidProjectName(project_name.to_owned()));
    }
    let trimmed_drive = drive.trim();
    let drive_bytes = trimmed_drive.as_bytes();
    if drive_bytes.len() != 2 || !drive_bytes[0].is_ascii_alphabetic() || drive_bytes[1] != b':' {
        return Err(WorktreeError::InvalidDrivePrefix(drive.to_owned()));
    }
    let mut global_root = PathBuf::from(format!("{trimmed_drive}\\"));
    for segment in folder
        .split(['\\', '/'])
        .filter(|segment| !segment.is_empty())
    {
        global_root.push(segment);
    }
    global_root.push(project_name);
    let config = LocationConfig {
        global_project_root: global_root,
        project_override: project_override.map(Path::to_path_buf),
    };
    Ok(effective_root(config))
}

/// Resolves the full worktree directory path for a job (`None` for a spec interview),
/// validating that the planned path fits within [`MAX_WORKTREE_PATH_LENGTH`] before any
/// git or filesystem operation runs.
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

/// Formats the first transcript step recorded when a worktree is created from `master` (F5).
pub fn creation_transcript_step(worktree_path: &Path) -> String {
    format!(
        "Worktree · {} · created from master",
        worktree_path.display()
    )
}

/// Parses the comma-separated list of untracked files to copy into a new worktree.
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
    // Sharing the build cache means one compiler cache and a target folder per worktree
    // (F5): a shared `CARGO_TARGET_DIR` is locked by cargo, so agents would build one at a time.
    if cargo_target_dir.is_some() {
        command
            .envs(crate::ready_pool::compiler_cache_env())
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

pub(crate) fn run_git_command<I, S>(
    working_directory: &Path,
    arguments: I,
) -> Result<(), WorktreeError>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let mut git_arguments: Vec<OsString> = vec![
        OsStr::new("-c").to_os_string(),
        OsStr::new("core.longpaths=true").to_os_string(),
    ];
    for argument in arguments {
        git_arguments.push(argument.as_ref().to_os_string());
    }
    run_checked_command(working_directory, "git", git_arguments, None)
}

/// Outcome of probing `git show-ref --verify --quiet refs/heads/<branch>` before `worktree add -b`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BranchExistenceProbe {
    /// Exit status 0: the branch already existed before `worktree add -b`.
    Existing,
    /// Exit status 1: `show-ref` confirmed the branch did not exist at probe time.
    Absent,
    /// Any other exit status or spawn failure: skip `git branch -D` rollback so a pre-existing
    /// branch cannot be deleted when `show-ref` failed for an unrelated reason.
    Indeterminate,
}

/// Classifies the exit code of `git show-ref --verify --quiet` so only exit code `Some(1)`
/// is treated as absent before `git worktree add -b`.
pub fn classify_show_ref_exit_code(exit_code: Option<i32>) -> BranchExistenceProbe {
    match exit_code {
        Some(0) => BranchExistenceProbe::Existing,
        Some(1) => BranchExistenceProbe::Absent,
        _ => BranchExistenceProbe::Indeterminate,
    }
}

/// Returns `true` when `git worktree add -b` failed because the target branch already existed
/// (including when a concurrent process created the branch between the preflight probe and
/// `worktree add -b`), in which case this attempt did not create the branch and must not
/// delete it.
pub fn is_branch_already_exists_stderr(stderr: &str, branch_name: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    let expected = format!("a branch named '{branch_name}' already exists").to_ascii_lowercase();
    lower.contains(&expected)
        || (lower.contains("a branch named ") && lower.contains(" already exists"))
}

/// Decides whether `git branch -D` rollback is permitted after a failed `git worktree add -b`:
/// only when `show-ref` confirmed the branch was absent before the attempt AND the failure was
/// not `a branch named ... already exists` (which proves another actor owned the branch before
/// `worktree add -b` created it).
pub fn should_rollback_branch_after_worktree_add_error(
    probe: BranchExistenceProbe,
    branch_name: &str,
    error: &WorktreeError,
) -> bool {
    if probe != BranchExistenceProbe::Absent {
        return false;
    }
    match error {
        WorktreeError::CommandFailed { stderr, .. } => {
            !is_branch_already_exists_stderr(stderr, branch_name)
        }
        _ => true,
    }
}

/// Returns `true` when `git worktree add` stderr indicates git's path or `$GIT_DIR` length limit.
pub fn is_git_worktree_path_too_long_stderr(stderr: &str) -> bool {
    let lower = stderr.to_ascii_lowercase();
    stderr.contains("'$GIT_DIR' too big")
        || stderr.contains("\"$GIT_DIR\" too big")
        || lower.contains("git_dir' too big")
        || lower.contains("filename too long")
}

/// Maps a `git worktree add` failure whose stderr reports `'$GIT_DIR' too big` or
/// `Filename too long` to [`WorktreeError::PathTooLong`].
pub fn map_worktree_add_error(worktree_path: &Path, error: WorktreeError) -> WorktreeError {
    match error {
        WorktreeError::CommandFailed { ref stderr, .. }
            if is_git_worktree_path_too_long_stderr(stderr) =>
        {
            WorktreeError::PathTooLong {
                path: worktree_path.to_path_buf(),
                limit: MAX_WORKTREE_PATH_LENGTH,
            }
        }
        other => other,
    }
}

/// Whether `refs/heads/<branch_name>` exists, so a new thread can pick a branch nobody has.
/// A probe that fails for another reason answers `false`: `git worktree add -b` then reports
/// the real problem instead of the caller searching for a free name forever.
pub fn local_branch_exists(repository_root: &Path, branch_name: &str) -> bool {
    matches!(
        probe_local_branch_existence(repository_root, branch_name),
        BranchExistenceProbe::Existing
    )
}

fn probe_local_branch_existence(repository_root: &Path, branch_name: &str) -> BranchExistenceProbe {
    let reference = format!("refs/heads/{branch_name}");
    let output = crate::command::hidden_command("git")
        .current_dir(repository_root)
        .args([
            "-c",
            "core.longpaths=true",
            "show-ref",
            "--verify",
            "--quiet",
            reference.as_str(),
        ])
        .output();
    match output {
        Ok(process_output) => classify_show_ref_exit_code(process_output.status.code()),
        Err(_) => BranchExistenceProbe::Indeterminate,
    }
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

pub(crate) fn create_git_worktree_from_base(
    request: &WorktreeSetupRequest,
    worktree_path: &Path,
) -> Result<(), WorktreeError> {
    validate_worktree_path_length(worktree_path)?;
    if let Some(parent_directory) = worktree_path.parent() {
        fs::create_dir_all(parent_directory).map_err(|source| WorktreeError::CreateDirectory {
            path: parent_directory.to_path_buf(),
            source,
        })?;
    }
    let worktree_path_text = worktree_path.to_string_lossy().into_owned();
    if let Some(branch) = &request.branch_name {
        let probe = probe_local_branch_existence(&request.repository_root, branch);
        let raw_result = run_git_command(
            &request.repository_root,
            [
                "worktree",
                "add",
                "-b",
                branch.as_str(),
                worktree_path_text.as_str(),
                &request.base_ref,
            ],
        );
        if let Err(ref error) = raw_result
            && should_rollback_branch_after_worktree_add_error(probe, branch, error)
        {
            let _ = run_git_command(&request.repository_root, ["branch", "-D", branch.as_str()]);
        }
        raw_result.map_err(|error| map_worktree_add_error(worktree_path, error))
    } else {
        run_git_command(
            &request.repository_root,
            [
                "worktree",
                "add",
                "--detach",
                worktree_path_text.as_str(),
                &request.base_ref,
            ],
        )
        .map_err(|error| map_worktree_add_error(worktree_path, error))
    }
}

fn execute_setup_action(
    request: &WorktreeSetupRequest,
    action: SetupAction<&PathBuf>,
    cargo_target_dir: Option<&Path>,
    copied_files: &mut Vec<PathBuf>,
) -> Result<(), WorktreeError> {
    match action {
        SetupAction::CreateWorktreeFromMaster(worktree_path) => {
            create_git_worktree_from_base(request, worktree_path)
        }
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

/// Executes the planned worktree creation and setup steps in order. The returned
/// [`CreatedWorktree`] marks `agent_may_start` via [`can_start_agent`] only after every
/// setup action finishes.
pub fn execute_worktree_setup(
    request: &WorktreeSetupRequest,
) -> Result<CreatedWorktree, WorktreeError> {
    let actions = plan_setup_actions(
        request.settings,
        &request.worktree_path,
        &request.shared_cargo_target_dir,
    );
    let cargo_target_dir = resolve_cargo_target_dir(
        request.settings.share_build_cache,
        request.shared_cargo_target_dir.clone(),
    );
    run_setup_actions(request, actions, cargo_target_dir)
}

/// Every setup step after the checkout, for a worktree [`create_git_worktree_from_base`]
/// already created (see `titled`, which names the worktree in between).
pub fn finish_worktree_setup(
    request: &WorktreeSetupRequest,
) -> Result<CreatedWorktree, WorktreeError> {
    let actions = plan_setup_actions(
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
    run_setup_actions(request, actions, cargo_target_dir)
}

fn run_setup_actions(
    request: &WorktreeSetupRequest,
    actions: Vec<SetupAction<&PathBuf>>,
    cargo_target_dir: Option<PathBuf>,
) -> Result<CreatedWorktree, WorktreeError> {
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

fn remove_single_worktree_with_backoff(
    repository_root: &Path,
    worktree_path: &Path,
) -> Result<(), WorktreeError> {
    let worktree_path_text = worktree_path.to_string_lossy().into_owned();
    let mut last_error = None;
    let retry_delays = [0_u64]
        .into_iter()
        .chain(WINDOWS_REMOVE_BACKOFF_MILLISECONDS);
    for (attempt, delay_milliseconds) in retry_delays.enumerate() {
        if attempt > 0 {
            thread::sleep(Duration::from_millis(delay_milliseconds));
        }
        match run_git_command(
            repository_root,
            ["worktree", "remove", "--force", worktree_path_text.as_str()],
        ) {
            Ok(()) => return Ok(()),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| WorktreeError::CommandFailed {
        command: format!("git -c core.longpaths=true worktree remove --force {worktree_path_text}"),
        working_directory: repository_root.to_path_buf(),
        status: String::from("unknown"),
        stderr: String::from("worktree removal failed after retries"),
    }))
}

/// Executes a single [`GitCommand`] emitted by the pure lifecycle state machine.
pub fn execute_git_command(
    repository_root: &Path,
    command: &GitCommand<PathBuf, String>,
) -> Result<(), WorktreeError> {
    match command {
        GitCommand::RemoveWorktree(worktree_path) => {
            remove_single_worktree_with_backoff(repository_root, worktree_path)
        }
        GitCommand::DeleteBranch(branch_name) => {
            run_git_command(repository_root, ["branch", "-D", branch_name.as_str()])
        }
        GitCommand::PruneOrphans => run_git_command(repository_root, ["worktree", "prune"]),
    }
}

/// Executes a sequence of [`GitCommand`]s in order.
pub fn execute_git_commands(
    repository_root: &Path,
    commands: &[GitCommand<PathBuf, String>],
) -> Result<(), WorktreeError> {
    for command in commands {
        execute_git_command(repository_root, command)?;
    }
    Ok(())
}

/// Runs `git worktree prune` for the Settings → Worktrees **Prune orphans** button (F5).
pub fn prune_orphan_worktrees(repository_root: &Path) -> Result<(), WorktreeError> {
    let command: GitCommand<PathBuf, String> = crate::lifecycle::prune_orphans_command();
    execute_git_command(repository_root, &command)
}
