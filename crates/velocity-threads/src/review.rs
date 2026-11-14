//! Planning one PR duty review (SPEC.md F7 PR duty, F5 `review-pr-<n>` worktrees): where its
//! worktree goes, which agent runs it, and the PR duty MCP server it records decisions with.

use std::path::{Path, PathBuf};

use agent_client_protocol::v1::schema::{McpServer, McpServerStdio};
use velocity_acp::RunSettings;
use velocity_acp::client::{AgentLaunch, SessionSetup};
use velocity_core::duty::DutyMergeMethod;
use velocity_core::pr_review::ReviewSubject;
use velocity_git::{JobKind, WorktreeError, WorktreeSetupRequest};
use velocity_store::duty::{DutyAgentItem, PrDutyRoster};

use crate::LaunchError;
use crate::launch::{
    Location, SHARED_TARGET_FOLDER, agent_launch, base_branch, setup_settings, with_compiler_cache,
};

/// The command-line switch that makes Velocity's own executable serve the PR duty MCP server,
/// so the server ships inside the app and needs no separate install.
pub const PR_DUTY_SERVER_SWITCH: &str = "--pr-duty-mcp";

/// Follows the server's arguments, with the review's base commit, when the review is by
/// reading (F7 "Checks before merge").
pub const READING_REVIEW_ARGUMENT: &str = "reading";

/// Everything one review needs before anything runs.
#[derive(Clone, Debug)]
pub struct ReviewPlan {
    pub project: u32,
    pub nickname: String,
    pub subject: ReviewSubject,
    pub base_branch: String,
    pub merge_method: DutyMergeMethod,
    pub worktree: WorktreeSetupRequest,
    pub agent: AgentLaunch,
    pub bypass_modes: Vec<String>,
    /// The model, effort and speed the review runs on (F7, F15).
    pub run_settings: RunSettings<String>,
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
    let subject = ReviewSubject::PullRequest(number);
    plan_in(roster, agent, subject, worktree_path, base_commit)
}

/// Plans `agent`'s review of Rosen change `change`, which thread `thread` works on, in the
/// project's `review-change-<thread>` folder. Rosen writes the change's merge result there:
/// a plain build tree, not a git worktree.
pub fn plan_change_review(
    roster: &PrDutyRoster,
    agent: &DutyAgentItem,
    thread: u32,
    change: &str,
) -> Result<ReviewPlan, LaunchError> {
    let location = Location::new(&roster.project, &roster.worktrees);
    let folder = location
        .project_root()?
        .join(format!("review-change-{thread}"));
    let subject = ReviewSubject::Change {
        thread,
        change: change.to_owned(),
    };
    plan_in(roster, agent, subject, folder, "")
}

fn plan_in(
    roster: &PrDutyRoster,
    agent: &DutyAgentItem,
    subject: ReviewSubject,
    worktree_path: PathBuf,
    base_commit: &str,
) -> Result<ReviewPlan, LaunchError> {
    let location = Location::new(&roster.project, &roster.worktrees);
    let shared_target = location.project_root()?.join(SHARED_TARGET_FOLDER);
    let preferences = &roster.worktrees;
    let worktree = WorktreeSetupRequest::detached(
        PathBuf::from(&roster.project.repository_path),
        worktree_path,
        base_commit.to_owned(),
        setup_settings(preferences),
        preferences.copy_files.clone(),
        preferences.setup_command.clone(),
        shared_target,
    );
    let (launch, bypass_modes) = agent_launch(&roster.settings, agent.provider)?;
    Ok(ReviewPlan {
        project: roster.project.id,
        nickname: agent.nickname.clone(),
        subject,
        base_branch: base_branch(&roster.project),
        merge_method: roster.rules.merge_method,
        worktree,
        agent: with_compiler_cache(launch, preferences.share_build_cache),
        bypass_modes,
        run_settings: crate::titles::duty_run_settings(&roster.settings, agent),
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
    /// The base commit a review by reading reads its diff against; `None` for a review that
    /// runs the local checks.
    pub reading_base: Option<&'a str>,
}

/// Where PR `number`'s `review-pr-<n>` worktree is kept, whether or not it exists now.
pub fn review_worktree(roster: &PrDutyRoster, number: u32) -> Option<PathBuf> {
    Location::new(&roster.project, &roster.worktrees)
        .job_worktree(&JobKind::DutyReview(u64::from(number)))
        .ok()
        .flatten()
}

/// The session a review runs: unrestricted like every agent (Rule 1), in the review worktree,
/// with the PR duty MCP server beside the shared servers.
pub fn review_session(plan: &ReviewPlan, worktree: PathBuf, server: ReviewServer) -> SessionSetup {
    let mut setup = SessionSetup::new(worktree.clone(), plan.bypass_modes.clone())
        .with_run_settings(plan.run_settings.clone());
    let method = match plan.merge_method {
        DutyMergeMethod::Squash => "squash",
        DutyMergeMethod::Rebase => "rebase",
        DutyMergeMethod::MergeCommit => "merge",
    };
    let number = plan.subject.number().unwrap_or_default();
    let mut arguments = vec![
        PR_DUTY_SERVER_SWITCH.to_owned(),
        worktree.display().to_string(),
        server.app_data.display().to_string(),
        plan.project.to_string(),
        plan.nickname.clone(),
        number.to_string(),
        method.to_owned(),
        server.report.display().to_string(),
    ];
    if let Some(base_commit) = server.reading_base {
        arguments.extend([READING_REVIEW_ARGUMENT.to_owned(), base_commit.to_owned()]);
    }
    let stdio = McpServerStdio::new("velocity-pr-duty", server.executable).args(arguments);
    setup.shared_servers.push(McpServer::Stdio(stdio));
    setup
}

/// The session a Rosen change's review runs: in its review folder, with PR duty's Rosen MCP
/// server, which reads its change and reviewer from `launch_file`.
pub fn change_review_session(
    plan: &ReviewPlan,
    executable: &Path,
    launch_file: &Path,
) -> SessionSetup {
    let mut setup = SessionSetup::new(
        plan.worktree.worktree_path.clone(),
        plan.bypass_modes.clone(),
    )
    .with_run_settings(plan.run_settings.clone());
    let stdio = McpServerStdio::new("velocity-rosen-duty", executable).args(vec![
        crate::ROSEN_DUTY_SERVER_SWITCH.to_owned(),
        launch_file.display().to_string(),
    ]);
    setup.shared_servers.push(McpServer::Stdio(stdio));
    setup
}
