//! Per-project skill choices (F18). Installation and provider linking live at the edges.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSkills {
    #[serde(default)]
    pub packs: BTreeMap<String, PackSelection>,
    /// What the latest project scan suggested (F3, F18 Suggestions); the cards show why.
    #[serde(default)]
    pub suggestions: Vec<SkillSuggestion>,
    /// Suggestions you turned off, as `pack` or `pack/bundle`. A re-check after a merge
    /// never turns these back on ("switched on unless you turn them off").
    #[serde(default)]
    pub declined: Vec<String>,
}

/// One pack the project scan found a reason for, with the bundles that reason implies
/// (the Game dev router plus only the detected engine's bundle).
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillSuggestion {
    #[serde(default)]
    pub pack: String,
    #[serde(default)]
    pub bundles: Vec<String>,
    /// What the scan found, e.g. `UI found: ui/ (Svelte + Tailwind)`.
    #[serde(default)]
    pub reason: String,
}

impl SkillSuggestion {
    pub fn new(pack: impl Into<String>, bundles: Vec<String>, reason: impl Into<String>) -> Self {
        Self {
            pack: pack.into(),
            bundles,
            reason: reason.into(),
        }
    }
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
        self.record_decline(&edit);
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

impl ProjectSkills {
    /// Switches the scan's suggestions on, except ones you turned off, and remembers them
    /// for the cards. Returns whether anything changed, so an unchanged re-check after a
    /// merge writes nothing.
    pub fn apply_suggestions(&mut self, suggestions: Vec<SkillSuggestion>) -> bool {
        let before = self.clone();
        for suggestion in &suggestions {
            if self.declined.contains(&suggestion.pack) {
                continue;
            }
            let selection = self
                .packs
                .entry(suggestion.pack.clone())
                .or_insert_with(|| PackSelection::for_pack(&suggestion.pack));
            selection.enabled = true;
            for bundle in &suggestion.bundles {
                let key = format!("{}/{bundle}", suggestion.pack);
                if !self.declined.contains(&key) && !selection.bundles.contains(bundle) {
                    selection.bundles.push(bundle.clone());
                }
            }
        }
        self.suggestions = suggestions;
        *self != before
    }

    /// The scan's reason for suggesting `pack`, if it suggested it.
    pub fn suggestion_for(&self, pack: &str) -> Option<&SkillSuggestion> {
        self.suggestions
            .iter()
            .find(|suggestion| suggestion.pack == pack)
    }

    /// Turning a suggested pack or bundle off declines it; turning it back on lifts that.
    fn record_decline(&mut self, edit: &SkillSettingsEdit) {
        let (key, enabled, suggested) = match edit {
            SkillSettingsEdit::Pack {
                identifier,
                enabled,
            } => (
                identifier.clone(),
                *enabled,
                self.suggestion_for(identifier).is_some(),
            ),
            SkillSettingsEdit::Bundle {
                identifier,
                bundle,
                enabled,
            } => (
                format!("{identifier}/{bundle}"),
                *enabled,
                self.suggestion_for(identifier)
                    .is_some_and(|suggestion| suggestion.bundles.contains(bundle)),
            ),
        };
        if enabled {
            self.declined.retain(|item| item != &key);
        } else if suggested && !self.declined.contains(&key) {
            self.declined.push(key);
        }
    }
}
