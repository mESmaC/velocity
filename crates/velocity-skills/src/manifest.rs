//! Parsing `SKILL.md` manifests and discovering skills inside a cloned pack (SPEC.md F18).

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::SkillLibraryError;

/// One discovered skill folder (`SKILL.md` plus optional scripts, references and data, SPEC.md §2).
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscoveredSkill {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Relative directory inside the pack clone containing `SKILL.md` (`.` when at the root).
    #[serde(default)]
    pub relative_folder: String,
    #[serde(default)]
    pub has_scripts: bool,
    #[serde(default)]
    pub has_data: bool,
    /// True when the folder is reached through a link the pack tracks in git (a `120000`
    /// entry): a mirror of a skill that lives elsewhere in the pack, such as claude-skills'
    /// `vendor/` and per-agent folders. Mirrors never count twice in a bundle.
    #[serde(default)]
    pub mirror: bool,
}

impl DiscoveredSkill {
    pub fn new(
        name: String,
        description: String,
        relative_folder: String,
        has_scripts: bool,
        has_data: bool,
    ) -> Self {
        Self {
            name,
            description,
            relative_folder,
            has_scripts,
            has_data,
            mirror: false,
        }
    }
}

/// Marks every skill whose folder lies at or under one of `linked_paths` (forward-slash paths
/// relative to the pack) as a mirror.
pub fn mark_mirrored_skills(skills: &mut [DiscoveredSkill], linked_paths: &[String]) {
    for skill in skills {
        skill.mirror = linked_paths.iter().any(|link| {
            skill.relative_folder == *link
                || skill
                    .relative_folder
                    .strip_prefix(link.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        });
    }
}

/// Parses the skill name and one-line description from a `SKILL.md` file's YAML frontmatter
/// or Markdown heading/first paragraph.
pub fn parse_skill_markdown(content: &str, fallback_name: &str) -> (String, String) {
    let normalized = content.replace("\r\n", "\n");
    let mut name = None;
    let mut description = None;
    let body = if let Some(rest) = normalized.strip_prefix("---\n")
        && let Some((frontmatter, after)) = rest.split_once("\n---\n")
    {
        for line in frontmatter.lines() {
            let trimmed = line.trim();
            if let Some(value) = trimmed.strip_prefix("name:") {
                let cleaned = clean_scalar(value);
                if !cleaned.is_empty() {
                    name = Some(cleaned);
                }
            } else if let Some(value) = trimmed.strip_prefix("description:") {
                let cleaned = clean_scalar(value);
                if !cleaned.is_empty() && cleaned != "|" && cleaned != ">" {
                    description = Some(cleaned);
                }
            }
        }
        after
    } else {
        normalized.as_str()
    };

    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Some(heading) = trimmed.strip_prefix('#') {
            if name.is_none() {
                let cleaned = heading.trim_start_matches('#').trim();
                if !cleaned.is_empty() {
                    name = Some(cleaned.to_owned());
                }
            }
            continue;
        }
        if description.is_none() && !trimmed.starts_with("```") {
            description = Some(trimmed.to_owned());
            break;
        }
    }

    let resolved_name = name
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| fallback_name.to_owned());
    let resolved_description = description
        .map(|text| first_sentence_or_line(&text))
        .unwrap_or_default();
    (resolved_name, resolved_description)
}

fn clean_scalar(raw: &str) -> String {
    raw.trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim()
        .to_owned()
}

fn first_sentence_or_line(raw: &str) -> String {
    let single_line = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if single_line.chars().count() <= 140 {
        return single_line;
    }
    single_line.chars().take(137).collect::<String>() + "..."
}

/// Walks a cloned skill pack directory and finds every `SKILL.md` (excluding `.git`).
pub fn discover_pack_skills(
    clone_root: &Path,
    default_name: &str,
) -> Result<Vec<DiscoveredSkill>, SkillLibraryError> {
    let mut skills = Vec::new();
    visit_directory(clone_root, clone_root, default_name, &mut skills)?;
    skills.sort_by(|left, right| {
        left.relative_folder
            .cmp(&right.relative_folder)
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(skills)
}

fn read_skill_in_directory(
    clone_root: &Path,
    current: &Path,
    default_name: &str,
) -> Result<Option<DiscoveredSkill>, SkillLibraryError> {
    let skill_file = current.join("SKILL.md");
    if !skill_file.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(&skill_file).map_err(|source| SkillLibraryError::Io {
        path: skill_file.clone(),
        source,
    })?;
    let content = String::from_utf8_lossy(&bytes);
    let relative = current
        .strip_prefix(clone_root)
        .ok()
        .and_then(|rel| {
            let text = rel.to_string_lossy().replace('\\', "/");
            (!text.is_empty()).then_some(text)
        })
        .unwrap_or_else(|| String::from("."));
    let folder_fallback = current
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty() && relative != ".")
        .unwrap_or_else(|| default_name.to_owned());
    let (name, description) = parse_skill_markdown(&content, &folder_fallback);
    let has_scripts = current.join("scripts").is_dir();
    let has_data = current.join("data").is_dir() || current.join("references").is_dir();
    Ok(Some(DiscoveredSkill::new(
        name,
        description,
        relative,
        has_scripts,
        has_data,
    )))
}

fn visit_directory(
    clone_root: &Path,
    current: &Path,
    default_name: &str,
    skills: &mut Vec<DiscoveredSkill>,
) -> Result<(), SkillLibraryError> {
    if let Some(skill) = read_skill_in_directory(clone_root, current, default_name)? {
        skills.push(skill);
    }

    let entries = fs::read_dir(current).map_err(|source| SkillLibraryError::Io {
        path: current.to_path_buf(),
        source,
    })?;
    let mut subdirs = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| SkillLibraryError::Io {
            path: current.to_path_buf(),
            source,
        })?;
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if name == ".git" || name.starts_with(".velocity") {
            continue;
        }
        let path = entry.path();
        if path.is_dir() {
            subdirs.push(path);
        }
    }
    subdirs.sort();
    for subdir in subdirs {
        visit_directory(clone_root, &subdir, default_name, skills)?;
    }
    Ok(())
}

/// Categories of files inside a skill pack whose diffs are surfaced before moving a pin (F18:
/// "shows the diff of `SKILL.md` files, scripts and data before moving the pin").
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillDiffCategory {
    SkillMarkdown,
    Script,
    Data,
    Other,
}

/// Classifies a repository-relative path for the update diff view.
pub fn classify_pack_path(relative_path: &str) -> SkillDiffCategory {
    let normalized = relative_path.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    if lower == "skill.md" || lower.ends_with("/skill.md") {
        return SkillDiffCategory::SkillMarkdown;
    }
    if lower.starts_with("scripts/")
        || lower.contains("/scripts/")
        || lower.ends_with(".py")
        || lower.ends_with(".sh")
        || lower.ends_with(".ps1")
        || lower.ends_with(".js")
        || lower.ends_with(".mjs")
        || lower.ends_with(".ts")
    {
        return SkillDiffCategory::Script;
    }
    if lower.starts_with("data/")
        || lower.contains("/data/")
        || lower.starts_with("references/")
        || lower.contains("/references/")
        || lower.ends_with(".csv")
        || lower.ends_with(".json")
        || lower.ends_with(".tsv")
        || lower.ends_with(".yaml")
        || lower.ends_with(".yml")
        || lower.ends_with(".md")
    {
        return SkillDiffCategory::Data;
    }
    SkillDiffCategory::Other
}
