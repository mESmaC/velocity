//! Velocity's skill pack library (SPEC.md §4 F18, §7 M12): clones and pins skill packs in
//! one machine-wide library, previews updates as a diff of `SKILL.md` files, scripts and data
//! before moving a pin, and configures `core.symlinks=true` and `PYTHONUTF8=1` on Windows.

#![forbid(unsafe_code)]

pub mod catalog;
pub mod library;
pub mod manifest;
pub mod settings;
pub mod suggestions;

use std::path::PathBuf;
use thiserror::Error;

pub use catalog::{
    SkillBundleDefinition, SkillPackCatalogEntry, built_in_catalog, find_built_in_pack,
    normalize_github_slug, pack_id_from_slug, resolve_pack_source, sanitize_pack_id,
};
pub use library::{
    InstalledSkillPack, SkillLibrary, SkillLibrarySnapshot, SkillPackDiffFile,
    SkillPackUpdatePreview, SkillPythonOutput, run_python_utf8,
};
pub use manifest::{
    DiscoveredSkill, SkillDiffCategory, classify_pack_path, discover_pack_skills,
    parse_skill_markdown,
};

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
}
