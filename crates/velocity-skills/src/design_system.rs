//! UI UX Pro Max design system (SPEC.md F18 "Pack specifics"): **Generate** runs the pack's
//! search with `--design-system --persist`, which writes `design-system/<project>/MASTER.md`
//! plus page overrides into the project's main checkout. The design system is committed like
//! SPEC.md, and while it exists AGENTS.md carries the rule that points agents at it (F11).

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::SkillLibraryError;
use crate::library::{SkillLibrary, run_python_utf8};

pub const DESIGN_SYSTEM_FOLDER: &str = "design-system";
pub const UIUX_PACK: &str = "uiux";

// The skill's own copy of its search first; older pins kept the script under `src/`.
const SEARCH_SCRIPTS: &[&str] = &[
    ".claude/skills/ui-ux-pro-max/scripts/search.py",
    "src/ui-ux-pro-max/scripts/search.py",
];

/// Where a project's design system lives and whether it has been generated.
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesignSystemStatus {
    /// `design-system/<project>/MASTER.md`, relative to the checkout.
    #[serde(default)]
    pub master_path: String,
    #[serde(default)]
    pub exists: bool,
    /// When MASTER.md was last written, in Unix seconds.
    #[serde(default)]
    pub generated_at: Option<u32>,
    /// True when AGENTS.md carries the design system rule.
    #[serde(default)]
    pub rule_in_agents: bool,
    /// The card's line: `generated 5 min ago · agents read it before UI work`, or
    /// `not generated yet`.
    #[serde(default)]
    pub summary: String,
}

/// The folder name `search.py` derives from a project name (its `safe_slug`): lower case,
/// every run of other characters turned into one `-`, and `default` when nothing is left.
pub fn design_system_slug(project_name: &str) -> String {
    let mut slug = String::new();
    for character in project_name.to_lowercase().chars() {
        if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
            slug.push(character);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let trimmed = slug.trim_matches('-');
    if trimmed.is_empty() {
        String::from("default")
    } else {
        trimmed.to_owned()
    }
}

pub fn design_system_folder(project_name: &str) -> String {
    format!(
        "{DESIGN_SYSTEM_FOLDER}/{}",
        design_system_slug(project_name)
    )
}

pub fn design_system_master_path(project_name: &str) -> String {
    format!("{}/MASTER.md", design_system_folder(project_name))
}

/// The AGENTS.md rule SPEC.md F18 gives a project with a design system.
pub fn design_system_rule(project_name: &str) -> String {
    format!(
        "Before any UI work, read `{}`. Use only its colours, type and spacing.",
        design_system_master_path(project_name)
    )
}

/// AGENTS.md with the design system rule present when `rule` is `Some` and absent when `None`.
/// Returns `None` when the file already matches. The rule goes at the end of `## UI`, and a
/// file with no `## UI` section gets one at its end. Removal deletes only Velocity's own bullet.
pub fn sync_design_system_rule(agents_markdown: &str, rule: Option<&str>) -> Option<String> {
    let newline = if agents_markdown.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let lines: Vec<&str> = agents_markdown.lines().collect();
    let is_rule = |line: &&str| is_design_system_rule(line);
    let present = lines.iter().any(is_rule);
    match (rule, present) {
        (None, false) => None,
        (Some(rule), true) if lines.iter().any(|line| line.trim() == format!("- {rule}")) => None,
        (None, true) => Some(join(
            lines.into_iter().filter(|line| !is_rule(line)),
            newline,
        )),
        (Some(rule), _) => {
            let kept: Vec<&str> = lines.into_iter().filter(|line| !is_rule(line)).collect();
            Some(insert_rule(&kept, rule, newline))
        }
    }
}

// Velocity's bullet, whatever project path it named: a renamed project moves the rule.
fn is_design_system_rule(line: &str) -> bool {
    let trimmed = line.trim();
    trimmed.starts_with("- Before any UI work, read `design-system/")
        && trimmed.ends_with("Use only its colours, type and spacing.")
}

fn insert_rule(lines: &[&str], rule: &str, newline: &str) -> String {
    let bullet = format!("- {rule}");
    let mut output: Vec<String> = lines.iter().map(|line| (*line).to_owned()).collect();
    let heading = lines.iter().position(|line| line.trim() == "## UI");
    match heading {
        Some(start) => {
            let end = lines[start + 1..]
                .iter()
                .position(|line| line.starts_with("## "))
                .map_or(lines.len(), |offset| start + 1 + offset);
            // Directly after the section's last non-blank line, so it stays in its list.
            let last = (start..end)
                .rev()
                .find(|index| !lines[*index].trim().is_empty())
                .unwrap_or(start);
            output.insert(last + 1, bullet);
        }
        None => {
            while output.last().is_some_and(|line| line.trim().is_empty()) {
                output.pop();
            }
            if !output.is_empty() {
                output.push(String::new());
            }
            output.push(String::from("## UI"));
            output.push(bullet);
        }
    }
    join(output.iter().map(String::as_str), newline)
}

fn join<'a>(lines: impl Iterator<Item = &'a str>, newline: &str) -> String {
    let mut text = lines.collect::<Vec<_>>().join(newline);
    text.push_str(newline);
    text
}

/// The search query for a project: the first sentence of SPEC.md's opening paragraph, cut to
/// twelve words, which names the product and its kind. Falls back to the project's name.
pub fn design_system_query(project_name: &str, spec_markdown: Option<&str>) -> String {
    let paragraph = spec_markdown.and_then(|spec| {
        spec.lines().map(str::trim).find(|line| {
            !line.is_empty()
                && !line.starts_with(['#', '-', '*', '>', '|', '`', '<'])
                && !line
                    .chars()
                    .all(|character| character == '-' || character == '=')
        })
    });
    let Some(paragraph) = paragraph else {
        return project_name.trim().to_owned();
    };
    let sentence = paragraph.split(". ").next().unwrap_or(paragraph);
    let words: Vec<String> = sentence
        .split_whitespace()
        .map(|word| word.trim_matches(['*', '_', '`', '"', ':', ',', '.', '(', ')']))
        .filter(|word| !word.is_empty())
        .take(12)
        .map(str::to_owned)
        .collect();
    if words.is_empty() {
        project_name.trim().to_owned()
    } else {
        words.join(" ")
    }
}

/// Reads the design system's status from the checkout.
pub fn design_system_status(checkout: &Path, project_name: &str) -> DesignSystemStatus {
    let master_path = design_system_master_path(project_name);
    let master = checkout.join(&master_path);
    let generated_at = fs::metadata(&master)
        .ok()
        .filter(fs::Metadata::is_file)
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|elapsed| u32::try_from(elapsed.as_secs()).unwrap_or(u32::MAX));
    let rule = format!("- {}", design_system_rule(project_name));
    let rule_in_agents = fs::read_to_string(checkout.join("AGENTS.md"))
        .is_ok_and(|text| text.lines().any(|line| line.trim() == rule));
    let summary = match generated_at {
        Some(at) => format!(
            "generated {} · agents read it before UI work",
            age(at, now())
        ),
        None => String::from("not generated yet"),
    };
    DesignSystemStatus {
        master_path,
        exists: generated_at.is_some(),
        generated_at,
        rule_in_agents,
        summary,
    }
}

fn now() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u32::try_from(elapsed.as_secs()).unwrap_or(u32::MAX)
        })
}

/// How long ago `at` was, in the reference's words: `just now`, `12 min ago`, `3 h ago`, `2 d ago`.
pub fn age(at: u32, now: u32) -> String {
    let seconds = now.saturating_sub(at);
    match seconds {
        0..60 => String::from("just now"),
        60..3_600 => format!("{} min ago", seconds / 60),
        3_600..86_400 => format!("{} h ago", seconds / 3_600),
        _ => format!("{} d ago", seconds / 86_400),
    }
}

/// Runs UI UX Pro Max's search with `--design-system --persist` in the checkout. `regenerate`
/// adds `--force`: without it, the search leaves an existing MASTER.md untouched.
pub fn run_design_system_search(
    library: &SkillLibrary,
    checkout: &Path,
    project_name: &str,
    query: &str,
    regenerate: bool,
) -> Result<(), SkillLibraryError> {
    let pack = library.installed_pack(UIUX_PACK)?;
    let clone = PathBuf::from(&pack.clone_path);
    let script = SEARCH_SCRIPTS
        .iter()
        .map(|relative| clone.join(relative))
        .find(|path| path.is_file())
        .ok_or_else(|| SkillLibraryError::MissingPackFile {
            pack: UIUX_PACK.to_owned(),
            file: SEARCH_SCRIPTS[0].to_owned(),
        })?;
    let mut arguments = vec![
        query.to_owned(),
        String::from("--design-system"),
        String::from("--persist"),
        String::from("-p"),
        project_name.to_owned(),
        String::from("--output-dir"),
        plain_path(checkout),
    ];
    if regenerate {
        arguments.push(String::from("--force"));
    }
    let output = run_python_utf8(checkout, &script, &arguments)?;
    let master = checkout.join(design_system_master_path(project_name));
    if output.exit_code != 0 || !master.is_file() {
        return Err(SkillLibraryError::ToolFailed {
            tool: String::from("ui-ux-pro-max search.py --design-system --persist"),
            output: tail(&format!("{}\n{}", output.stdout, output.stderr)),
        });
    }
    Ok(())
}

/// **Generate** / **Regenerate** for a project: runs the search in the main checkout, keeps the
/// AGENTS.md rule in step, and commits the design system folder and AGENTS.md, each on its own
/// and nothing else that happens to be staged, as the spec interview commits SPEC.md.
pub fn generate_design_system(
    library: &SkillLibrary,
    checkout: &Path,
    project_name: &str,
    regenerate: bool,
) -> Result<DesignSystemStatus, SkillLibraryError> {
    let spec = fs::read_to_string(checkout.join("SPEC.md")).ok();
    let query = design_system_query(project_name, spec.as_deref());
    run_design_system_search(library, checkout, project_name, &query, regenerate)?;
    velocity_git::commit_file(
        checkout,
        Path::new(&design_system_folder(project_name)),
        "Generate the design system (UI UX Pro Max)",
    )?;
    if write_design_system_rule(checkout, project_name)? {
        velocity_git::commit_file(
            checkout,
            Path::new("AGENTS.md"),
            "AGENTS.md: read the design system before UI work",
        )?;
    }
    Ok(design_system_status(checkout, project_name))
}

/// Adds or removes the design system rule in the checkout's AGENTS.md to match whether the
/// design system exists. Returns true when the file changed. A missing AGENTS.md is left
/// missing: F11's template installs it, and this only keeps one rule in step.
pub fn write_design_system_rule(
    checkout: &Path,
    project_name: &str,
) -> Result<bool, SkillLibraryError> {
    let path = checkout.join("AGENTS.md");
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(source) => return Err(SkillLibraryError::Io { path, source }),
    };
    let exists = checkout
        .join(design_system_master_path(project_name))
        .is_file();
    let rule = exists.then(|| design_system_rule(project_name));
    let Some(updated) = sync_design_system_rule(&text, rule.as_deref()) else {
        return Ok(false);
    };
    replace_file(&path, updated.as_bytes()).map_err(|source| SkillLibraryError::Io {
        path: path.clone(),
        source,
    })?;
    Ok(true)
}

// Write next to the file and rename over it, so an interrupted write never leaves a
// half-written AGENTS.md behind.
fn replace_file(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = path.with_file_name(format!(
        ".velocity-design-rule-{}.partial",
        std::process::id()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn plain_path(path: &Path) -> String {
    let text = path.to_string_lossy();
    text.strip_prefix(r"\\?\").unwrap_or(&text).to_owned()
}

fn tail(text: &str) -> String {
    let lines: Vec<&str> = text.trim().lines().collect();
    lines[lines.len().saturating_sub(12)..].join("\n")
}
