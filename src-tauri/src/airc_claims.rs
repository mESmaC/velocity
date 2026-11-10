//! Thread claim wiring only; the channel crate owns claim decisions and storage.
use std::path::Path;
use tauri::{AppHandle, Manager};
use velocity_acp::client::{AgentEvent, SessionSetup};
use velocity_airc::{Channel, ClaimResult};
use velocity_threads::ThreadKey;

pub fn attach(
    application: &AppHandle,
    key: ThreadKey,
    setup: &SessionSetup,
    task: &str,
    branch: &str,
) -> Result<(), String> {
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let channel = Channel::open(&data, &key.0.to_string()).map_err(|error| error.to_string())?;
    let member = format!("thread-{}", key.1);
    channel
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
