//! The code graph (F19): every agent's map of its project's code, so it finds what it needs
//! with fewer searches and reads only the lines that matter.

mod build;
mod cache;
pub mod command_line;
pub mod coordination;
mod extract;
mod files;
mod git_tree;
mod graph;
mod imports;
mod index;
mod language;
mod lean;
mod markdown;
mod mcp;
mod program;
mod project_service;
mod projects;
mod query;
mod resolve;
mod rosen;
mod search;
pub mod signature_impact;
mod spec_outline;
pub mod status;
mod subsystems;
mod svelte;
mod text;
pub mod token_usage;
mod tools;
pub mod usage;
mod workspace_search;

pub use extract::{FileParse, SymbolKind};
pub use graph::{Graph, Provenance, Record, RecordKind, Relation};
pub use index::{BuildReport, Index};
pub use language::Language;
pub use mcp::{
    INSTRUCTIONS, LAZY_FLAG, ROLE_FLAG, SERVER_NAME, SERVER_SWITCH, WARM_SWITCH, serve,
    serve_stdio, server_arguments, task_context, warm, warm_stdio, workspace_map,
};
pub use project_service::{
    PROJECT_SWITCH, change_signature_warnings, dispatch_signature_warnings, mentioned_files,
    mentioned_paths, serve_project, warm_project,
};
pub use search::is_conversational_prompt;
pub use signature_impact::{
    AlteredSignature, ChangeStatus, ReferenceDirection, SignatureChangeKind, StaleReferenceWarning,
};
pub use tools::Tools;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("code graph: {0}")]
    Io(#[from] std::io::Error),
    #[error("code graph cache: {0}")]
    Json(#[from] serde_json::Error),
    #[error("code graph: git failed: {0}")]
    Git(String),
    #[error("code graph: Rosen failed: {0}")]
    Rosen(#[from] rosen_client::Error),
    #[error("{0}")]
    Invalid(String),
}
