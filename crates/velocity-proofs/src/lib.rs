//! Velocity's proof checks (SPEC.md F17 "What Velocity checks"), run in a PR's worktree before
//! PR duty reviews it and again on every push: `lake build`, no `sorry` or `admit`, the axiom
//! audit, a named SPEC.md section in every design proof, and the statement lock that withdraws
//! an approval when a theorem statement changes after it.

mod axioms;
mod check;
mod lean_source;
mod levels;
mod lock_store;
mod report;
mod statement_lock;

pub use axioms::{
    ALLOWED_AXIOMS, AxiomAudit, DisallowedAxiom, audit_script, module_name, parse_audit,
};
pub use check::{check_worktree, levels_between, statements_at};
pub use lean_source::{
    Placeholder, ProofFile, TheoremStatement, names_spec_section, placeholders, theorem_statements,
};
pub use levels::{
    FeatureEvidence, FeatureLevels, LanguageLevel, ProofLevel, feature_evidence, feature_levels,
    fixture_name, language_of, strongest_available, touched_features,
};
pub use lock_store::{load_statement_lock, save_statement_lock};
pub use report::{CheckStep, ProofCheckReport};
pub use statement_lock::StatementLock;

#[derive(Debug, thiserror::Error)]
pub enum ProofCheckError {
    #[error("{0}")]
    Git(String),
    #[error("Could not read or write a proof file: {0}")]
    Io(#[from] std::io::Error),
    #[error("A statement lock is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
}
