//! The project's enabled skills as the composer's `$` menu lists them (SPEC.md F4, F18).

use velocity_core::skill_menu::SkillMenuEntry;
use velocity_core::skills::{PackSelection, ProjectSkills};

use crate::SkillLibrarySnapshot;
use crate::settings::skill_enabled;

/// Every skill the project has switched on, from the pinned packs in the library, in library
/// order. The menu inserts `$<name>`, so a name two packs share is listed once.
pub fn skill_menu_entries(
    choices: &ProjectSkills,
    library: &SkillLibrarySnapshot,
) -> Vec<SkillMenuEntry> {
    let mut entries: Vec<SkillMenuEntry> = Vec::new();
    for pack in &library.installed {
        let selection = choices
            .packs
            .get(&pack.id)
            .cloned()
            .unwrap_or_else(|| PackSelection::for_pack(&pack.id));
        for skill in &pack.skills {
            if skill_enabled(&pack.id, &selection, skill)
                && !entries.iter().any(|entry| entry.name == skill.name)
            {
                entries.push(SkillMenuEntry::new(
                    skill.name.clone(),
                    skill.description.clone(),
                ));
            }
        }
    }
    entries
}
