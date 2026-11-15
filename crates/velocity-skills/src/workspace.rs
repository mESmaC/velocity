//! Skills linked at an agent's final workspace path, outside Rosen's prepared template.
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

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

/// Links enabled skills for every provider. Existing project skills win name collisions.
/// Local, self-ignored rules keep the links out of Rosen snapshots even after a rebase;
/// changing Rosen's cached project ignore list would be lost at the next manual rebase.
pub fn link_workspace_skills(
    choices: &ProjectSkills,
    library: &SkillLibrarySnapshot,
    workspace: &Path,
) -> Result<(), SkillLibraryError> {
    let links = enabled_links(choices, library);
    if links.is_empty() {
        return Ok(());
    }
    let root = fs::canonicalize(workspace).map_err(|source| error(workspace, source))?;
    let mut folders = std::collections::BTreeSet::new();
    for (_, relative) in PROVIDER_SKILL_FOLDERS {
        if !folders.insert(relative) {
            continue;
        }
        let folder = create_folder(&root, relative).map_err(|source| error(&root, source))?;
        for (name, source) in &links {
            let destination = folder.join(name);
            match fs::symlink_metadata(&destination) {
                Ok(_) => continue,
                Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                Err(source) => return Err(error(&destination, source)),
            }
            exclude_link(&folder, name).map_err(|source| error(&folder, source))?;
            link_directory(source, &destination).map_err(|source| error(&destination, source))?;
        }
    }
    Ok(())
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

fn valid_name(name: &str) -> bool {
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
    let previous = match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.is_file() && !is_link(&metadata) => fs::read_to_string(&path)?,
        Ok(_) => {
            return Err(io::Error::other(
                "the skill ignore file is not a regular file",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)?;
            file.write_all(b"# Local Velocity skill links.\n/.gitignore\n")?;
            String::new()
        }
        Err(error) => return Err(error),
    };
    let rule = format!("/{name}");
    if !previous.lines().any(|line| line == rule) {
        let mut file = OpenOptions::new().append(true).open(path)?;
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
