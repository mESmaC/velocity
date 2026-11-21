//! AIRC for every agent (F14, §3.2): Velocity's own executable serves each agent's AIRC MCP
//! connection, and every session Velocity starts is given it in `session/new`.

use anyhow::Context;
use std::path::Path;
use tauri::{AppHandle, Manager};
use velocity_acp::client::AircLaunch;
use velocity_airc::{SERVER_SWITCH, serve_stdio_with_handlers};

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
    serve_stdio_with_handlers(
        rest,
        velocity_threads::issue_activity::handle_issue_request,
        velocity_threads::next_work::handle_next_work,
    )
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
    _path: &Path,
    thread: &velocity_store::ThreadItem,
) -> Option<agent_client_protocol::v1::schema::McpServer> {
    managed(application).map(|airc| {
        airc.thread_member(
            key.0,
            &thread.airc_member(),
            &thread.agent_name(),
            thread.started_by_dispatch,
        )
    })
}

/// Opens a project's AIRC channel using the store's Rosen configuration so `Channel::open`
/// never reads or parses `state.json` from disk (#555).
pub(crate) fn open_project_channel(
    app_data: &Path,
    project: &velocity_store::ProjectItem,
) -> Result<velocity_airc::Channel, String> {
    velocity_airc::Channel::open_for_project(
        app_data,
        &project.id.to_string(),
        project.rosen_in_use(),
    )
    .map_err(|error| error.to_string())
}

/// Opens `project_identifier`'s AIRC channel using the Rosen setting stored in `ManagedStateStore` (#555).
pub(crate) fn open_channel(
    application: &AppHandle,
    project_identifier: u32,
) -> Result<velocity_airc::Channel, String> {
    let rosen = crate::threads::lock(
        application
            .state::<crate::state::ManagedStateStore>()
            .inner(),
        "persisted state",
    )?
    .project(project_identifier)
    .map(velocity_store::ProjectItem::rosen_in_use)
    .unwrap_or(false);
    let app_data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    velocity_airc::Channel::open_for_project(&app_data, &project_identifier.to_string(), rosen)
        .map_err(|error| error.to_string())
}
