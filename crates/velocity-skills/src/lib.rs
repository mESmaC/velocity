//! Velocity's skill pack library (SPEC.md §4 F18, §7 M12): clones and pins skill packs in
//! one machine-wide library, previews updates as a diff of `SKILL.md` files, scripts and data
//! before moving a pin, and configures `core.symlinks=true` and `PYTHONUTF8=1` on Windows.

#![forbid(unsafe_code)]

pub mod bundles;
pub mod catalog;
pub mod design_system;
pub mod details;
pub mod library;
pub mod manifest;
pub mod menu;
pub mod rea;
mod requirements;
pub mod settings;
pub mod suggestions;

use std::path::PathBuf;
use thiserror::Error;

pub use bundles::{
    GAMEDEV_ROUTER_BUNDLE, SkillBundleCount, SkillPackBundleCounts, bundle_of_skill,
    bundle_skill_counts, selection_includes,
};
pub use catalog::{
    SkillBundleDefinition, SkillPackCatalogEntry, built_in_catalog, find_built_in_pack,
    normalize_github_slug, pack_id_from_slug, resolve_pack_source, sanitize_pack_id,
};
pub use design_system::{
    DesignSystemStatus, design_system_master_path, design_system_query, design_system_rule,
    design_system_slug, design_system_status, generate_design_system, sync_design_system_rule,
};
pub use details::{SkillPackDetails, add_pack_details, pack_shared_servers};
pub use library::{
    InstalledSkillPack, SkillLibrary, SkillLibrarySnapshot, SkillPackDiffFile,
    SkillPackUpdatePreview, SkillPythonOutput, run_python_utf8,
};
pub use manifest::{
    DiscoveredSkill, SkillDiffCategory, classify_pack_path, discover_pack_skills,
    parse_skill_markdown,
};
pub use rea::{REA_SERVER_NAME, SkillMcpServer, rea_mcp_server};

#[derive(Debug, Error)]
pub enum SkillLibraryError {
    #[error("skill library I/O failed at `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("skill library registry format error: {0}")]
    RegistryFormat(#[from] serde_json::Error),
    #[error("invalid skill pack source `{0}`")]
    InvalidPackSource(String),
    #[error("skill pack `{0}` is not installed in the library")]
    PackNotInstalled(String),
    #[error("cannot move pin for `{0}` before its update diff has been checked")]
    UpdateDiffNotReviewed(String),
    #[error("candidate commit `{provided}` does not match the reviewed diff commit `{expected}`")]
    CandidateCommitMismatch { expected: String, provided: String },
    #[error("`{command}` failed: {stderr}")]
    GitCommand { command: String, stderr: String },
    #[error("Python 3 was not found on PATH")]
    PythonNotFound,
    #[error("skill pack `{pack}` has no `{file}` at its pinned commit")]
    MissingPackFile { pack: String, file: String },
    #[error(
        "`{tool}` failed:
{output}"
    )]
    ToolFailed { tool: String, output: String },
    #[error(transparent)]
    Git(#[from] velocity_git::GitError),
}
