//! Settings projection from pinned manifests and saved project choices (F18).
use crate::{DiscoveredSkill, SkillLibrarySnapshot, SkillPackCatalogEntry};
use serde::{Deserialize, Serialize};
use velocity_core::skills::{PackSelection, ProjectSkills};

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillPackCard {
    pub catalog: SkillPackCatalogEntry,
    pub installed: bool,
    pub pin: String,
    pub selection: PackSelection,
    pub enabled_count: u32,
    pub requirements: Vec<String>,
    #[serde(default)]
    pub details: crate::details::SkillPackDetails,
    /// `suggested · UI found` when the project scan suggested this pack (F3, F18), else empty.
    pub suggestion_badge: String,
    /// What the scan found, or what would make it suggest the pack; empty for other packs.
    pub detection: String,
}
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SkillsSettingsView {
    pub project_identifier: u32,
    pub project_name: String,
    pub cards: Vec<SkillPackCard>,
    pub enabled_count: u32,
    pub description_tokens: u32,
    pub budget_warning: bool,
}

pub fn settings_view(
    identifier: u32,
    name: String,
    choices: &ProjectSkills,
    library: &SkillLibrarySnapshot,
) -> SkillsSettingsView {
    let mut count = 0;
    let mut tokens = 0;
    let cards = library
        .catalog
        .iter()
        .map(|catalog| {
            let installed = library.installed.iter().find(|pack| pack.id == catalog.id);
            let selection = choices
                .packs
                .get(&catalog.id)
                .cloned()
                .unwrap_or_else(|| PackSelection::for_pack(&catalog.id));
            let skills: Vec<_> = installed
                .into_iter()
                .flat_map(|pack| &pack.skills)
                .filter(|skill| skill_enabled(&catalog.id, &selection, skill))
                .collect();
            let enabled_count = u32::try_from(skills.len()).unwrap_or(u32::MAX);
            count += enabled_count;
            // Estimate the name and description loaded at startup, without loading skill bodies.
            tokens += description_tokens(&skills);
            SkillPackCard {
                catalog: with_installed_counts(catalog, library),
                installed: installed.is_some(),
                pin: installed
                    .map(|pack| pack.short_commit.clone())
                    .unwrap_or_default(),
                selection,
                enabled_count,
                requirements: crate::requirements::requirements(catalog),
                details: crate::details::SkillPackDetails::default(),
                suggestion_badge: suggestion_badge(&catalog.id, choices),
                detection: detection(&catalog.id, choices),
            }
        })
        .collect();
    SkillsSettingsView {
        project_identifier: identifier,
        project_name: name,
        cards,
        enabled_count: count,
        description_tokens: tokens,
        budget_warning: count > 150,
    }
}
// The chips show how many skills each bundle holds at the pinned commit, once installed.
fn with_installed_counts(
    catalog: &SkillPackCatalogEntry,
    library: &SkillLibrarySnapshot,
) -> SkillPackCatalogEntry {
    let mut entry = catalog.clone();
    let Some(counts) = library
        .bundle_counts
        .iter()
        .find(|counts| counts.pack_id == catalog.id)
    else {
        return entry;
    };
    for bundle in &mut entry.bundles {
        if let Some(count) = counts
            .bundles
            .iter()
            .find(|count| count.bundle_id == bundle.id)
        {
            bundle.skill_count = count.skill_count;
        }
    }
    entry
}
fn suggestion_badge(pack: &str, choices: &ProjectSkills) -> String {
    choices
        .suggestion_for(pack)
        .map(|suggestion| {
            let finding = suggestion.reason.split(':').next().unwrap_or_default();
            format!("suggested \u{00b7} {finding}")
        })
        .unwrap_or_default()
}
fn detection(pack: &str, choices: &ProjectSkills) -> String {
    if let Some(suggestion) = choices.suggestion_for(pack) {
        return suggestion.reason.clone();
    }
    match pack {
        "uiux" => crate::suggestions::UI_HINT.into(),
        "gamedev" => crate::suggestions::ENGINE_HINT.into(),
        _ => String::new(),
    }
}
fn description_tokens(skills: &[&DiscoveredSkill]) -> u32 {
    skills
        .iter()
        .map(|skill| {
            u32::try_from(
                (skill.name.chars().count() + skill.description.chars().count()).div_ceil(4),
            )
            .unwrap_or(u32::MAX)
        })
        .sum()
}

/// Bundle membership follows each pack's folders at its pinned commit (`crate::bundles`).
pub fn skill_enabled(pack: &str, selection: &PackSelection, skill: &DiscoveredSkill) -> bool {
    selection.enabled && crate::bundles::selection_includes(pack, &selection.bundles, skill)
}
