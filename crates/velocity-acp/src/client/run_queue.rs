//! Queue instructions belong at the ACP edge so interviews and helper agents get the
//! same machine-wide queue as build threads, whatever their project's AGENTS.md says.

use super::AgentLaunch;
use agent_client_protocol::v1::schema::ContentBlock;
use std::ffi::{OsStr, OsString};

/// Used for executable lookup as well as the environment: on Windows changing a child's
/// PATH alone does not make CreateProcess find a bundled `rosen.exe` by its bare name.
pub(super) fn search_path(existing: Option<OsString>) -> Option<OsString> {
    let path = existing.as_deref().unwrap_or_else(|| OsStr::new(""));
    velocity_rosen::path_with_rosen(&path.to_string_lossy())
        .map(Into::into)
        .or(existing)
}

pub(super) fn prompt_content(preface: Option<String>, prompt: String) -> Vec<ContentBlock> {
    preface
        .map(ContentBlock::from)
        .into_iter()
        .chain(std::iter::once(ContentBlock::from(prompt)))
        .collect()
}

/// Antigravity runs long shell commands in the background; unlike Velocity's own queued
/// runs, those commands never wake the agent once its turn ends (#544).
pub(super) const ANTIGRAVITY_BACKGROUND_NOTE: &str = "A shell command still running in the \
    background when your turn ends will not wake you, so wait for it (or check its status) \
    before ending your turn. Only `queue_run`, `preflight` and `workflow_run` results from \
    `velocity-run` come back as a new turn.";

/// Fallback preface when `velocity-run` is unavailable on a non-Rosen session.
pub(super) fn preface(launch: &AgentLaunch, previous: Option<String>) -> Option<String> {
    if launch
        .env
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("ROSEN_CHANGE"))
        || velocity_rosen::path_with_rosen("").is_none()
    {
        return previous;
    }
    let note = velocity_rosen::run_queue_note();
    Some(match previous {
        Some(previous) => format!("{note}\n\n{previous}"),
        None => note.to_owned(),
    })
}

/// Provider-specific launch note when the provider requires one beyond AGENTS.md.
pub(super) fn provider_note(launch: &AgentLaunch) -> Option<&'static str> {
    (launch.own_config == Some(crate::Provider::Antigravity)
        || launch.concurrency_provider == Some(crate::Provider::Antigravity))
    .then_some(ANTIGRAVITY_BACKGROUND_NOTE)
}

/// Provider-specific launch instructions delivered ahead of the session's first prompt.
pub(super) fn append_provider_preface(
    launch: &AgentLaunch,
    previous: Option<String>,
) -> Option<String> {
    let Some(note) = provider_note(launch) else {
        return previous;
    };
    let line = format!("[Velocity] {note}");
    Some(match previous {
        Some(previous) => format!("{previous}\n\n{line}"),
        None => line,
    })
}
