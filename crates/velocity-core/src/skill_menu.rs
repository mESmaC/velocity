//! The composer's `$` skill menu and skill loads in the transcript (SPEC.md F4, F18).
//!
//! Typing `$` lists the project's enabled skills, filtered by what follows the `$`. Picking one
//! replaces the `$word` being typed with `$<skill> `. When a message names an enabled skill, the
//! agent is told to load it for that message. A step where an agent loads a skill shows as
//! "Skill · <name>" in the transcript.

use crate::thread_view_model::SlashCommandItem;

/// One enabled skill as the `$` menu lists it.
#[cfg_attr(
    feature = "bindings",
    derive(serde::Serialize, serde::Deserialize, specta::Type)
)]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillMenuEntry {
    pub name: String,
    pub description: String,
}

impl SkillMenuEntry {
    pub fn new(name: impl Into<String>, description: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
        }
    }
}

/// Characters a skill name may hold after the `$`, as the reference's `\$[\w-]*` matches.
fn is_skill_name_character(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '-'
}

/// The `$word` at the end of the composer text, when the cursor is still typing it: it starts
/// the text or follows whitespace, and holds only skill-name characters after the `$`.
pub fn typed_skill_token(composer_text: &str) -> Option<&str> {
    let start = composer_text.rfind('$')?;
    let (before, token) = composer_text.split_at(start);
    let starts_word = before.chars().next_back().is_none_or(char::is_whitespace);
    let name_only = token[1..].chars().all(is_skill_name_character);
    (starts_word && name_only).then_some(token)
}

/// The enabled skills whose `$name` starts with the `$word` being typed, in the order given.
/// Matching ignores ASCII case, so `$UI` still finds `$ui-ux-pro-max`.
pub fn skill_menu_items(composer_text: &str, enabled: &[SkillMenuEntry]) -> Vec<SlashCommandItem> {
    let Some(token) = typed_skill_token(composer_text) else {
        return Vec::new();
    };
    let typed = token[1..].to_ascii_lowercase();
    enabled
        .iter()
        .filter(|skill| skill.name.to_ascii_lowercase().starts_with(&typed))
        .map(|skill| SlashCommandItem {
            command: format!("${}", skill.name),
            description: skill.description.clone(),
        })
        .collect()
}

/// The composer text after picking `command` (`$<skill>`): the `$word` being typed becomes
/// the picked skill and a space, and the text before it stays as written.
pub fn complete_skill(composer_text: &str, command: &str) -> String {
    let kept = typed_skill_token(composer_text).map_or(composer_text, |token| {
        &composer_text[..composer_text.len() - token.len()]
    });
    format!("{kept}{command} ")
}

/// Enabled skills the message names as `$<skill>`, each once, in the order they appear.
/// A `$word` that names no enabled skill (`$HOME`, `$5`) is left alone.
pub fn requested_skills<'a>(message: &str, enabled: &'a [SkillMenuEntry]) -> Vec<&'a str> {
    let mut requested: Vec<&str> = Vec::new();
    let mut previous: Option<char> = None;
    for (index, character) in message.char_indices() {
        let starts_word = previous.is_none_or(char::is_whitespace);
        previous = Some(character);
        if character != '$' || !starts_word {
            continue;
        }
        let rest = &message[index + 1..];
        let length = rest
            .find(|next: char| !is_skill_name_character(next))
            .unwrap_or(rest.len());
        let named = &rest[..length];
        if let Some(skill) = enabled.iter().find(|skill| skill.name == named)
            && !requested.contains(&skill.name.as_str())
        {
            requested.push(skill.name.as_str());
        }
    }
    requested
}

/// The message the agent receives: the user's words, then one line telling it which skills to
/// load for this message. A message naming no enabled skill is sent unchanged.
pub fn with_skill_instructions(message: &str, enabled: &[SkillMenuEntry]) -> String {
    let requested = requested_skills(message, enabled);
    let Some((last, first)) = requested.split_last() else {
        return message.to_owned();
    };
    let quoted = |name: &&str| format!("`{name}`");
    let names = if first.is_empty() {
        quoted(last)
    } else {
        let listed: Vec<String> = first.iter().map(quoted).collect();
        format!("{} and {}", listed.join(", "), quoted(last))
    };
    let noun = if first.is_empty() { "skill" } else { "skills" };
    format!("{message}\n\nLoad the {names} {noun} before you start on this message.")
}

/// The skill a tool step loads, if it loads one:
/// - Claude Code's `Skill` tool, which `claude-agent-acp` titles `Load skill: <name>` and whose
///   raw input carries `skill`;
/// - any agent reading a skill's `SKILL.md` (Codex, Kimi Code and Antigravity load skills this
///   way), named by the folder that holds it.
pub fn loaded_skill(title: &str, raw_skill: Option<&str>, path: Option<&str>) -> Option<String> {
    let from_title = title
        .strip_prefix("Load skill:")
        .map(str::trim)
        .filter(|name| !name.is_empty());
    let from_input = raw_skill.map(str::trim).filter(|name| !name.is_empty());
    if let Some(name) = from_title.or(from_input) {
        return Some(name.to_owned());
    }
    path.and_then(skill_folder_of_manifest)
}

/// `<anything>/<skill>/SKILL.md` → `<skill>`, with either path separator.
fn skill_folder_of_manifest(path: &str) -> Option<String> {
    let mut segments = path.rsplit(['/', '\\']);
    let file = segments.next()?;
    if !file.eq_ignore_ascii_case("SKILL.md") {
        return None;
    }
    segments
        .next()
        .filter(|folder| !folder.is_empty() && *folder != "." && *folder != "..")
        .map(str::to_owned)
}
