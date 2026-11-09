//! Planning a build thread from the home composer (SPEC.md F4, F5, §3.2): its name, its
//! worktree from the project's base branch, and the ACP agent it runs.

use std::path::{Path, PathBuf};

use thiserror::Error;
use velocity_acp::client::AgentLaunch;
use velocity_core::worktree_settings::WorktreePreferences;
use velocity_core::worktree_view::split_location;
use velocity_git::{
    JobKind, WorktreeError, WorktreeSettings as GitWorktreeSettings, WorktreeSetupRequest,
    local_branch_exists, resolve_project_worktree_root, resolve_worktree_directory,
};
use velocity_store::threads::NewThread;
use velocity_store::{ProjectItem, ProviderKind, Settings};

use crate::naming::{fallback_title, message_slug, unique_slug};

/// The project's shared `CARGO_TARGET_DIR` (F5), beside its worktrees. Slugs never contain a
/// dot, so no worktree can take this name.
const SHARED_TARGET_FOLDER: &str = ".cargo-target";

#[derive(Debug, Error)]
pub enum LaunchError {
    #[error("{0} has no confirmed ACP agent yet (SPEC.md Q14)")]
    ProviderNotConfirmed(&'static str),
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

/// Plans a thread for `project` from Settings → Worktrees as the store reads it for that
/// project (`StateStore::worktree_context`), so threads land where the settings page's path
/// preview says they will (F5).
pub fn plan_launch(
    project: &ProjectItem,
    worktrees: &WorktreePreferences,
    settings: &Settings,
    prompt: &str,
) -> Result<LaunchPlan, LaunchError> {
    plan_named_launch(
        project,
        worktrees,
        settings,
        prompt,
        &fallback_title(prompt),
        &message_slug(prompt),
    )
}

/// Dispatched work keeps its item's title and never calls the title model (F4).
pub(crate) fn plan_named_launch(
    project: &ProjectItem,
    worktrees: &WorktreePreferences,
    settings: &Settings,
    prompt: &str,
    title: &str,
    requested_slug: &str,
) -> Result<LaunchPlan, LaunchError> {
    let location = Location::new(project, worktrees);
    let repository = PathBuf::from(&project.repository_path);
    let slug = unique_slug(requested_slug, |candidate| {
        slug_taken(project, &repository, &location, candidate)
    });
    let worktree_path = location
        .worktree(&slug)?
        .ok_or(WorktreeError::EmptyLeafName)?;
    let shared_target = location.project_root()?.join(SHARED_TARGET_FOLDER);
    let branch_name = format!("vel/{slug}");
    let (agent, bypass_modes) = agent_launch(settings.providers.default_provider)?;
    let share_cache = worktrees.share_build_cache;
    Ok(LaunchPlan {
        project: project.id,
        prompt: prompt.to_owned(),
        thread: NewThread::new(
            title.into(),
            branch_name.clone(),
            worktree_path.display().to_string(),
            settings,
        ),
        worktree: WorktreeSetupRequest {
            repository_root: repository,
            worktree_path,
            branch_name: Some(branch_name),
            base_ref: base_branch(project),
            settings: GitWorktreeSettings {
                delete_branch_on_merge: worktrees.delete_local_branch,
                share_build_cache: share_cache,
                // Clearing either field in Settings → Worktrees skips that step.
                copy_untracked_files: !worktrees.copy_files.trim().is_empty(),
                run_setup_command: !worktrees.setup_command.trim().is_empty(),
            },
            untracked_files: worktrees.copy_files.clone(),
            setup_command: worktrees.setup_command.clone(),
            shared_cargo_target_dir: shared_target.clone(),
        },
        agent: with_shared_target(agent, share_cache.then_some(shared_target)),
        bypass_modes,
    })
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

/// Where a project's worktrees go, by the same rule as Settings → Worktrees: the project's
/// own location when set, otherwise `<location>\<project name>`.
struct Location {
    drive: String,
    folder: String,
    project_name: String,
    project_override: Option<PathBuf>,
}

impl Location {
    fn new(project: &ProjectItem, worktrees: &WorktreePreferences) -> Self {
        let (drive, folder) = split_location(&worktrees.root);
        Location {
            drive,
            folder,
            project_name: project.name.clone(),
            project_override: worktrees.project_override.as_ref().map(PathBuf::from),
        }
    }

    fn project_root(&self) -> Result<PathBuf, WorktreeError> {
        resolve_project_worktree_root(
            &self.drive,
            &self.folder,
            &self.project_name,
            self.project_override.as_deref(),
        )
    }

    fn worktree(&self, slug: &str) -> Result<Option<PathBuf>, WorktreeError> {
        resolve_worktree_directory(
            &self.drive,
            &self.folder,
            &self.project_name,
            self.project_override.as_deref(),
            &JobKind::Branch(slug.to_owned()),
        )
    }
}

fn base_branch(project: &ProjectItem) -> String {
    if project.default_branch.trim().is_empty() {
        "master".into()
    } else {
        project.default_branch.clone()
    }
}

/// A slug is taken when a thread, a local branch or a folder already uses it (F5 Guards).
fn slug_taken(project: &ProjectItem, repository: &Path, location: &Location, slug: &str) -> bool {
    let branch = format!("vel/{slug}");
    let folder_taken = match location.worktree(slug) {
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
