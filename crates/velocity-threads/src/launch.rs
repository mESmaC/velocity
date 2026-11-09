//! Planning a build thread from the home composer (SPEC.md F4, F5, §3.2): its name, its
//! worktree from the project's base branch, and the ACP agent it runs.

use std::path::{Path, PathBuf};

use thiserror::Error;
use velocity_acp::client::AgentLaunch;
use velocity_git::{
    DEFAULT_SETUP_COMMAND, DEFAULT_UNTRACKED_FILES, JobKind, WorktreeError,
    WorktreeSettings as GitWorktreeSettings, WorktreeSetupRequest, local_branch_exists,
    resolve_project_worktree_root, resolve_worktree_directory,
};
use velocity_store::threads::NewThread;
use velocity_store::{ProjectItem, ProviderKind, Settings};

use crate::naming::{fallback_title, message_slug, unique_slug};

/// The reference's default worktree folder under the chosen drive (`WT.folder`).
const DEFAULT_WORKTREE_FOLDER: &str = r"\velocity\worktrees";
/// The project's shared `CARGO_TARGET_DIR` (F5), beside its worktrees. Slugs never contain a
/// dot, so no worktree can take this name.
const SHARED_TARGET_FOLDER: &str = ".cargo-target";
/// Shown on the model pill until the model menu (F15) chooses one: the agent's own default.
const AGENT_DEFAULT_MODEL: &str = "agent default";

#[derive(Debug, Error)]
pub enum LaunchError {
    #[error("{0} has no confirmed ACP agent yet (SPEC.md Q14)")]
    ProviderNotConfirmed(&'static str),
    #[error("Could not tell which drive `{0}` is on; set Settings → Worktrees → Location")]
    UnknownDrive(String),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
}

/// Everything a new thread needs before anything runs.
#[derive(Clone, Debug)]
pub struct LaunchPlan {
    pub project: u32,
    pub prompt: String,
    pub thread: NewThread,
    pub worktree: WorktreeSetupRequest,
    pub agent: AgentLaunch,
    pub bypass_modes: Vec<String>,
}

pub fn plan_launch(
    project: &ProjectItem,
    settings: &Settings,
    prompt: &str,
) -> Result<LaunchPlan, LaunchError> {
    let (drive, folder) = worktree_location(project, settings)?;
    let project_folder = project_folder_name(project);
    let repository = PathBuf::from(&project.repository_path);
    let slug = unique_slug(&message_slug(prompt), |candidate| {
        slug_taken(
            project,
            &repository,
            &drive,
            &folder,
            &project_folder,
            candidate,
        )
    });
    let worktree_path = resolve_worktree_directory(
        &drive,
        &folder,
        &project_folder,
        None,
        &JobKind::Branch(slug.clone()),
    )?
    .ok_or(WorktreeError::EmptyLeafName)?;
    let shared_target = resolve_project_worktree_root(&drive, &folder, &project_folder, None)?
        .join(SHARED_TARGET_FOLDER);
    let branch_name = format!("vel/{slug}");
    let (agent, bypass_modes) = agent_launch(settings.providers.default_provider)?;
    let share_cache = settings.worktrees.share_rust_build_cache;
    Ok(LaunchPlan {
        project: project.id,
        prompt: prompt.to_owned(),
        thread: new_thread(settings, prompt, &branch_name, &worktree_path),
        worktree: WorktreeSetupRequest {
            repository_root: repository,
            worktree_path,
            branch_name: Some(branch_name),
            base_ref: base_branch(project),
            settings: GitWorktreeSettings {
                delete_branch_on_merge: settings.worktrees.delete_local_branch_on_merge,
                share_build_cache: share_cache,
                copy_untracked_files: true,
                run_setup_command: true,
            },
            // Settings → Worktrees → New worktree setup is not saved yet (#22): F5's defaults.
            untracked_files: DEFAULT_UNTRACKED_FILES.into(),
            setup_command: DEFAULT_SETUP_COMMAND.into(),
            shared_cargo_target_dir: shared_target.clone(),
        },
        agent: with_shared_target(agent, share_cache.then_some(shared_target)),
        bypass_modes,
    })
}

fn new_thread(settings: &Settings, prompt: &str, branch: &str, worktree: &Path) -> NewThread {
    NewThread {
        title: fallback_title(prompt),
        branch_name: branch.to_owned(),
        worktree_path: worktree.display().to_string(),
        provider: settings.providers.default_provider,
        model_name: settings
            .providers
            .default_model
            .clone()
            .unwrap_or_else(|| AGENT_DEFAULT_MODEL.into()),
        // Read from the agent's config options once the model menu exists (F15).
        model_supports_fast: false,
        effort: settings.providers.default_effort,
        preferred_speed: settings.providers.default_speed,
    }
}

/// The command for each provider's ACP agent (§3.2) and the ids of its session modes that
/// skip permission checks (Rule 1). Antigravity's agent is still open (Q14).
pub fn agent_launch(provider: ProviderKind) -> Result<(AgentLaunch, Vec<String>), LaunchError> {
    let (program, args, bypass): (&str, &[&str], &[&str]) = match provider {
        ProviderKind::Claude => ("claude-code-acp", &[], &["bypassPermissions"]),
        ProviderKind::Codex => ("codex-acp", &[], &["full-access"]),
        ProviderKind::Kimi => ("kimi", &["--acp"], &[]),
        ProviderKind::Antigravity => return Err(LaunchError::ProviderNotConfirmed("Antigravity")),
    };
    Ok((
        AgentLaunch {
            program: PathBuf::from(program),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
            env: Vec::new(),
        },
        bypass.iter().map(|mode| (*mode).to_owned()).collect(),
    ))
}

/// Agents build with the project's shared Rust cache, as the setup command did (F5).
fn with_shared_target(mut agent: AgentLaunch, shared_target: Option<PathBuf>) -> AgentLaunch {
    if let Some(target) = shared_target {
        agent
            .env
            .push(("CARGO_TARGET_DIR".into(), target.display().to_string()));
    }
    agent
}

/// `(drive, folder)` for new worktrees: Settings → Worktrees → Location when set, otherwise
/// the reference's `\velocity\worktrees` on the project's own drive.
fn worktree_location(
    project: &ProjectItem,
    settings: &Settings,
) -> Result<(String, String), LaunchError> {
    let root = match &settings.worktrees.location_root {
        Some(root) => root.clone(),
        None => format!(
            "{}{DEFAULT_WORKTREE_FOLDER}",
            drive_of(&project.repository_path)
                .ok_or_else(|| LaunchError::UnknownDrive(project.repository_path.clone()))?
        ),
    };
    let drive = drive_of(&root).ok_or_else(|| LaunchError::UnknownDrive(root.clone()))?;
    let folder = root
        .trim_start_matches(r"\\?\")
        .get(2..)
        .unwrap_or_default()
        .to_owned();
    Ok((drive, folder))
}

fn drive_of(path: &str) -> Option<String> {
    let path = path.trim_start_matches(r"\\?\");
    let bytes = path.as_bytes();
    (bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':')
        .then(|| path[..2].to_ascii_uppercase())
}

/// The project's folder under the worktree root: its checkout's folder name, which is
/// always one path segment (a GitHub `owner/name` would not be).
fn project_folder_name(project: &ProjectItem) -> String {
    Path::new(&project.repository_path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| project.name.clone())
}

fn base_branch(project: &ProjectItem) -> String {
    if project.default_branch.trim().is_empty() {
        "master".into()
    } else {
        project.default_branch.clone()
    }
}

/// A slug is taken when a thread, a local branch or a folder already uses it (F5 Guards).
fn slug_taken(
    project: &ProjectItem,
    repository: &Path,
    drive: &str,
    folder: &str,
    project_folder: &str,
    slug: &str,
) -> bool {
    let branch = format!("vel/{slug}");
    let folder_taken = match resolve_worktree_directory(
        drive,
        folder,
        project_folder,
        None,
        &JobKind::Branch(slug.to_owned()),
    ) {
        Ok(path) => path.is_some_and(|path| path.exists()),
        // A reserved name such as `workspace` moves on to the next suffix.
        Err(WorktreeError::ReservedLeafName(_)) => true,
        // Other errors (a path too long) are not cured by a suffix; planning reports them.
        Err(_) => false,
    };
    folder_taken
        || project
            .threads
            .iter()
            .any(|thread| thread.branch_name == branch)
        || local_branch_exists(repository, &branch)
}
