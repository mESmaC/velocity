//! Skills linked at an agent's final workspace path, outside Rosen's prepared template.
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use velocity_core::skill_menu::{SkillMenuEntry, requested_skills};
use velocity_core::skills::{PackSelection, ProjectSkills};

use crate::{SkillLibraryError, SkillLibrarySnapshot, settings::skill_enabled};

/// F18's provider-folder table. Kimi also reads the standard folder until Q26 is verified.
pub const PROVIDER_SKILL_FOLDERS: &[(&str, &str)] = &[
    ("Claude Code", ".claude/skills"),
    ("Codex", ".agents/skills"),
    ("Antigravity", ".agents/skills"),
    ("Kimi Code", ".kimi-code/skills"),
    ("Kimi Code", ".agents/skills"),
];

const INVOKED_SKILLS_FOLDER: &str = ".velocity/invoked-skills";

/// Run only after Rosen has placed a workspace at its final path. Pool preparation owns
/// toolchains and builds; this order gives the setup command the agent's files and skills.
pub fn finish_workspace_setup(
    app_data: &Path,
    choices: &ProjectSkills,
    request: &velocity_git::WorktreeSetupRequest,
) -> Result<(), SkillLibraryError> {
    let mut request = request.clone();
    if request.settings.copy_untracked_files {
        let paths = velocity_git::parse_untracked_file_list(&request.untracked_files)?;
        velocity_git::copy_configured_untracked_files(
            &request.repository_root,
            &request.worktree_path,
            &paths,
        )?;
    }
    let library = crate::SkillLibrary::open(app_data)?.snapshot()?;
    link_workspace_skills(choices, &library, &request.worktree_path)?;
    request.settings.copy_untracked_files = false;
    velocity_git::finish_worktree_setup(&request)?;
    Ok(())
}

/// Links always-on skills for every provider. Existing project skills win name collisions.
/// Local, self-ignored rules keep the links out of Rosen snapshots even after a rebase;
/// changing Rosen's cached project ignore list would be lost at the next manual rebase.
pub fn link_workspace_skills(
    choices: &ProjectSkills,
    library: &SkillLibrarySnapshot,
    workspace: &Path,
) -> Result<(), SkillLibraryError> {
    let folders: BTreeSet<_> = PROVIDER_SKILL_FOLDERS
        .iter()
        .map(|(_, folder)| *folder)
        .collect();
    link_skills(workspace, &enabled_links(choices, library), folders)
}

/// Resolve at the turn boundary, since a running agent may have indexed skills at startup.
pub fn prepare_skill_prompt(
    message: &str,
    library: &SkillLibrarySnapshot,
    workspace: &Path,
) -> Option<String> {
    let installed = crate::menu::installed_skills(library);
    let entries = installed
        .keys()
        .map(|name| SkillMenuEntry::new(name, ""))
        .collect::<Vec<_>>();
    let links = requested_skills(message, &entries)
        .into_iter()
        .filter_map(|name| {
            let (source, _) = installed.get(name)?;
            Some((name.to_owned(), source.clone()))
        })
        .collect::<BTreeMap<_, _>>();
    if links.is_empty() {
        return None;
    }
    // A subsequent session in the same checkout must not auto-load an invoked skill.
    let linked = link_skills(workspace, &links, [INVOKED_SKILLS_FOLDER]);
    if let Err(error) = linked {
        eprintln!("Could not link invoked skills: {error}");
    }
    let library_root = fs::canonicalize(&library.library_root).ok();
    let manifests = links
        .iter()
        .map(|(name, source)| {
            let manifest = invocation_manifest(workspace, name, source, library_root.as_deref());
            format!("- {}", manifest.display())
        })
        .collect::<Vec<_>>();
    Some(format!(
        "Read the full skill instructions for this turn from these SKILL.md files before starting:\n{}",
        manifests.join("\n")
    ))
}

fn invocation_manifest(
    workspace: &Path,
    name: &str,
    source: &Path,
    library_root: Option<&Path>,
) -> PathBuf {
    let project_skill = workspace.join(".agents/skills").join(name);
    let project_manifest = project_skill.join("SKILL.md");
    if project_manifest.is_file()
        && let Ok(target) = fs::canonicalize(project_skill)
        && library_root.is_some_and(|root| !target.starts_with(root))
    {
        return project_manifest;
    }
    // An alias can point to a different pack after a library change.
    let invoked_skill = workspace.join(INVOKED_SKILLS_FOLDER).join(name);
    if let Ok(target) = fs::canonicalize(&invoked_skill)
        && fs::canonicalize(source).is_ok_and(|expected| expected == target)
    {
        return invoked_skill.join("SKILL.md");
    }
    source.join("SKILL.md")
}

fn link_skills<'a>(
    workspace: &Path,
    links: &BTreeMap<String, PathBuf>,
    folders: impl IntoIterator<Item = &'a str>,
) -> Result<(), SkillLibraryError> {
    if links.is_empty() {
        return Ok(());
    }
    // Multiple sessions can share the main checkout and its ignore files.
    static LINKING: Mutex<()> = Mutex::new(());
    let _linking = LINKING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let root = fs::canonicalize(workspace).map_err(|source| error(workspace, source))?;
    let exclude = if root.join(".git").exists() && !root.join(".rosen").exists() {
        let path = crate::library::run_git(
            &root,
            [
                "rev-parse",
                "--path-format=absolute",
                "--git-path",
                "info/exclude",
            ],
        )?;
        Some(PathBuf::from(path.trim()))
    } else {
        None
    };
    for relative in folders {
        let folder = create_folder(&root, relative).map_err(|source| error(&root, source))?;
        for (name, source) in links {
            let destination = folder.join(name);
            if find_link_destination(&destination).map_err(|source| error(&destination, source))? {
                continue;
            }
            let ignored = match &exclude {
                Some(path) => append_ignore_rule(path, &format!("/{relative}/{name}")),
                None => exclude_link(&folder, name),
            };
            ignored.map_err(|source| error(&folder, source))?;
            link_directory(source, &destination).map_err(|source| error(&destination, source))?;
        }
    }
    Ok(())
}

fn find_link_destination(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn enabled_links(
    choices: &ProjectSkills,
    library: &SkillLibrarySnapshot,
) -> BTreeMap<String, PathBuf> {
    let mut links = BTreeMap::new();
    for pack in &library.installed {
        let selection = choices
            .packs
            .get(&pack.id)
            .cloned()
            .unwrap_or_else(|| PackSelection::for_pack(&pack.id));
        for skill in &pack.skills {
            if !skill.mirror
                && valid_name(&skill.name)
                && skill_enabled(&pack.id, &selection, skill)
            {
                links.entry(skill.name.clone()).or_insert_with(|| {
                    PathBuf::from(&pack.clone_path).join(&skill.relative_folder)
                });
            }
        }
    }
    links
}

pub(crate) fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
}

/// Never create a provider folder through a project-owned link into another checkout.
fn create_folder(root: &Path, relative: &str) -> io::Result<PathBuf> {
    let mut folder = root.to_path_buf();
    for segment in relative.split('/') {
        folder.push(segment);
        match fs::symlink_metadata(&folder) {
            Ok(metadata) if metadata.is_dir() && !is_link(&metadata) => {}
            Ok(_) => {
                return Err(io::Error::other(format!(
                    "{} is not an ordinary skill directory",
                    folder.display()
                )));
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => fs::create_dir(&folder)?,
            Err(error) => return Err(error),
        }
    }
    Ok(folder)
}

fn exclude_link(folder: &Path, name: &str) -> io::Result<()> {
    let path = folder.join(".gitignore");
    if !path.exists() {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(b"# Local Velocity skill links.\n/.gitignore\n")?;
    }
    append_ignore_rule(&path, &format!("/{name}"))
}

fn append_ignore_rule(path: &Path, rule: &str) -> io::Result<()> {
    let previous = match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !is_link(&metadata) => fs::read(path)?,
        Ok(_) => {
            return Err(io::Error::other(
                "the skill ignore file is not a regular file",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => return Err(error),
    };
    if !String::from_utf8_lossy(&previous)
        .lines()
        .any(|line| line == rule)
    {
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(file, "\n{rule}")?;
    }
    Ok(())
}

#[cfg(windows)]
fn is_link(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_link(metadata: &fs::Metadata) -> bool {
    metadata.is_symlink()
}

fn link_directory(source: &Path, destination: &Path) -> io::Result<()> {
    let source = fs::canonicalize(source)?;
    #[cfg(windows)]
    {
        junction::create(source, destination)
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, destination)
    }
}

fn error(path: &Path, source: io::Error) -> SkillLibraryError {
    SkillLibraryError::Io {
        path: path.to_path_buf(),
        source,
    }
}
