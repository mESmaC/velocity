//! Settings → Proofs (SPEC.md F13): reads one project's checkout for the page. The proof list
//! comes from SPEC.md, the design proofs and their levels from git, the translators from the
//! language table. Nothing is built or run.

use std::path::Path;

use velocity_core::proof_card::ProofLevel;
use velocity_core::proofs_settings::{
    DesignProofEntry, LanguageTranslator, ProofsSettingsView, proof_list_from_spec,
};

use crate::ProofCheckError;
use crate::card::level_kind;
use crate::check::tracked_design_levels;
use crate::levels::{FeatureLevels, language_translators, strongest_available};

/// The Proofs page for the project checked out at `repository`.
pub fn proofs_settings(
    repository: &Path,
    stress_diff_input_count: u32,
) -> Result<ProofsSettingsView, ProofCheckError> {
    let spec = match std::fs::read_to_string(repository.join("SPEC.md")) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let proof_list = spec.as_deref().and_then(proof_list_from_spec);
    let (lake_project, design_proofs) = match tracked_design_levels(repository)? {
        Some((folder, features)) => {
            let entries = features
                .iter()
                .map(|feature| design_entry(&folder, feature))
                .collect();
            (Some(folder), entries)
        }
        None => (None, Vec::new()),
    };
    Ok(ProofsSettingsView::new(
        spec.is_some(),
        proof_list,
        lake_project,
        design_proofs,
        translators(),
        stress_diff_input_count,
    ))
}

fn design_entry(lake_project: &str, feature: &FeatureLevels) -> DesignProofEntry {
    let folder = format!("{lake_project}/{}", feature.feature);
    let levels = feature
        .languages
        .iter()
        .map(|language| {
            ProofLevel::new(
                language.language.unwrap_or("No code"),
                level_kind(language.level),
                &language.evidence.replace('`', ""),
                language.flagged(),
            )
        })
        .collect();
    DesignProofEntry::new(&feature.feature, &folder, levels)
}

fn translators() -> Vec<LanguageTranslator> {
    language_translators()
        .map(|(language, translator)| {
            LanguageTranslator::new(
                language,
                translator,
                level_kind(strongest_available(language)),
            )
        })
        .collect()
}
