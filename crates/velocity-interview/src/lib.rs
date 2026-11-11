//! The spec interview (SPEC.md F3): the first thread of every project. Its agent scans the
//! main checkout, asks one question at a time, writes SPEC.md, and on Lock the spec is
//! committed and the drafted tasks are published (Rule 2).
//!
//! Pure parts: `envelope` (the block each reply ends with), `prompts`, `spec_file` (the
//! proof list in SPEC.md), `live` (ACP updates → transcript blocks) and `turns` (how each
//! step changes the saved project). `service` and `actions` are the async and process edge.

#![forbid(unsafe_code)]

mod actions;
pub mod envelope;
pub mod live;
mod process;
pub mod prompts;
mod service;
pub mod spec_file;
pub mod turns;

pub use actions::handle_intent;
pub use service::{Host, InterviewKey, Interviews, overlay_live, refresh, start_interviews};

use velocity_store::StateStore;

/// The active project's saved thread, when it is a spec interview.
fn project_layout_thread(store: &StateStore) -> Option<usize> {
    let active = store.active_project_identifier()?;
    let project = store
        .state()
        .projects
        .iter()
        .find(|item| item.id == active)?;
    let thread = project.active_thread_id?;
    project
        .threads
        .iter()
        .any(|item| item.id == thread && item.interview.is_some())
        .then_some(thread as usize)
}

#[derive(Debug, thiserror::Error)]
pub enum InterviewError {
    #[error("{path} isn't written yet. Keep grilling, or ask the agent to write it: {source}")]
    NoSpec {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Could not write the proof list into {path}: {source}")]
    WriteSpec {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("Could not commit the spec: {0}")]
    Git(#[from] velocity_git::GitError),
    #[error("Lock stopped: {0}")]
    Stopped(String),
}
