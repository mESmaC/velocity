//! The code graph (F19): every agent's map of its project's code, so it finds what it needs
//! with fewer searches and reads only the lines that matter.

mod build;
mod cache;
mod extract;
mod files;
mod graph;
mod imports;
mod index;
mod language;
mod markdown;
mod mcp;
mod program;
mod query;
mod resolve;
mod search;
mod subsystems;
mod svelte;
mod text;
mod tools;

pub use extract::{FileParse, SymbolKind};
pub use graph::{Graph, Provenance, Record, RecordKind, Relation};
pub use index::{BuildReport, Index};
pub use language::Language;
pub use mcp::{SERVER_NAME, SERVER_SWITCH, serve, serve_stdio, server_arguments, warm};
pub use tools::Tools;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("code graph: {0}")]
    Io(#[from] std::io::Error),
    #[error("code graph cache: {0}")]
    Json(#[from] serde_json::Error),
    #[error("code graph: git failed: {0}")]
    Git(String),
    #[error("{0}")]
    Invalid(String),
}
