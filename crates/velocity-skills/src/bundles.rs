//! Bundles of the built-in packs (SPEC.md F18 "Pack specifics"): which skill folders of a
//! pinned clone each bundle chip turns on, the Game dev router that is always on with its
//! pack, and the real per-bundle skill counts of an installed clone.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::catalog::find_built_in_pack;
use crate::library::InstalledSkillPack;
use crate::manifest::DiscoveredSkill;

/// The Game dev skills router: always on whenever the pack is (SPEC.md F18).
pub const GAMEDEV_ROUTER_BUNDLE: &str = "router";
const GAMEDEV_PACK: &str = "gamedev";

/// One bundle and the pack folders whose skills it holds. A skill belongs to the bundle when
/// its folder is one of `folders` or lies under one.
struct BundleFolders {
    pack: &'static str,
    bundle: &'static str,
    folders: &'static [&'static str],
}

// Folders come from each upstream repository's layout. claude-skills also carries mirror
// folders for other agents (`.gemini/`); listing the real folders keeps a mirror from
// counting a skill twice.
const BUNDLE_FOLDERS: &[BundleFolders] = &[
    bundle("uiux", "uiux", &[".claude/skills/ui-ux-pro-max"]),
    bundle("cskills", "eng", &["engineering-team"]),
    bundle("cskills", "engp", &["engineering"]),
    bundle("cskills", "product", &["product-team"]),
    bundle("cskills", "mkt", &["marketing-skill", "marketing"]),
    bundle("cskills", "prod", &["productivity"]),
    bundle("cskills", "research", &["research", "research-ops"]),
    bundle("cskills", "pm", &["project-management"]),
    bundle("cskills", "raqm", &["ra-qm-team"]),
    bundle("cskills", "comp", &["compliance-os"]),
    bundle("cskills", "clevel", &["c-level-advisor", "c-level-agents"]),
    bundle(
        "cskills",
        "biz",
        &[
            "business-growth",
            "business-operations",
            "commercial",
            "finance",
        ],
    ),
    bundle(
        "cskills",
        "launch",
        &["agent-launcher", "loop-library", "markdown-html"],
    ),
    bundle("gamedev", "router", &["router"]),
    bundle("gamedev", "godot", &["skills/godot"]),
    bundle("gamedev", "unity", &["skills/unity"]),
    bundle("gamedev", "unreal", &["skills/unreal"]),
    bundle("gamedev", "ebiten", &["skills/ebitengine"]),
    bundle("gamedev", "web", &["skills/web-engines"]),
    bundle("gamedev", "other", &["skills/other-engines"]),
    bundle("gamedev", "disc", &["skills/disciplines"]),
    bundle("gamedev", "genre", &["skills/genres"]),
    bundle("gamedev", "flow", &["skills/workflows"]),
    // The repository's other `.agents/skills/rea-*` skills are for developing REA itself.
    bundle("rea", "rea", &[".agents/skills/reverse-engineer-anything"]),
];

const fn bundle(
    pack: &'static str,
    bundle: &'static str,
    folders: &'static [&'static str],
) -> BundleFolders {
    BundleFolders {
        pack,
        bundle,
        folders,
    }
}

/// How many skills one bundle of an installed pack holds at its pinned commit.
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillBundleCount {
    #[serde(default)]
    pub bundle_id: String,
    #[serde(default)]
    pub skill_count: u32,
}

/// The real bundle counts of one installed pack, for its card's chips.
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillPackBundleCounts {
    #[serde(default)]
    pub pack_id: String,
    #[serde(default)]
    pub bundles: Vec<SkillBundleCount>,
}

fn folder_holds(folder: &str, skill_folder: &str) -> bool {
    skill_folder == folder
        || skill_folder
            .strip_prefix(folder)
            .is_some_and(|rest| rest.starts_with('/'))
}

fn has_bundle_table(pack_id: &str) -> bool {
    BUNDLE_FOLDERS.iter().any(|entry| entry.pack == pack_id)
}

/// The bundle a skill folder of a built-in pack belongs to, or `None` when no bundle holds it.
pub fn bundle_of_skill(pack_id: &str, relative_folder: &str) -> Option<&'static str> {
    let folder = relative_folder.replace('\\', "/");
    BUNDLE_FOLDERS
        .iter()
        .filter(|entry| entry.pack == pack_id)
        .find(|entry| entry.folders.iter().any(|root| folder_holds(root, &folder)))
        .map(|entry| entry.bundle)
}

/// The bundles that are on when a built-in pack is first switched on: its first bundle. That is
/// Engineering only for claude-skills, to keep the context budget low (SPEC.md F18), the
/// router for Game dev skills, and the single skill of UI UX Pro Max and REA.
pub fn default_bundle_selection(pack_id: &str) -> Vec<String> {
    find_built_in_pack(pack_id)
        .and_then(|entry| entry.bundles.first().map(|first| vec![first.id.clone()]))
        .unwrap_or_default()
}

/// A bundle selection with the rules of SPEC.md F18 applied: unknown bundles are dropped,
/// duplicates removed, and the Game dev router is always on with its pack.
pub fn normalize_bundle_selection(pack_id: &str, selected: &[String]) -> Vec<String> {
    let known: Vec<String> = find_built_in_pack(pack_id)
        .map(|entry| entry.bundles.into_iter().map(|bundle| bundle.id).collect())
        .unwrap_or_default();
    let mut chosen: BTreeSet<&str> = selected
        .iter()
        .map(String::as_str)
        .filter(|bundle| known.iter().any(|id| id == bundle))
        .collect();
    if pack_id == GAMEDEV_PACK {
        chosen.insert(GAMEDEV_ROUTER_BUNDLE);
    }
    // Keep the catalog's order, which is the order of the card's chips.
    known
        .iter()
        .filter(|id| chosen.contains(id.as_str()))
        .cloned()
        .collect()
}

/// The skills of an installed pack that a bundle selection turns on. A pack with no bundle
/// table (a custom Agent Skills repo) turns on every skill it has, once per skill name.
pub fn skills_in_bundles(pack: &InstalledSkillPack, selected: &[String]) -> Vec<DiscoveredSkill> {
    if !has_bundle_table(&pack.id) {
        return distinct_skills(&pack.skills);
    }
    let selection = normalize_bundle_selection(&pack.id, selected);
    pack.skills
        .iter()
        .filter(|skill| !skill.mirror)
        .filter(|skill| {
            bundle_of_skill(&pack.id, &skill.relative_folder)
                .is_some_and(|bundle| selection.iter().any(|id| id == bundle))
        })
        .cloned()
        .collect()
}

// A custom repo may mirror its skills for several agents (`.claude/skills/x`, `.agents/skills/x`).
// The folder outside a dot-folder wins, so each skill is linked once.
fn distinct_skills(skills: &[DiscoveredSkill]) -> Vec<DiscoveredSkill> {
    let mut ordered: Vec<&DiscoveredSkill> = skills.iter().filter(|skill| !skill.mirror).collect();
    ordered.sort_by_key(|skill| skill.relative_folder.starts_with('.'));
    let mut seen = BTreeSet::new();
    let mut distinct: Vec<DiscoveredSkill> = ordered
        .into_iter()
        .filter(|skill| seen.insert(skill.name.to_ascii_lowercase()))
        .cloned()
        .collect();
    distinct.sort_by(|left, right| left.relative_folder.cmp(&right.relative_folder));
    distinct
}

/// Counts the skills each bundle holds in an installed clone, in the catalog's chip order.
pub fn bundle_skill_counts(pack: &InstalledSkillPack) -> SkillPackBundleCounts {
    let bundles = BUNDLE_FOLDERS
        .iter()
        .filter(|entry| entry.pack == pack.id)
        .map(|entry| SkillBundleCount {
            bundle_id: entry.bundle.to_owned(),
            skill_count: pack
                .skills
                .iter()
                .filter(|skill| {
                    !skill.mirror
                        && bundle_of_skill(&pack.id, &skill.relative_folder) == Some(entry.bundle)
                })
                .count()
                .try_into()
                .unwrap_or(u32::MAX),
        })
        .collect();
    SkillPackBundleCounts {
        pack_id: pack.id.clone(),
        bundles,
    }
}
