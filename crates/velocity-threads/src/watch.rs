//! Planning one Main watcher watch (SPEC.md F7 Main watcher, F5 `<agent>-sweep` worktrees):
//! a detached checkout of the base branch's new tip, the watcher's own agent, and the two
//! MCP servers it records through: the watch server for its verdicts and the scout server
//! for flaky-test issues.

use std::path::{Path, PathBuf};

use agent_client_protocol::v1::schema::{McpServer, McpServerStdio};
use velocity_acp::client::SessionSetup;
use velocity_store::duty::{DutyAgentItem, WatchRoster};

use crate::LaunchError;
use crate::sweep::{SCOUT_SERVER_SWITCH, SweepPlan, plan_duty_checkout};

/// The command-line switch that makes Velocity's own executable serve the watch MCP server.
pub const WATCH_SERVER_SWITCH: &str = "--watch-mcp";

/// Plans `watcher`'s watch worktree at `base_ref` and its agent from its own provider.
pub fn plan_watch(
    roster: &WatchRoster,
    watcher: &DutyAgentItem,
    base_ref: &str,
) -> Result<SweepPlan, LaunchError> {
    plan_duty_checkout(&roster.project, &roster.worktrees, watcher, base_ref)
}

/// Where a watch's servers write what they record, for the runner to read.
#[derive(Clone, Debug)]
pub struct WatchReports {
    /// Verdicts: green, broken, the fix or revert PR.
    pub verdicts: PathBuf,
    /// Flaky-test issues filed through the scout server.
    pub issues: PathBuf,
}

/// The session a watch runs: unrestricted like every agent (Rule 1), in the watch worktree,
/// with the watch and scout MCP servers beside the shared servers. `executable` is Velocity
/// itself, which serves both.
pub fn watch_session(
    plan: &SweepPlan,
    worktree: PathBuf,
    executable: &Path,
    app_data: &Path,
    tip: &str,
    reports: &WatchReports,
) -> SessionSetup {
    let mut setup = SessionSetup::new(worktree.clone(), plan.bypass_modes.clone());
    let checkout = worktree.display().to_string();
    let watch = McpServerStdio::new("velocity-watch", executable).args(vec![
        WATCH_SERVER_SWITCH.to_owned(),
        checkout.clone(),
        app_data.display().to_string(),
        plan.project.to_string(),
        plan.nickname.clone(),
        plan.base_branch.clone(),
        tip.to_owned(),
        reports.verdicts.display().to_string(),
    ]);
    let scout = McpServerStdio::new("velocity-scout", executable).args(vec![
        SCOUT_SERVER_SWITCH.to_owned(),
        checkout,
        plan.nickname.clone(),
        reports.issues.display().to_string(),
    ]);
    setup.shared_servers.push(McpServer::Stdio(watch));
    setup.shared_servers.push(McpServer::Stdio(scout));
    setup
}
