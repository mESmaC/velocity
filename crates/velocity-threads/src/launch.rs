//! Planning a build thread from the home composer (SPEC.md F4, F5, §3.2): its name, its
//! worktree from the project's base branch, and the ACP agent it runs.

use std::path::{Path, PathBuf};

use thiserror::Error;
use velocity_acp::client::AgentLaunch;
use velocity_core::thread_titles::title_slug;
use velocity_core::worktree_settings::WorktreePreferences;
use velocity_core::worktree_view::split_location;
use velocity_git::{
    JobKind, WorktreeError, WorktreeName, WorktreeSettings as GitWorktreeSettings,
    WorktreeSetupRequest, local_branch_exists, resolve_project_worktree_root,
    resolve_worktree_directory,
};
use velocity_store::providers::ProviderUnavailable;
use velocity_store::threads::NewThread;
use velocity_store::{ProjectItem, ProviderKind, Settings};

use crate::naming::{fallback_title, message_slug, unique_slug};
use crate::titles::{TitlePlan, plan_title};

/// The folder setup plans name for the build cache (F5), beside the worktrees. Builds no
/// longer share a target folder (see [`with_compiler_cache`]); slugs never contain a dot, so
/// no worktree can take this name.
pub(crate) const SHARED_TARGET_FOLDER: &str = ".cargo-target";

#[derive(Debug, Error)]
pub enum LaunchError {
    #[error("every branch name for \"vel/{0}\" is already taken in this project")]
    NamesExhausted(String),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    #[error(transparent)]
    Provider(#[from] velocity_store::providers::ProviderUnavailable),
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
    /// The title model's request (F4), run while the worktree is checked out.
    pub title: TitlePlan,
    /// What naming the worktree from the title needs.
    pub naming: WorktreeNaming,
}

/// The project and its Settings → Worktrees as they were at launch, so the title's branch
/// and folder follow the same rules as the message slug's.
#[derive(Clone)]
pub struct WorktreeNaming {
    project: ProjectItem,
    worktrees: WorktreePreferences,
}

impl std::fmt::Debug for WorktreeNaming {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorktreeNaming")
            .field("project", &self.project.id)
            .finish_non_exhaustive()
    }
}

/// The branch and folder a model title names (F4): `vel/<title-slug>`, made unique the way
/// the message slug is. `None` when the names would not change or can't be planned (a path
/// too long), and the provisional names stay.
pub fn title_worktree_name(
    naming: &WorktreeNaming,
    current: &WorktreeSetupRequest,
    title: &str,
) -> Option<WorktreeName> {
    let location = Location::new(&naming.project, &naming.worktrees);
    let wanted = title_slug(title);
    if current.branch_name.as_deref() == Some(format!("vel/{wanted}").as_str()) {
        return None;
    }
    let slug = unique_slug(&wanted, |candidate| {
        slug_taken(
            &naming.project,
            &current.repository_root,
            &location,
            candidate,
        )
    })?;
    let worktree_path = location.worktree(&slug).ok()??;
    Some(WorktreeName {
        worktree_path,
        branch_name: format!("vel/{slug}"),
    })
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
    })
    .ok_or_else(|| LaunchError::NamesExhausted(requested_slug.to_owned()))?;
    let worktree_path = location
        .worktree(&slug)?
        .ok_or(WorktreeError::EmptyLeafName)?;
    let shared_target = location.project_root()?.join(SHARED_TARGET_FOLDER);
    let branch_name = format!("vel/{slug}");
    let thread = NewThread::new(
        title.into(),
        branch_name.clone(),
        worktree_path.display().to_string(),
        settings,
    );
    // The thread runs where its record says: the default, or the first signed-in provider
    // when you signed out of the default. Nothing starts when every provider is signed out.
    settings.ensure_new_work(thread.provider)?;
    let (agent, bypass_modes) = agent_launch(settings, thread.provider)?;
    let share_cache = worktrees.share_build_cache;
    Ok(LaunchPlan {
        project: project.id,
        prompt: prompt.to_owned(),
        thread,
        worktree: WorktreeSetupRequest {
            repository_root: repository,
            worktree_path,
            branch_name: Some(branch_name),
            base_ref: base_branch(project),
            settings: setup_settings(worktrees),
            untracked_files: worktrees.copy_files.clone(),
            setup_command: worktrees.setup_command.clone(),
            shared_cargo_target_dir: shared_target,
        },
        agent: with_compiler_cache(agent, share_cache),
        bypass_modes,
        title: plan_title(
            settings,
            PathBuf::from(&project.repository_path),
            prompt,
            fallback_title(prompt),
        ),
        naming: WorktreeNaming {
            project: project.clone(),
            worktrees: worktrees.clone(),
        },
    })
}

/// The command for each provider's ACP agent (§3.2) and the ids of its session modes that
/// skip permission checks (Rule 1): the built-in command, or the one you set in Settings →
/// Providers, or the command of an agent you added (F15). An agent you removed has none.
/// The launch carries Settings → Agents' shared MCP servers (§3.2 Tool access).
pub fn agent_launch(
    settings: &Settings,
    provider: ProviderKind,
) -> Result<(AgentLaunch, Vec<String>), ProviderUnavailable> {
    let command = settings
        .agent_command(provider)
        .map(|command| (PathBuf::from(&command.program), command.arguments.clone()));
    velocity_acp::configured_agent(crate::titles::provider(provider), command)
        .map(|mut agent| {
            // Every role's agent comes from here, so every one gets the shared servers.
            agent.launch.shared_servers = crate::shared_mcp::shared_mcp_servers(settings);
            (agent.launch, agent.bypass_modes)
        })
        .ok_or(ProviderUnavailable::Removed)
}

/// Settings → Worktrees as the worktree setup steps read them (F5).
pub(crate) fn setup_settings(worktrees: &WorktreePreferences) -> GitWorktreeSettings {
    GitWorktreeSettings {
        delete_branch_on_merge: worktrees.delete_local_branch,
        share_build_cache: worktrees.share_build_cache,
        // Clearing either field in Settings → Worktrees skips that step.
        copy_untracked_files: !worktrees.copy_files.trim().is_empty(),
        run_setup_command: !worktrees.setup_command.trim().is_empty(),
    }
}

/// Agents build with the shared compiler cache and their own worktree's `target` folder
/// (F5), as the setup command and the warm pool's prebuild do, so a prebuilt `target` stays
/// fresh for the agent's first build.
pub(crate) fn with_compiler_cache(mut agent: AgentLaunch, share_build_cache: bool) -> AgentLaunch {
    if share_build_cache {
        agent.env.extend(velocity_git::compiler_cache_env());
    }
    agent
}

/// Where a project's worktrees go, by the same rule as Settings → Worktrees: the project's
/// own location when set, otherwise `<location>\<project name>`.
pub(crate) struct Location {
    drive: String,
    folder: String,
    project_name: String,
    project_override: Option<PathBuf>,
}

impl Location {
    pub(crate) fn new(project: &ProjectItem, worktrees: &WorktreePreferences) -> Self {
        let (drive, folder) = split_location(&worktrees.root);
        Location {
            drive,
            folder,
            project_name: project.name.clone(),
            project_override: worktrees.project_override.as_ref().map(PathBuf::from),
        }
    }

    pub(crate) fn project_root(&self) -> Result<PathBuf, WorktreeError> {
        resolve_project_worktree_root(
            &self.drive,
            &self.folder,
            &self.project_name,
            self.project_override.as_deref(),
        )
    }

    fn worktree(&self, slug: &str) -> Result<Option<PathBuf>, WorktreeError> {
        self.job_worktree(&JobKind::Branch(slug.to_owned()))
    }

    pub(crate) fn job_worktree(
        &self,
        job: &JobKind<String, String>,
    ) -> Result<Option<PathBuf>, WorktreeError> {
        resolve_worktree_directory(
            &self.drive,
            &self.folder,
            &self.project_name,
            self.project_override.as_deref(),
            job,
        )
    }
}

pub(crate) fn base_branch(project: &ProjectItem) -> String {
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
