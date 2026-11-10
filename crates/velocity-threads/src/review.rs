//! Planning one PR duty review (SPEC.md F7 PR duty, F5 `review-pr-<n>` worktrees): where its
//! worktree goes, which agent runs it, and the PR duty MCP server it records decisions with.

use std::path::{Path, PathBuf};

use agent_client_protocol::v1::schema::{McpServer, McpServerStdio};
use velocity_acp::client::{AgentLaunch, SessionSetup};
use velocity_core::duty::DutyMergeMethod;
use velocity_git::{JobKind, WorktreeError, WorktreeSetupRequest};
use velocity_store::duty::{DutyAgentItem, PrDutyRoster};

use crate::LaunchError;
use crate::launch::{
    Location, SHARED_TARGET_FOLDER, agent_launch, base_branch, setup_settings, with_shared_target,
};

/// The command-line switch that makes Velocity's own executable serve the PR duty MCP server,
/// so the server ships inside the app and needs no separate install.
pub const PR_DUTY_SERVER_SWITCH: &str = "--pr-duty-mcp";

/// Everything one review needs before anything runs.
#[derive(Clone, Debug)]
pub struct ReviewPlan {
    pub project: u32,
    pub nickname: String,
    pub number: u32,
    pub base_branch: String,
    pub merge_method: DutyMergeMethod,
    pub worktree: WorktreeSetupRequest,
    pub agent: AgentLaunch,
    pub bypass_modes: Vec<String>,
}

/// Plans `agent`'s review of PR `number` in the PR's `review-pr-<n>` worktree, made at
/// `base_commit` before the PR head is checked out over it.
pub fn plan_review(
    roster: &PrDutyRoster,
    agent: &DutyAgentItem,
    number: u32,
    base_commit: &str,
) -> Result<ReviewPlan, LaunchError> {
    let location = Location::new(&roster.project, &roster.worktrees);
    let worktree_path = location
        .job_worktree(&JobKind::DutyReview(u64::from(number)))?
        .ok_or(WorktreeError::EmptyLeafName)?;
    let shared_target = location.project_root()?.join(SHARED_TARGET_FOLDER);
    let preferences = &roster.worktrees;
    let worktree = WorktreeSetupRequest::detached(
        PathBuf::from(&roster.project.repository_path),
        worktree_path,
        base_commit.to_owned(),
        setup_settings(preferences),
        preferences.copy_files.clone(),
        preferences.setup_command.clone(),
        shared_target.clone(),
    );
    let (launch, bypass_modes) = agent_launch(agent.provider);
    Ok(ReviewPlan {
        project: roster.project.id,
        nickname: agent.nickname.clone(),
        number,
        base_branch: base_branch(&roster.project),
        merge_method: roster.rules.merge_method,
        worktree,
        agent: with_shared_target(
            launch,
            preferences.share_build_cache.then_some(shared_target),
        ),
        bypass_modes,
    })
}

/// Where the PR duty MCP server records decisions and reports them.
#[derive(Clone, Copy, Debug)]
pub struct ReviewServer<'a> {
    /// Velocity's own executable.
    pub executable: &'a Path,
    /// The app data folder holding the project's AIRC channel (the approval record).
    pub app_data: &'a Path,
    pub report: &'a Path,
}

/// The session a review runs: unrestricted like every agent (Rule 1), in the review worktree,
/// with the PR duty MCP server beside the shared servers.
pub fn review_session(plan: &ReviewPlan, worktree: PathBuf, server: ReviewServer) -> SessionSetup {
    let mut setup = SessionSetup::new(worktree.clone(), plan.bypass_modes.clone());
    let method = match plan.merge_method {
        DutyMergeMethod::Squash => "squash",
        DutyMergeMethod::Rebase => "rebase",
        DutyMergeMethod::MergeCommit => "merge",
    };
    let stdio = McpServerStdio::new("velocity-pr-duty", server.executable).args(vec![
        PR_DUTY_SERVER_SWITCH.to_owned(),
        worktree.display().to_string(),
        server.app_data.display().to_string(),
        plan.project.to_string(),
        plan.nickname.clone(),
        plan.number.to_string(),
        method.to_owned(),
        server.report.display().to_string(),
    ]);
    setup.shared_servers.push(McpServer::Stdio(stdio));
    setup
}
