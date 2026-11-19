//! Velocity's proof checks (F17): `lake build`, the placeholder scan, the axiom audit and
//! the SPEC.md section check for design proofs. The proof card uses the same checks.

mod axioms;
mod card;
mod check;
mod lean_source;
mod levels;
mod lock_store;
mod report;
mod settings_page;
mod statement_lock;

pub use axioms::{
    ALLOWED_AXIOMS, AxiomAudit, DisallowedAxiom, audit_script, module_name, parse_audit,
};
pub use card::{CardInput, FixtureTable, LockState, build_card};
pub use check::{
    base_reference, changes_proofs, check_build_tree, check_worktree, checkout_at, levels_between,
    proof_card, statements_at,
};
pub use lean_source::{
    Placeholder, ProofFile, TheoremStatement, names_spec_section, placeholders, theorem_statements,
};
pub use levels::{
    FeatureEvidence, FeatureLevels, LanguageLevel, ProofLevel, feature_evidence, feature_levels,
    fixture_name, language_of, strongest_available, touched_features,
};
pub use lock_store::load_statement_lock;
pub use report::{CheckStep, ProofCheckReport};
pub use settings_page::proofs_settings;
pub use statement_lock::StatementLock;

#[derive(Debug, thiserror::Error)]
pub enum ProofCheckError {
    #[error("{0}")]
    Git(String),
    #[error("Could not read or write a proof file: {0}")]
    Io(#[from] std::io::Error),
    #[error("A statement lock is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("The change removes the Lake project's manifest, {0}: its proofs can't be built")]
    RemovedProject(String),
}
