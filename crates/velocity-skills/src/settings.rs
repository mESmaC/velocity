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
                catalog: catalog.clone(),
                installed: installed.is_some(),
                pin: installed
                    .map(|pack| pack.short_commit.clone())
                    .unwrap_or_default(),
                selection,
                enabled_count,
                requirements: requirements(catalog),
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

/// Bundle membership follows the pack's repository directories; custom packs enable all skills.
pub fn skill_enabled(pack: &str, selection: &PackSelection, skill: &DiscoveredSkill) -> bool {
    if !selection.enabled {
        return false;
    }
    if pack != "cskills" && pack != "gamedev" {
        return true;
    }
    let path = skill.relative_folder.to_ascii_lowercase();
    selection.bundles.iter().any(|bundle| {
        bundle_paths(bundle)
            .iter()
            .any(|prefix| path.split('/').any(|part| part == *prefix))
    })
}
fn bundle_paths(bundle: &str) -> Vec<&str> {
    match bundle {
        "eng" => vec!["engineering"],
        "engp" => vec!["engineering-team"],
        "mkt" => vec!["marketing", "marketing-skill"],
        "product" => vec!["product-team"],
        "prod" => vec!["productivity"],
        "pm" => vec!["project-management"],
        "raqm" => vec!["ra-qm-team"],
        "comp" => vec!["compliance-os"],
        "clevel" => vec!["c-level-advisor", "c-level-agents"],
        "biz" => vec!["business-growth", "business-operations", "finance"],
        "launch" => vec!["agent-launcher"],
        "router" => vec!["router"],
        "disc" => vec!["disciplines"],
        "genre" => vec!["genres"],
        "flow" => vec!["workflows"],
        "ebiten" => vec!["ebitengine"],
        "web" => vec!["web-engines"],
        "other" => vec!["other-engines"],
        _ => vec![bundle],
    }
}
fn requirements(catalog: &SkillPackCatalogEntry) -> Vec<String> {
    let mut result = Vec::new();
    if catalog.requires_python {
        result.push(probe("python", "Python 3", 3));
    }
    if catalog.requires_node {
        result.push(probe("node", "Node 22+", 22));
        for program in ["ghidra", "hopper", "ida", "rizin", "objdump"] {
            result.push(format!(
                "{program} optional \u{00b7} {}",
                if std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths)
                    .any(|folder| folder.join(program).is_file()
                        || folder.join(format!("{program}.exe")).is_file()))
                {
                    "found"
                } else {
                    "not found"
                }
            ));
        }
    }
    if result.is_empty() {
        result.push("No required runtime".into());
    }
    result
}
fn probe(program: &str, label: &str, minimum: u32) -> String {
    let version = command_version(program);
    let found = version
        .as_deref()
        .and_then(|text| {
            text.trim_start_matches(|character: char| !character.is_ascii_digit())
                .split('.')
                .next()?
                .parse::<u32>()
                .ok()
        })
        .is_some_and(|major| major >= minimum);
    format!(
        "{} {label} \u{00b7} {}",
        if found { "\u{2713}" } else { "\u{00b7}" },
        version.as_deref().unwrap_or("not found")
    )
}
fn command_version(program: &str) -> Option<String> {
    let mut command = std::process::Command::new(program);
    command.arg("--version");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    let output = command.output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
