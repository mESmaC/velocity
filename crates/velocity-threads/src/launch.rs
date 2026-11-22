//! Planning a build thread from the home composer (SPEC.md F4, F5, §3.2): its name, its
//! worktree from the project's base branch, and the ACP agent it runs.

use std::path::{Path, PathBuf};

use thiserror::Error;
use velocity_acp::client::AgentLaunch;
use velocity_core::worktree_settings::WorktreePreferences;
use velocity_core::worktree_view::split_location;
use velocity_git::{
    JobKind, WorktreeError, WorktreeSettings as GitWorktreeSettings, WorktreeSetupRequest,
    resolve_project_worktree_root, resolve_worktree_directory,
};
use velocity_store::providers::ProviderUnavailable;
use velocity_store::threads::{NewThread, ThreadModel};
use velocity_store::{ProjectItem, ProviderKind, Settings};

use crate::naming::{fallback_title, message_slug, unique_slug};
use crate::titles::{TitlePlan, plan_title};

pub(crate) fn add_review_handoff(
    setup: &mut velocity_acp::client::SessionSetup,
    app_data: &Path,
    project: u32,
    nickname: &str,
) {
    let member = velocity_acp::client::duty_member(nickname);
    let state = airc_work_handoff(app_data, project, &[member]);
    let workspace = setup.worktree.display();
    let handoff = format!("Velocity work handoff\nWorkspace: {workspace}\n{state}");
    setup.preface = Some(match setup.preface.take() {
        Some(preface) => format!("{preface}\n\n{handoff}"),
        None => handoff,
    });
}

/// Read current claims and unresolved dependency waits for a fresh ACP session.
pub fn airc_work_handoff(app_data: &Path, project: u32, members: &[String]) -> String {
    airc_work_handoff_for_project(app_data, project, false, members)
}

/// Like [`airc_work_handoff`], passing the store's Rosen configuration explicitly (#555).
pub fn airc_work_handoff_for_project(
    app_data: &Path,
    project: u32,
    rosen: bool,
    members: &[String],
) -> String {
    let read = || {
        let channel =
            velocity_airc::Channel::open_for_project(app_data, &project.to_string(), rosen)?;
        Ok::<_, velocity_airc::Error>((channel.claims()?, channel.dependency_waits()?))
    };
    let Ok((claims, waits)) = read() else {
        return "AIRC state is unavailable. Check airc_claims and the AIRC feed.".into();
    };
    let held: Vec<_> = claims
        .iter()
        .filter(|claim| members.contains(&claim.member))
        .collect();
    let mut lines = Vec::new();
    for claim in held.iter().take(4) {
        let files = claim
            .files
            .iter()
            .take(4)
            .map(|file| handoff_line(file))
            .collect::<Vec<_>>()
            .join(", ");
        if files.is_empty() {
            lines.push(format!("Held claim: {}.", handoff_line(claim.key())));
        } else {
            lines.push(format!(
                "Held claim: {}. Paths: {}.",
                handoff_line(claim.key()),
                files
            ));
        }
    }
    for wait in waits
        .iter()
        .filter(|wait| members.contains(&wait.member))
        .take(4)
    {
        lines.push(format!(
            "Open dependency: {} holds {}.",
            handoff_line(&wait.holder_nick),
            handoff_line(&wait.file)
        ));
    }
    if lines.is_empty() {
        lines.push("No AIRC claims or dependency waits remain.".into());
    } else {
        lines.push("See airc_claims and the AIRC feed for the full state.".into());
    }
    lines.join("\n")
}

pub fn handoff_line(text: &str) -> String {
    text.chars()
        .take(160)
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect()
}

/// The folder setup plans name for the build cache (F5), beside the worktrees. Builds no
/// longer share a target folder (see [`with_compiler_cache`]); slugs never contain a dot, so
/// no worktree can take this name.
pub(crate) const SHARED_TARGET_FOLDER: &str = ".cargo-target";

#[derive(Debug, Error)]
pub enum LaunchError {
    #[error("launch settings need a worktree root; read a Settings snapshot from StateStore")]
    MissingWorktreeRoot,
    #[error("every branch name for \"vel/{0}\" is already taken in this project")]
    NamesExhausted(String),
    #[error(transparent)]
    Worktree(#[from] WorktreeError),
    #[error(transparent)]
    Provider(#[from] velocity_store::providers::ProviderUnavailable),
}

/// Everything a read-only question thread needs: no workspace, change, claim or landable status (#533).
#[derive(Clone, Debug)]
pub struct QuestionPlan {
    pub project: u32,
    pub prompt: String,
    pub repository: PathBuf,
    pub thread: NewThread,
    pub agent: AgentLaunch,
    pub bypass_modes: Vec<String>,
    pub title: TitlePlan,
}

/// Plans a read-only question thread in the project repository with no workspace or change (#533).
pub fn plan_question_launch(
    project: &ProjectItem,
    settings: &Settings,
    prompt: &str,
    model: ThreadModel,
) -> Result<QuestionPlan, LaunchError> {
    let repository = PathBuf::from(&project.repository_path);
    let mut thread = NewThread::with_model(
        fallback_title(prompt),
        String::new(),
        String::new(),
        settings,
        model,
    );
    thread.role = velocity_core::tool_policy::ThreadRole::Question;
    settings.ensure_new_work(thread.provider)?;
    let (mut agent, mut bypass_modes) = agent_launch(settings, thread.provider)?;
    velocity_acp::providers::apply_read_only(&mut agent, &mut bypass_modes);
    Ok(QuestionPlan {
        project: project.id,
        prompt: prompt.to_owned(),
        repository: repository.clone(),
        thread,
        agent,
        bypass_modes,
        title: plan_title(settings, repository, prompt, fallback_title(prompt)),
    })
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
    /// The title model's request (F4), run while the workspace is created.
    pub title: TitlePlan,
}

/// Compatibility entry point for saved Settings snapshots (F5/F13).
/// Use the explicit preferences entry point when the store context is available.
pub fn plan_launch(
    project: &ProjectItem,
    settings: &Settings,
    prompt: &str,
) -> Result<LaunchPlan, LaunchError> {
    // Raw defaults have no store directory. Refuse to invent a different default root.
    if settings.worktrees.location_root.is_none() {
        return Err(LaunchError::MissingWorktreeRoot);
    }
    let preferences =
        velocity_store::worktree_settings::project_preferences(project, settings, Path::new(""));
    plan_launch_with_preferences(project, &preferences, settings, prompt)
}

/// Plans a thread for `project` from Settings → Worktrees as the store reads it for that
/// project (`StateStore::worktree_context`), so threads land where the settings page's path
/// preview says they will (F5).
pub fn plan_launch_with_preferences(
    project: &ProjectItem,
    worktrees: &WorktreePreferences,
    settings: &Settings,
    prompt: &str,
) -> Result<LaunchPlan, LaunchError> {
    plan_composer_launch(
        project,
        worktrees,
        settings,
        prompt,
        ThreadModel::from_settings(settings),
    )
}

pub fn plan_composer_launch(
    project: &ProjectItem,
    worktrees: &WorktreePreferences,
    settings: &Settings,
    prompt: &str,
    model: ThreadModel,
) -> Result<LaunchPlan, LaunchError> {
    plan_with_model(
        project,
        worktrees,
        settings,
        prompt,
        &fallback_title(prompt),
        &message_slug(prompt),
        model,
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
    plan_with_model(
        project,
        worktrees,
        settings,
        prompt,
        title,
        requested_slug,
        ThreadModel::from_settings(settings),
    )
}

fn plan_with_model(
    project: &ProjectItem,
    worktrees: &WorktreePreferences,
    settings: &Settings,
    prompt: &str,
    title: &str,
    requested_slug: &str,
    model: ThreadModel,
) -> Result<LaunchPlan, LaunchError> {
    let location = Location::new(project, worktrees);
    let repository = PathBuf::from(&project.repository_path);
    let slug = unique_slug(requested_slug, |candidate| {
        slug_taken(project, &location, candidate)
    })
    .ok_or_else(|| LaunchError::NamesExhausted(requested_slug.to_owned()))?;
    let worktree_path = location
        .worktree(&slug)?
        .ok_or(WorktreeError::EmptyLeafName)?;
    let shared_target = location.project_root()?.join(SHARED_TARGET_FOLDER);
    let branch_name = format!("vel/{slug}");
    let thread = NewThread::with_model(
        title.into(),
        branch_name.clone(),
        worktree_path.display().to_string(),
        settings,
        model,
    );
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
    let account = settings.launch_account(
        provider,
        settings.default_account(provider),
        velocity_store::providers::unix_now(),
    );
    agent_launch_for_account(settings, provider, account)
}

pub fn agent_launch_for_account(
    settings: &Settings,
    provider: ProviderKind,
    account: u32,
) -> Result<(AgentLaunch, Vec<String>), ProviderUnavailable> {
    let command = settings
        .agent_command(provider)
        .map(|command| (PathBuf::from(&command.program), command.arguments.clone()));
    velocity_acp::configured_agent(crate::titles::provider(provider), command)
        .map(|mut agent| {
            if settings.agents.trim_agent_context {
                agent.trim_unused_context();
            }
            // Every role's agent comes from here, so every one gets the shared servers and
            // owner-approved environment pass-through list (#530).
            agent.launch.shared_servers = crate::shared_mcp::shared_mcp_servers(settings);
            agent.launch.env_passthrough = settings.providers.env_passthrough.clone();
            if let Some(profile) = settings
                .saved_account(provider, account)
                .filter(|profile| profile.id != 0)
            {
                agent.launch.use_account(profile.id, &profile.directory);
            }
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

/// The folder a project's workspaces are made in (Settings → Worktrees).
pub fn project_workspace_root(
    project: &ProjectItem,
    worktrees: &WorktreePreferences,
) -> Option<PathBuf> {
    Location::new(project, worktrees).project_root().ok()
}

pub(crate) fn base_branch(project: &ProjectItem) -> String {
    if project.default_branch.trim().is_empty() {
        "master".into()
    } else {
        project.default_branch.clone()
    }
}

/// A slug is taken when a thread or an existing folder already uses it (F5 Guards).
fn slug_taken(project: &ProjectItem, location: &Location, slug: &str) -> bool {
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
}
