//! Built-in skill packs and repository normalization (SPEC.md F18).

use serde::{Deserialize, Serialize};

use crate::SkillLibraryError;

/// One bundle inside a skill pack (SPEC.md F18: claude-skills by domain, Game dev skills by
/// engine/discipline/genre/workflow).
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillBundleDefinition {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub skill_count: u32,
}

impl SkillBundleDefinition {
    pub fn new(id: impl Into<String>, label: impl Into<String>, skill_count: u32) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            skill_count,
        }
    }
}

/// Static metadata for a built-in or user-added skill pack (SPEC.md F18).
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillPackCatalogEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub repository: String,
    #[serde(default)]
    pub license: String,
    #[serde(default)]
    pub kind_summary: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub built_in: bool,
    #[serde(default)]
    pub requires_python: bool,
    #[serde(default)]
    pub requires_node: bool,
    #[serde(default)]
    pub bundles: Vec<SkillBundleDefinition>,
}

impl SkillPackCatalogEntry {
    pub fn custom(id: String, name: String, repository: String) -> Self {
        Self {
            id,
            name,
            repository,
            license: String::from("Custom"),
            kind_summary: String::from("Agent Skills repository"),
            description: String::from(
                "Custom Agent Skills repository (a folder per skill with a SKILL.md).",
            ),
            built_in: false,
            requires_python: false,
            requires_node: false,
            bundles: Vec::new(),
        }
    }
}

/// The four built-in skill packs in SPEC.md F18 and `design/main-window.html`.
pub fn built_in_catalog() -> Vec<SkillPackCatalogEntry> {
    vec![
        uiux_catalog_entry(),
        claude_skills_catalog_entry(),
        gamedev_catalog_entry(),
        rea_catalog_entry(),
    ]
}

fn uiux_catalog_entry() -> SkillPackCatalogEntry {
    SkillPackCatalogEntry {
        id: String::from("uiux"),
        name: String::from("UI UX Pro Max"),
        repository: String::from("nextlevelbuilder/ui-ux-pro-max-skill"),
        license: String::from("MIT"),
        kind_summary: String::from("1 skill · design intelligence"),
        description: String::from(
            "80 styles, 192 palettes, 74 font pairings, 25 chart types, 120 UX rules and guidelines for 22 stacks, behind a local BM25 search. Generates a design system per project.",
        ),
        built_in: true,
        requires_python: true,
        requires_node: false,
        bundles: vec![SkillBundleDefinition::new("uiux", "ui-ux-pro-max", 1)],
    }
}

fn claude_skills_catalog_entry() -> SkillPackCatalogEntry {
    SkillPackCatalogEntry {
        id: String::from("cskills"),
        name: String::from("claude-skills"),
        repository: String::from("alirezarezvani/claude-skills"),
        license: String::from("MIT"),
        kind_summary: String::from("410 skills · 807 Python tools"),
        description: String::from(
            "Domain skills in bundles, each with a SKILL.md, stdlib-only Python tools and reference docs. Turn on only the bundles a project needs: every enabled skill puts its description in every agent's context.",
        ),
        built_in: true,
        requires_python: true,
        requires_node: false,
        bundles: vec![
            SkillBundleDefinition::new("eng", "Engineering", 54),
            SkillBundleDefinition::new("engp", "Engineering · advanced", 104),
            SkillBundleDefinition::new("product", "Product", 19),
            SkillBundleDefinition::new("mkt", "Marketing", 57),
            SkillBundleDefinition::new("prod", "Productivity", 12),
            SkillBundleDefinition::new("research", "Research", 16),
            SkillBundleDefinition::new("pm", "Project management", 9),
            SkillBundleDefinition::new("raqm", "Regulatory & QM", 23),
            SkillBundleDefinition::new("comp", "Compliance", 9),
            SkillBundleDefinition::new("clevel", "C-level", 68),
            SkillBundleDefinition::new("biz", "Business · finance", 28),
            SkillBundleDefinition::new("launch", "Agent launcher · loops", 12),
        ],
    }
}

fn gamedev_catalog_entry() -> SkillPackCatalogEntry {
    SkillPackCatalogEntry {
        id: String::from("gamedev"),
        name: String::from("Game dev skills"),
        repository: String::from("gamedev-skills/awesome-gamedev-agent-skills"),
        license: String::from("Apache-2.0"),
        kind_summary: String::from("77 skills · router"),
        description: String::from(
            "Engine, discipline, genre and workflow playbooks. Its router detects the engine from project files and loads only the matching skills.",
        ),
        built_in: true,
        requires_python: false,
        requires_node: false,
        bundles: vec![
            SkillBundleDefinition::new("router", "Router", 1),
            SkillBundleDefinition::new("godot", "Godot 4.7", 16),
            SkillBundleDefinition::new("unity", "Unity 6.3", 8),
            SkillBundleDefinition::new("unreal", "Unreal 5.8", 6),
            SkillBundleDefinition::new("ebiten", "Ebitengine", 1),
            SkillBundleDefinition::new("web", "Phaser · PixiJS · three.js", 6),
            SkillBundleDefinition::new("other", "Bevy · pygame · LÖVE · Defold · Roblox", 12),
            SkillBundleDefinition::new("disc", "Disciplines", 15),
            SkillBundleDefinition::new("genre", "Genres", 9),
            SkillBundleDefinition::new("flow", "Jam · Steam · itch.io", 4),
        ],
    }
}

fn rea_catalog_entry() -> SkillPackCatalogEntry {
    SkillPackCatalogEntry {
        id: String::from("rea"),
        name: String::from("REA"),
        repository: String::from("morluto/rea"),
        license: String::from("MIT"),
        kind_summary: String::from("MCP server + investigation skill"),
        description: String::from(
            "Reverse engineering for agents: native binaries, Electron/JS apps, .NET, Android APKs, websites, firmware and packet captures. Findings come back with evidence and unknowns. Only for software you own or may examine.",
        ),
        built_in: true,
        requires_python: false,
        requires_node: true,
        bundles: vec![SkillBundleDefinition::new("rea", "rea skill", 1)],
    }
}

/// Finds a built-in pack by its pack identifier (`uiux`, `cskills`, `gamedev`, `rea`) or
/// by its `owner/repo` string.
pub fn find_built_in_pack(identifier_or_repo: &str) -> Option<SkillPackCatalogEntry> {
    let trimmed = identifier_or_repo.trim();
    let normalized = normalize_github_slug(trimmed);
    built_in_catalog().into_iter().find(|entry| {
        entry.id.eq_ignore_ascii_case(trimmed)
            || normalized
                .as_deref()
                .is_some_and(|slug| entry.repository.eq_ignore_ascii_case(slug))
    })
}

/// Normalizes a GitHub repository reference (`owner/repo` or `https://github.com/owner/repo`)
/// when the input is a GitHub slug; returns `None` for local filesystem paths or other URLs.
pub fn normalize_github_slug(input: &str) -> Option<String> {
    let trimmed = input.trim().trim_end_matches('/').trim_end_matches(".git");
    let stripped = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
        .or_else(|| trimmed.strip_prefix("github.com/"))
        .unwrap_or(trimmed);
    let mut parts = stripped.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    if parts.next().is_some() || !is_slug_segment(owner) || !is_slug_segment(repo) {
        return None;
    }
    Some(format!("{owner}/{repo}"))
}

fn is_slug_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

/// Resolves a pack identifier, `owner/repo`, or clone source URL/path into a catalog entry
/// and the concrete git remote URL/path to clone from.
pub fn resolve_pack_source(
    pack_or_repo: &str,
    source_override: Option<&str>,
) -> Result<(SkillPackCatalogEntry, String), SkillLibraryError> {
    let trimmed = pack_or_repo.trim();
    if trimmed.is_empty() {
        return Err(SkillLibraryError::InvalidPackSource(
            "skill pack identifier or repository cannot be empty".into(),
        ));
    }
    if let Some(entry) = find_built_in_pack(trimmed) {
        let remote = match source_override.map(str::trim).filter(|s| !s.is_empty()) {
            Some(custom) => custom.to_owned(),
            None => format!("https://github.com/{}.git", entry.repository),
        };
        return Ok((entry, remote));
    }
    if let Some(slug) = normalize_github_slug(trimmed) {
        let id = pack_id_from_slug(&slug);
        // `_/_` is a valid slug with no alphanumerics: an empty id would make the pack's
        // folder the library's packs folder, which install clears first.
        if id.is_empty() {
            return Err(SkillLibraryError::InvalidPackSource(trimmed.to_owned()));
        }
        let name = slug.split('/').nth(1).unwrap_or(&slug).to_owned();
        let entry = SkillPackCatalogEntry::custom(id, name, slug.clone());
        let remote = match source_override.map(str::trim).filter(|s| !s.is_empty()) {
            Some(custom) => custom.to_owned(),
            None => format!("https://github.com/{slug}.git"),
        };
        return Ok((entry, remote));
    }
    if let Some(custom_source) = source_override.map(str::trim).filter(|s| !s.is_empty()) {
        let id = sanitize_pack_id(trimmed);
        if id.is_empty() {
            return Err(SkillLibraryError::InvalidPackSource(trimmed.to_owned()));
        }
        let entry = SkillPackCatalogEntry::custom(id, trimmed.to_owned(), trimmed.to_owned());
        return Ok((entry, custom_source.to_owned()));
    }
    Err(SkillLibraryError::InvalidPackSource(trimmed.to_owned()))
}

pub fn pack_id_from_slug(slug: &str) -> String {
    sanitize_pack_id(&slug.replace('/', "-"))
}

pub fn sanitize_pack_id(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_owned()
}
