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

/// Rosen workspaces already carry their workspace note. Every other session gets the
/// queue note ahead of its own preface, including sessions resumed after a restart.
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
