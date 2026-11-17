//! AIRC for every agent (F14, §3.2): Velocity's own executable serves each agent's AIRC MCP
//! connection, and every session Velocity starts is given it in `session/new`.

use anyhow::Context;
use std::path::Path;
use tauri::{AppHandle, Manager};
use velocity_acp::client::AircLaunch;
use velocity_airc::{SERVER_SWITCH, serve_stdio_with_issues};

/// `velocity --airc-mcp <app-data> <project-id> <member-id> <nick>`: serve one agent's AIRC
/// connection on stdin/stdout and exit. `false` when Velocity was started normally.
pub fn serve_airc_server() -> anyhow::Result<bool> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let Some((switch, rest)) = arguments.split_first() else {
        return Ok(false);
    };
    if switch.to_str() != Some(SERVER_SWITCH) {
        return Ok(false);
    }
    serve_stdio_with_issues(rest, velocity_threads::issue_activity::post_issue_update)
        .context("The AIRC MCP connection failed")?;
    Ok(true)
}

/// Records where agents' AIRC servers come from, before anything starts an agent.
pub fn initialize(application: &AppHandle) -> anyhow::Result<()> {
    application.manage(launch(application)?);
    Ok(())
}

pub fn launch(application: &AppHandle) -> anyhow::Result<AircLaunch> {
    let executable = std::env::current_exe().context("Could not find Velocity's executable")?;
    let app_data = application
        .path()
        .app_data_dir()
        .context("Could not resolve the application data directory")?;
    Ok(AircLaunch::new(executable, app_data))
}

/// The managed launch; `None` only before `initialize` ran.
pub fn managed(application: &AppHandle) -> Option<AircLaunch> {
    application
        .try_state::<AircLaunch>()
        .map(|launch| launch.inner().clone())
}

pub fn for_thread(
    application: &AppHandle,
    key: velocity_threads::ThreadKey,
    path: &Path,
    thread: &velocity_store::ThreadItem,
) -> Option<agent_client_protocol::v1::schema::McpServer> {
    managed(application)
        .map(|airc| airc.thread_with_origin(key.0, key.1, path, thread.started_by_dispatch))
}
