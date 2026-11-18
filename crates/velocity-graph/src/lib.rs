//! The code graph (F19): every agent's map of its project's code, so it finds what it needs
//! with fewer searches and reads only the lines that matter.

mod build;
mod cache;
pub mod coordination;
mod extract;
mod files;
mod graph;
mod imports;
mod index;
mod language;
mod lean;
mod markdown;
mod mcp;
mod program;
mod project_service;
mod query;
mod resolve;
mod rosen;
mod search;
pub mod status;
mod subsystems;
mod svelte;
mod text;
pub mod token_usage;
mod tools;
pub mod usage;

pub use extract::{FileParse, SymbolKind};
pub use graph::{Graph, Provenance, Record, RecordKind, Relation};
pub use index::{BuildReport, Index};
pub use language::Language;
pub use mcp::{
    SERVER_NAME, SERVER_SWITCH, WARM_SWITCH, serve, serve_stdio, server_arguments, task_context,
    warm, warm_stdio,
};
pub use project_service::{PROJECT_SWITCH, serve_project, warm_project};
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
