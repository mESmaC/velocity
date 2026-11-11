//! Per-project skill choices (F18). Installation and provider linking live at the edges.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSkills {
    #[serde(default)]
    pub packs: BTreeMap<String, PackSelection>,
}

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PackSelection {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub bundles: Vec<String>,
}
impl PackSelection {
    pub fn for_pack(identifier: &str) -> Self {
        let bundles = match identifier {
            "cskills" => vec!["eng".into()],
            "gamedev" => vec!["router".into()],
            _ => Vec::new(),
        };
        Self {
            enabled: false,
            bundles,
        }
    }
}

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum SkillSettingsEdit {
    Pack {
        identifier: String,
        enabled: bool,
    },
    Bundle {
        identifier: String,
        bundle: String,
        enabled: bool,
    },
}
impl ProjectSkills {
    pub fn apply(&mut self, edit: SkillSettingsEdit) {
        let identifier = match &edit {
            SkillSettingsEdit::Pack { identifier, .. }
            | SkillSettingsEdit::Bundle { identifier, .. } => identifier.clone(),
        };
        let selection = self
            .packs
            .entry(identifier.clone())
            .or_insert_with(|| PackSelection::for_pack(&identifier));
        match edit {
            SkillSettingsEdit::Pack { enabled, .. } => selection.enabled = enabled,
            SkillSettingsEdit::Bundle {
                bundle, enabled, ..
            } => {
                if enabled && !selection.bundles.contains(&bundle) {
                    selection.bundles.push(bundle);
                } else if !enabled {
                    selection.bundles.retain(|item| item != &bundle);
                }
                // The router remains available whenever the game pack is enabled.
                if identifier == "gamedev" && !selection.bundles.iter().any(|item| item == "router")
                {
                    selection.bundles.push("router".into());
                }
            }
        }
    }
}
