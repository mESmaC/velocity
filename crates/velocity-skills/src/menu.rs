//! Installed skill names shared by the composer and the session prompt resolver (F18).

use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::path::PathBuf;
use velocity_core::skill_menu::SkillMenuEntry;

use crate::{DiscoveredSkill, SkillLibrarySnapshot};

/// A colliding name can still be invoked by its pack-qualified name.
pub fn installed_skills(
    library: &SkillLibrarySnapshot,
) -> BTreeMap<String, (PathBuf, &DiscoveredSkill)> {
    let mut installed = BTreeMap::new();
    let mut duplicates = Vec::new();
    for pack in &library.installed {
        for skill in pack.skills.iter().filter(|skill| !skill.mirror) {
            let source = PathBuf::from(&pack.clone_path).join(&skill.relative_folder);
            let name = invocation_name(&skill.name);
            match installed.entry(name.clone()) {
                Entry::Vacant(entry) => {
                    entry.insert((source, skill));
                }
                Entry::Occupied(_) => {
                    duplicates.push((format!("{}--{name}", pack.id), source, skill));
                }
            }
        }
    }
    for (name, source, skill) in duplicates {
        let mut alias = name.clone();
        let mut suffix = 2;
        while installed.contains_key(&alias) {
            alias = format!("{name}-{suffix}");
            suffix += 1;
        }
        installed.insert(alias, (source, skill));
    }
    installed
}

fn invocation_name(name: &str) -> String {
    if crate::workspace::valid_name(name) && !name.contains('.') {
        return name.to_owned();
    }
    // Some packs supply a Markdown heading as the name. The composer needs a single word.
    let slug = crate::sanitize_pack_id(name);
    if slug.is_empty() {
        "skill".into()
    } else {
        slug
    }
}

pub fn skill_menu_entries(library: &SkillLibrarySnapshot) -> Vec<SkillMenuEntry> {
    installed_skills(library)
        .into_iter()
        .map(|(name, (_, skill))| SkillMenuEntry::new(name, skill.description.clone()))
        .collect()
}
