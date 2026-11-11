//! What a built-in pack adds to its card and to every agent once it is switched on for a
//! project (SPEC.md F18 "Pack specifics"): UI UX Pro Max's design system row, Game dev
//! skills' engine line, and REA's shared MCP server.

use std::path::Path;

use serde::{Deserialize, Serialize};
use velocity_core::skills::ProjectSkills;

use crate::design_system::{DesignSystemStatus, UIUX_PACK, design_system_status};
use crate::engine::{GameEngineDetection, detect_game_engine};
use crate::library::{SkillLibrary, SkillLibrarySnapshot};
use crate::rea::{REA_PACK, SkillMcpServer, rea_mcp_server};
use crate::settings::SkillsSettingsView;

const GAMEDEV_PACK: &str = "gamedev";

/// The pack-specific rows of one card. Every field is empty for packs without one.
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillPackDetails {
    /// UI UX Pro Max, while on: the design system and its Generate / Regenerate row.
    #[serde(default)]
    pub design_system: Option<DesignSystemStatus>,
    /// Game dev skills: the engine the project's files show.
    #[serde(default)]
    pub engine: Option<GameEngineDetection>,
    /// REA, while on and installed: the MCP server every new agent gets.
    #[serde(default)]
    pub shared_server: Option<SkillMcpServer>,
}

/// Fills each card's pack-specific rows from the project's checkout.
pub fn add_pack_details(
    view: &mut SkillsSettingsView,
    checkout: &Path,
    library: &SkillLibrarySnapshot,
) {
    for card in &mut view.cards {
        let on = card.selection.enabled;
        card.details = match card.catalog.id.as_str() {
            UIUX_PACK if on => SkillPackDetails {
                design_system: Some(design_system_status(checkout, &view.project_name)),
                ..SkillPackDetails::default()
            },
            GAMEDEV_PACK => SkillPackDetails {
                engine: Some(detect_game_engine(checkout)),
                ..SkillPackDetails::default()
            },
            REA_PACK if on => SkillPackDetails {
                shared_server: library
                    .installed
                    .iter()
                    .find(|pack| pack.id == REA_PACK)
                    .and_then(|pack| rea_mcp_server(pack).ok()),
                ..SkillPackDetails::default()
            },
            _ => SkillPackDetails::default(),
        };
    }
}

/// The MCP servers a project's switched-on packs share with every agent started in it
/// (§3.2 Tool access). A pack that is off or not installed adds none.
pub fn pack_shared_servers(choices: &ProjectSkills, library: &SkillLibrary) -> Vec<SkillMcpServer> {
    let rea_on = choices
        .packs
        .get(REA_PACK)
        .is_some_and(|selection| selection.enabled);
    if !rea_on {
        return Vec::new();
    }
    library
        .installed_pack(REA_PACK)
        .and_then(|pack| rea_mcp_server(&pack))
        .map(|server| vec![server])
        .unwrap_or_default()
}
