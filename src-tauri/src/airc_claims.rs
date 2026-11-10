//! Thread claim wiring only; the channel crate owns claim decisions and storage.
use agent_client_protocol::v1::schema::{McpServer, McpServerStdio};
use std::path::Path;
use tauri::{AppHandle, Manager};
use velocity_acp::client::{AgentEvent, SessionSetup};
use velocity_airc::{Channel, ClaimResult};
use velocity_threads::ThreadKey;

pub const SERVER_SWITCH: &str = "--airc-mcp";

pub fn serve() -> anyhow::Result<bool> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    if arguments
        .first()
        .is_none_or(|argument| argument != SERVER_SWITCH)
    {
        return Ok(false);
    }
    if arguments.len() != 5 {
        anyhow::bail!("--airc-mcp needs app-data, project, member and nick");
    }
    let channel = Channel::open(Path::new(&arguments[1]), &arguments[2].to_string_lossy())?;
    velocity_airc::serve(
        &channel,
        &arguments[3].to_string_lossy(),
        &arguments[4].to_string_lossy(),
        std::io::stdin().lock(),
        std::io::stdout().lock(),
    )?;
    Ok(true)
}

pub fn attach(
    application: &AppHandle,
    key: ThreadKey,
    setup: &mut SessionSetup,
    task: &str,
    branch: &str,
) -> Result<(), String> {
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let channel = Channel::open(&data, &key.0.to_string()).map_err(|error| error.to_string())?;
    let member = format!("thread-{}", key.1);
    let nick = channel
        .join(&member, &setup.worktree.display().to_string())
        .map_err(|error| error.to_string())?;
    match channel
        .claim(&member, task, branch, Vec::new())
        .map_err(|error| error.to_string())?
    {
        ClaimResult::Granted { .. } => {}
        ClaimResult::Refused { holder, .. } => {
            return Err(format!("Piece already claimed by {holder}"));
        }
    }
    let executable = std::env::current_exe().map_err(|error| error.to_string())?;
    setup.airc = Some(McpServer::Stdio(
        McpServerStdio::new("velocity-airc", executable).args(vec![
            SERVER_SWITCH.into(),
            data.display().to_string(),
            key.0.to_string(),
            member,
            nick,
        ]),
    ));
    Ok(())
}

pub fn observe(
    application: &AppHandle,
    key: ThreadKey,
    worktree: &Path,
    event: &AgentEvent,
) -> Result<(), String> {
    let paths = match event {
        AgentEvent::FileWritten { path } => vec![path.clone()],
        AgentEvent::Transcript { update, .. } => match update {
            agent_client_protocol::v1::schema::SessionUpdate::ToolCall(call) => call
                .locations
                .iter()
                .map(|location| location.path.clone())
                .collect(),
            agent_client_protocol::v1::schema::SessionUpdate::ToolCallUpdate(call) => call
                .fields
                .locations
                .iter()
                .flatten()
                .map(|location| location.path.clone())
                .collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    };
    if paths.is_empty() {
        return Ok(());
    }
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let channel = Channel::open(&data, &key.0.to_string()).map_err(|error| error.to_string())?;
    for path in paths {
        let path = path.strip_prefix(worktree).unwrap_or(&path);
        channel
            .observe_file(&format!("thread-{}", key.1), &path.display().to_string())
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

pub fn release(application: &AppHandle, key: ThreadKey) -> Result<(), String> {
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let channel = Channel::open(&data, &key.0.to_string()).map_err(|error| error.to_string())?;
    let member = format!("thread-{}", key.1);
    for claim in channel.claims().map_err(|error| error.to_string())? {
        if claim.member == member {
            channel
                .release(&member, &claim.task)
                .map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}
