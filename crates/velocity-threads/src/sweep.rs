//! Planning one scout sweep (SPEC.md F7 Scouts, F5 `<scout>-sweep` worktrees): where its
//! worktree goes, which agent runs it with which tools, and what its card says while it runs.

use std::path::{Path, PathBuf};

use agent_client_protocol::v1::schema::{McpServer, McpServerStdio, SessionUpdate};
use velocity_acp::RunSettings;
use velocity_acp::client::{AgentEvent, AgentLaunch, SessionSetup};
use velocity_core::scout_sweep::step_doing;
use velocity_core::worktree_settings::WorktreePreferences;
use velocity_git::{JobKind, WorktreeError, WorktreeSetupRequest};
use velocity_store::duty::DutyAgentItem;
use velocity_store::duty::ScoutRoster;
use velocity_store::{ProjectItem, Settings};

use crate::LaunchError;
use crate::launch::{
    Location, SHARED_TARGET_FOLDER, agent_launch, base_branch, setup_settings, with_compiler_cache,
};

/// The command-line switch that makes Velocity's own executable serve the scout MCP server,
/// so the server ships inside the app and needs no separate install.
pub const SCOUT_SERVER_SWITCH: &str = "--scout-mcp";

/// Everything one sweep needs before anything runs.
#[derive(Clone, Debug)]
pub struct SweepPlan {
    pub project: u32,
    pub nickname: String,
    pub base_branch: String,
    pub worktree: WorktreeSetupRequest,
    pub agent: AgentLaunch,
    pub bypass_modes: Vec<String>,
    /// The model, effort and speed the duty agent runs on (F7, F15).
    pub run_settings: RunSettings<String>,
}

/// Plans `scout`'s sweep worktree at `base_ref` and its agent from the scout's own provider.
pub fn plan_sweep(
    roster: &ScoutRoster,
    scout: &DutyAgentItem,
    base_ref: &str,
) -> Result<SweepPlan, LaunchError> {
    plan_duty_checkout(
        &roster.settings,
        (&roster.project, &roster.worktrees),
        scout,
        base_ref,
    )
}

/// Plans a duty agent's own detached checkout of `base_ref` (`<agent>-sweep`, F5), shared by
/// scout sweeps and the Main watcher's watches, and its agent from its own provider.
pub fn plan_duty_checkout(
    settings: &Settings,
    (project, preferences): (&ProjectItem, &WorktreePreferences),
    agent_item: &DutyAgentItem,
    base_ref: &str,
) -> Result<SweepPlan, LaunchError> {
    let location = Location::new(project, preferences);
    let worktree_path = location
        .job_worktree(&JobKind::ScoutSweep(agent_item.nickname.clone()))?
        .ok_or(WorktreeError::EmptyLeafName)?;
    let shared_target = location.project_root()?.join(SHARED_TARGET_FOLDER);
    let worktree = WorktreeSetupRequest::detached(
        PathBuf::from(&project.repository_path),
        worktree_path,
        base_ref.to_owned(),
        setup_settings(preferences),
        preferences.copy_files.clone(),
        preferences.setup_command.clone(),
        shared_target,
    );
    let (agent, bypass_modes) = agent_launch(settings, agent_item.provider)?;
    Ok(SweepPlan {
        project: project.id,
        nickname: agent_item.nickname.clone(),
        base_branch: base_branch(project),
        worktree,
        agent: with_compiler_cache(agent, preferences.share_build_cache),
        bypass_modes,
        run_settings: crate::titles::duty_run_settings(settings, agent_item),
    })
}

/// The session a sweep runs: unrestricted like every agent (Rule 1), in the sweep worktree,
/// with the scout MCP server beside the shared servers.
pub fn sweep_session(
    plan: &SweepPlan,
    worktree: PathBuf,
    scout_server: &Path,
    report: &Path,
) -> SessionSetup {
    let mut setup = SessionSetup::new(worktree.clone(), plan.bypass_modes.clone())
        .with_run_settings(plan.run_settings.clone());
    let server = McpServerStdio::new("velocity-scout", scout_server).args(vec![
        SCOUT_SERVER_SWITCH.to_owned(),
        worktree.display().to_string(),
        plan.nickname.clone(),
        report.display().to_string(),
    ]);
    setup.shared_servers.push(McpServer::Stdio(server));
    setup
}

/// What a scout's card says after this agent event, if it changes it: the step it is on.
pub fn sweep_doing(event: &AgentEvent) -> Option<String> {
    let update = match event {
        AgentEvent::Transcript { update, .. } | AgentEvent::SessionUpdate(update) => update,
        AgentEvent::ProviderQueued(true, provider) => {
            return Some(format!("queued \u{b7} {provider}"));
        }
        AgentEvent::ProviderQueued(false, _) => return Some("starting the agent".into()),
        _ => return None,
    };
    match update {
        SessionUpdate::ToolCall(call) => Some(step_doing(&call.title)),
        SessionUpdate::ToolCallUpdate(update) => update.fields.title.as_deref().map(step_doing),
        SessionUpdate::AgentThoughtChunk(_) => Some("thinking".into()),
        _ => None,
    }
}
