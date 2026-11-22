//! Bootstraps a folder-backed Rosen project and publishes its interview spec.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use rosen_client::protocol::{ErrorCode, LandStatus, ReadAnswer, ReadRequest};
use rosen_client::{Client, Error as ClientError};
use rosen_core::ProjectConfig;
use rosen_core::change::ChangeState;
use toml_edit::{DocumentMut, Item, TableLike, value};

use crate::workspace_settings::table;
use crate::{ImportSource, ProjectServer};

#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Rosen(#[from] crate::RosenError),
    #[error(transparent)]
    Client(#[from] ClientError),
    #[error(transparent)]
    Git(#[from] velocity_git::GitError),
    #[error(transparent)]
    Agents(#[from] velocity_git::agents_template::AgentsTemplateError),
    #[error("{0}")]
    Refused(String),
}

/// The top-level keys of a new project's `rosen.toml`. A `collapse` root follows them,
/// since a key written after a table header would belong to that table.
const DEFAULT_KEYS: &str = r#"review = "after-land"
ignore = [".env", ".env.*", "*.pem", "*.pfx", "*.p12", "*.key"]
generated = ["Cargo.lock", "pnpm-lock.yaml", "ui/pnpm-lock.yaml", "ui/src/lib/bindings.ts"]
registration_lists = ["Cargo.toml", "src-tauri/src/lib.rs", "ui/src/lib/ipc.ts"]
"#;

const DEFAULT_TABLES: &str = r#"
[warm_pool]
size = 8

[prepare]
commands = []
"#;

pub struct ProjectBase {
    pub branch: String,
    pub has_spec: bool,
}

/// Velocity's folder-backed project data is independent of the saved switcher ID.
pub fn has_local_server(root: &Path) -> bool {
    root.join(".rosen")
        .join("server")
        .join("metadata.sqlite")
        .is_file()
}

pub fn is_workspace(root: &Path) -> bool {
    root.ancestors()
        .any(|folder| rosen_client::snapshot::WorkspaceMarker::path(folder).is_file())
}

/// Records the server location for background services that start with only a project ID.
pub fn link(app_data: &Path, project: u32, root: &Path) -> Result<(), ProjectError> {
    ProjectServer::link_folder(app_data, project, root)?;
    Ok(())
}

/// Checks before provisioning external resources that `root` can be created and linked (#661).
pub fn validate_create_folder(
    root: &Path,
    require_collapse_link: bool,
) -> Result<(), ProjectError> {
    if is_workspace(root) {
        return Err(ProjectError::Refused(
            "Choose the project folder instead of a Rosen workspace.".into(),
        ));
    }
    if root.is_file() {
        return Err(ProjectError::Refused(format!(
            "Choose a folder path; {} is a file.",
            root.display()
        )));
    }
    let local_server = has_local_server(root);
    if velocity_git::is_repository(root) && !local_server {
        return Err(ProjectError::Refused(
            "Use Open project to import this Git checkout.".into(),
        ));
    }
    if require_collapse_link && !local_server {
        validate_existing_collapse_config(root)?;
    }
    Ok(())
}

/// Starts the server and establishes the base before the project is registered.
pub async fn prepare(
    root: &Path,
    name: &str,
    mirror: Option<&str>,
    opening: bool,
) -> Result<ProjectBase, ProjectError> {
    if !opening {
        validate_create_folder(root, collapse_root_for_mirror(mirror).is_some())?;
    } else if is_workspace(root) {
        return Err(ProjectError::Refused(
            "Choose the project folder instead of a Rosen workspace.".into(),
        ));
    }
    let checkout = velocity_git::is_repository(root);
    fs::create_dir_all(root)?;
    let server = ProjectServer::for_folder(root)?;
    let client = server.connect().await?;
    let branch = match base_branch(&client).await? {
        Some(branch) => {
            if !opening && let Some(expected_collapse) = collapse_root_for_mirror(mirror) {
                let current = client.base_config().await?;
                if current.collapse.as_deref() != Some(expected_collapse.as_str())
                    || current.mirror.remote.as_deref() != mirror
                {
                    return Err(ProjectError::Refused(
                        "This folder already has a Rosen project. Use Open project to open it."
                            .into(),
                    ));
                }
            }
            branch
        }
        None => {
            if checkout && !opening {
                return Err(ProjectError::Refused(
                    "Use Open project to import this Git checkout.".into(),
                ));
            }
            import_initial_base(&server, &client, root, name, mirror, checkout).await?
        }
    };
    let has_spec = if opening {
        let spec = spec_sources(&client, "SPEC.md").await?;
        copy_sources(root, &spec, false)?;
        !spec.is_empty()
    } else {
        false
    };
    Ok(ProjectBase { branch, has_spec })
}

async fn base_branch(client: &Client) -> Result<Option<String>, ClientError> {
    match client.project_config(None).await {
        Ok(record) => Ok(record.base_ref),
        Err(ClientError::Server(error)) if error.code == ErrorCode::Refused => Ok(None),
        Err(error) => Err(error),
    }
}

fn collapse_root_for_mirror(mirror: Option<&str>) -> Option<String> {
    mirror
        .and_then(velocity_core::collapse::repository_from_remote)
        .and_then(|repository| velocity_core::collapse::collapse_root(&repository))
}

async fn import_initial_base(
    server: &ProjectServer,
    client: &Client,
    root: &Path,
    name: &str,
    mirror: Option<&str>,
    checkout: bool,
) -> Result<String, ProjectError> {
    let branch = if checkout {
        velocity_git::opened_project_default_branch(root)?
    } else {
        String::from("main")
    };
    let inherited = checkout.then(|| ImportSource::for_checkout(root, &branch));
    // Only a Collapse mirror chosen here sends live state at once (#517): Velocity has
    // just stored its push token for the feed. An inherited `origin` on Collapse keeps
    // mirroring, and the project offers Send Collapse its live state.
    let collapse = collapse_root_for_mirror(mirror);
    let mirror = mirror
        .or_else(|| inherited.as_ref()?.mirror.as_deref())
        .filter(|remote| {
            !remote.split_once("://").is_some_and(|(_, address)| {
                address
                    .split('/')
                    .next()
                    .is_some_and(|authority| authority.contains('@'))
            })
        });
    write_defaults(root, mirror, collapse.as_deref(), !checkout)?;
    velocity_git::agents_template::install_default_agents(root, name)?;
    // The pinned Rosen import requires a Git commit. Use an isolated bare repository
    // so the user's checkout, index and refs stay independent of the Rosen base.
    let temporary = tempfile::tempdir()?;
    let seed = temporary.path().join("seed.git");
    if checkout {
        seed_checkout(root, &seed, &branch)?;
    } else {
        seed_new(root, &seed, &branch)?;
    }
    let source = ImportSource {
        source: seed.to_string_lossy().into_owned(),
        branch: branch.clone(),
        mirror: mirror.map(str::to_owned),
    };
    server.import(client, &source).await?;
    base_branch(client)
        .await?
        .ok_or_else(|| ProjectError::Refused("Rosen did not establish the project base".into()))
}

const VALIDATION_COLLAPSE_MIRROR: &str = "https://api.collapse.sh/example/project.git";
const VALIDATION_COLLAPSE_ROOT: &str = "https://api.collapse.sh/example/project";

fn validate_existing_collapse_config(root: &Path) -> Result<(), ProjectError> {
    let path = root.join("rosen.toml");
    let Some(source) = read_existing_config(&path)? else {
        return Ok(());
    };
    let _ = edit_initial_config(
        &source,
        Some(VALIDATION_COLLAPSE_MIRROR),
        Some(VALIDATION_COLLAPSE_ROOT),
    )?;
    Ok(())
}

fn write_defaults(
    root: &Path,
    mirror: Option<&str>,
    collapse: Option<&str>,
    update_existing: bool,
) -> Result<(), ProjectError> {
    let path = root.join("rosen.toml");
    let mirror = mirror.filter(|remote| !remote.contains('@'));
    let collapse = collapse.filter(|_| mirror.is_some());
    let link_existing = update_existing && (mirror.is_some() || collapse.is_some());
    if !link_existing {
        match fs::symlink_metadata(&path) {
            Ok(_) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(ProjectError::Io(error)),
        }
    } else if let Some(source) = read_existing_config(&path)? {
        if let Some(updated) = edit_initial_config(&source, mirror, collapse)? {
            write_config_atomic(root, &path, &updated, true)?;
        }
        return Ok(());
    }
    let content = default_config_text(mirror, collapse)?;
    match write_config_atomic(root, &path, &content, false) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if link_existing
                && let Some(source) = read_existing_config(&path)?
                && let Some(updated) = edit_initial_config(&source, mirror, collapse)?
            {
                write_config_atomic(root, &path, &updated, true)?;
            }
            Ok(())
        }
        Err(error) => Err(ProjectError::Io(error)),
    }
}

fn default_config_text(mirror: Option<&str>, collapse: Option<&str>) -> io::Result<String> {
    // JSON's quoted string syntax is also valid for these TOML URL values.
    let quoted = |url: &str| serde_json::to_string(url).map_err(io::Error::other);
    let mut content = DEFAULT_KEYS.to_owned();
    // The root is written only with the mirror it belongs to (Rosen R9, R13, R14).
    if let Some(collapse) = collapse {
        content.push_str(&format!("collapse = {}\n", quoted(collapse)?));
    }
    content.push_str(DEFAULT_TABLES);
    if let Some(mirror) = mirror {
        content.push_str(&format!("\n[mirror]\nremote = {}\n", quoted(mirror)?));
    }
    Ok(content)
}

fn read_existing_config(path: &Path) -> Result<Option<String>, ProjectError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        Ok(_) => {
            return Err(ProjectError::Refused(format!(
                "Existing {} is not a regular file and cannot be linked.",
                path.display()
            )));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(ProjectError::Io(error)),
    }
    let bytes = fs::read(path)?;
    let source = String::from_utf8(bytes).map_err(|error| {
        ProjectError::Refused(format!(
            "Existing {} is not valid UTF-8 and cannot be linked: {error}",
            path.display()
        ))
    })?;
    Ok(Some(source))
}

/// Preserves existing `rosen.toml` settings and comments while adding the chosen
/// `[mirror] remote` and top-level `collapse` root (#661).
fn edit_initial_config(
    source: &str,
    mirror: Option<&str>,
    collapse: Option<&str>,
) -> Result<Option<String>, ProjectError> {
    ProjectConfig::parse(source).map_err(|error| {
        ProjectError::Refused(format!(
            "Existing rosen.toml is invalid and cannot be linked: {error}"
        ))
    })?;
    let collapse = collapse.filter(|_| mirror.is_some());
    if mirror.is_none() && collapse.is_none() {
        return Ok(None);
    }
    let mut document = source.parse::<DocumentMut>().map_err(|error| {
        ProjectError::Refused(format!(
            "Existing rosen.toml is invalid and cannot be linked: {error}"
        ))
    })?;
    let current_remote = document
        .get("mirror")
        .and_then(|section| section.get("remote"))
        .and_then(Item::as_str);
    let desired_remote = mirror.filter(|remote| current_remote != Some(*remote));
    let desired_collapse =
        collapse.filter(|root| document.get("collapse").and_then(Item::as_str) != Some(*root));
    if desired_remote.is_none() && desired_collapse.is_none() {
        return Ok(None);
    }
    if let Some(root) = desired_collapse {
        set_string_value(document.as_table_mut(), "collapse", root);
    }
    if let Some(remote) = desired_remote {
        let mirror_table = table(&mut document, "mirror").map_err(|error| {
            ProjectError::Refused(format!("Existing rosen.toml cannot be linked: {error}"))
        })?;
        set_string_value(mirror_table, "remote", remote);
    }
    let updated = document.to_string();
    verify_linked_config(&updated, mirror, collapse)?;
    Ok(Some(updated))
}

fn verify_linked_config(
    updated: &str,
    mirror: Option<&str>,
    collapse: Option<&str>,
) -> Result<(), ProjectError> {
    let parsed = ProjectConfig::parse(updated).map_err(|error| {
        ProjectError::Refused(format!("Updated rosen.toml is invalid: {error}"))
    })?;
    if let Some(remote) = mirror
        && parsed.mirror.remote.as_deref() != Some(remote)
    {
        return Err(ProjectError::Refused(
            "Existing rosen.toml prevented setting [mirror] remote.".into(),
        ));
    }
    if let Some(root) = collapse
        && parsed.collapse.as_deref() != Some(root)
    {
        return Err(ProjectError::Refused(
            "Existing rosen.toml prevented setting top-level collapse.".into(),
        ));
    }
    Ok(())
}

fn set_string_value(section: &mut dyn TableLike, key: &str, text: &str) {
    let mut replacement = value(text);
    if let Some(existing) = section.get_mut(key) {
        if let (Some(previous), Some(updated)) = (existing.as_value(), replacement.as_value_mut()) {
            *updated.decor_mut() = previous.decor().clone();
        }
        *existing = replacement;
    } else {
        section.insert(key, replacement);
    }
}

fn write_config_atomic(root: &Path, path: &Path, content: &str, replace: bool) -> io::Result<()> {
    let mut temporary = tempfile::NamedTempFile::new_in(root)?;
    temporary.write_all(content.as_bytes())?;
    if replace {
        temporary.persist(path).map_err(|error| error.error)?;
    } else {
        temporary
            .persist_noclobber(path)
            .map_err(|error| error.error)?;
    }
    Ok(())
}

fn seed_new(root: &Path, seed: &Path, branch: &str) -> Result<(), ProjectError> {
    git(root, seed, &["init", "--bare", "--initial-branch", branch])?;
    fs::write(
        seed.join("info/exclude"),
        ".rosen/\n.env\n.env.*\n*.pem\n*.pfx\n*.p12\n*.key\n",
    )?;
    // Pathspec exclusions apply even if the project's ignore file re-includes a path.
    git(
        root,
        seed,
        &[
            "add",
            "--all",
            "--",
            ".",
            ":(exclude,glob)**/.rosen/**",
            ":(exclude,glob)**/.env",
            ":(exclude,glob)**/.env.*",
            ":(exclude,glob)**/*.pem",
            ":(exclude,glob)**/*.pfx",
            ":(exclude,glob)**/*.p12",
            ":(exclude,glob)**/*.key",
        ],
    )?;
    git(
        root,
        seed,
        &["add", "--force", "--", "rosen.toml", "AGENTS.md"],
    )?;
    seed_commit(root, seed)
}

fn seed_checkout(root: &Path, seed: &Path, branch: &str) -> Result<(), ProjectError> {
    let output = crate::process::hidden_command("git")
        .args([
            "-c",
            "core.longpaths=true",
            "clone",
            "--mirror",
            "--quiet",
            "--",
        ])
        .arg(root)
        .arg(seed)
        .output()?;
    checked_output(output)?;
    let remote = format!("refs/remotes/origin/{branch}");
    let local = format!("refs/heads/{branch}");
    let reference = if git(root, seed, &["rev-parse", "--verify", &local]).is_ok() {
        Some(&local)
    } else if git(root, seed, &["rev-parse", "--verify", &remote]).is_ok() {
        Some(&remote)
    } else {
        None
    };
    let Some(reference) = reference else {
        // An empty repository (such as a brand-new Collapse web repo) has no commits yet;
        // seed its initial base commit with rosen.toml and AGENTS.md (#575).
        fs::remove_dir_all(seed)?;
        return seed_new(root, seed, branch);
    };
    let tip = git(root, seed, &["rev-parse", "--verify", reference])?;
    git(root, seed, &["update-ref", &local, tip.trim()])?;
    git(root, seed, &["symbolic-ref", "HEAD", &local])?;
    git(root, seed, &["read-tree", "HEAD"])?;
    let mut changed = false;
    for path in ["rosen.toml", "AGENTS.md"] {
        if git(root, seed, &["cat-file", "-e", &format!("HEAD:{path}")]).is_err() {
            git(root, seed, &["add", "--force", "--", path])?;
            changed = true;
        }
    }
    if changed {
        seed_commit(root, seed)?;
    }
    Ok(())
}

fn seed_commit(root: &Path, seed: &Path) -> Result<(), ProjectError> {
    git(
        root,
        seed,
        &[
            "-c",
            "user.name=Velocity",
            "-c",
            "user.email=velocity@localhost",
            "commit",
            "--no-gpg-sign",
            "-m",
            "Set up Rosen project",
        ],
    )?;
    Ok(())
}

fn git(root: &Path, seed: &Path, arguments: &[&str]) -> Result<String, ProjectError> {
    let mut command = crate::process::hidden_command("git");
    command
        .args([
            "-c",
            "core.autocrlf=false",
            "-c",
            "core.longpaths=true",
            "-c",
        ])
        .arg(format!("core.hooksPath={}", seed.join("hooks").display()))
        .arg("--git-dir")
        .arg(seed)
        .current_dir(root);
    if arguments.first() != Some(&"init") {
        command.arg("--work-tree").arg(root);
    }
    checked_output(command.args(arguments).output()?)
}

fn checked_output(output: std::process::Output) -> Result<String, ProjectError> {
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(ProjectError::Refused(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ))
    }
}

async fn spec_sources(
    client: &Client,
    spec_path: &str,
) -> Result<Vec<(PathBuf, String)>, ProjectError> {
    let mut files = Vec::new();
    let folder = velocity_spec::FOLDER;
    let mut pending = vec![folder.to_owned(), spec_path.replace('\\', "/")];
    while let Some(path) = pending.pop() {
        let request = ReadRequest {
            at: None,
            path: path.clone(),
        };
        match client.read_file(request).await {
            Ok(ReadAnswer::File { mode, data, .. })
                if path != folder && matches!(mode.as_str(), "100644" | "100755") =>
            {
                let markdown = String::from_utf8(data).map_err(io::Error::other)?;
                files.push((PathBuf::from(path), markdown));
            }
            Ok(ReadAnswer::Directory { entries, .. })
                if path == folder || path.starts_with("spec/") =>
            {
                pending.extend(entries.into_iter().filter_map(|entry| {
                    let markdown = Path::new(&entry.name)
                        .extension()
                        .is_some_and(|ext| ext.eq_ignore_ascii_case("md"));
                    (entry.mode == "40000" || markdown).then(|| format!("{path}/{}", entry.name))
                }));
            }
            Ok(_) => {}
            Err(ClientError::Server(error)) if error.code == ErrorCode::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(files)
}

fn copy_sources(root: &Path, files: &[(PathBuf, String)], replace: bool) -> io::Result<()> {
    let root = root.canonicalize()?;
    for (path, markdown) in files {
        let target = root.join(path);
        if !replace && target.exists() {
            continue;
        }
        let parent = spec_parent(&root, path)?;
        // A symlink at the destination is replaced atomically and never followed.
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(markdown.as_bytes())?;
        if replace {
            temporary.persist(target)?;
        } else if let Err(error) = temporary.persist_noclobber(target)
            && error.error.kind() != io::ErrorKind::AlreadyExists
        {
            return Err(error.error);
        }
    }
    Ok(())
}

fn spec_parent(root: &Path, file: &Path) -> io::Result<PathBuf> {
    let mut parent = root.to_path_buf();
    let relative = file
        .parent()
        .ok_or_else(|| io::Error::other("Missing spec parent folder"))?;
    for component in relative.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(io::Error::other("The spec path points outside the project"));
        };
        parent.push(name);
        match fs::create_dir(&parent) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        parent = parent.canonicalize()?;
        if !parent.starts_with(root) {
            return Err(io::Error::other(
                "The spec folder points outside the project",
            ));
        }
    }
    Ok(parent)
}

/// Stores and lands the completed interview before Velocity unlocks build work.
pub async fn publish_spec(
    server: &ProjectServer,
    workspace: &Path,
    repository: &Path,
    change: &str,
    spec_path: &str,
) -> Result<(), ProjectError> {
    let client = server.connect().await?;
    if client.change_show(change).await?.change.state != ChangeState::Landed {
        let agent = crate::agent_name(workspace);
        let session = crate::workspace::session(server, &agent).await?;
        session
            .snapshot(workspace, Some("Lock spec".to_owned()))
            .await?;
        let answer = session.land(change).await?;
        if let LandStatus::Refused { message, .. } = answer.outcome {
            return Err(ProjectError::Refused(message));
        }
    }
    // A successful land removes the workspace. Reading from the base also lets
    // Lock retry if copying the spec or saving the interview state failed.
    let files = spec_sources(&client, spec_path).await?;
    if files.is_empty() {
        return Err(ProjectError::Refused(
            "The Rosen base has no spec files".into(),
        ));
    }
    copy_sources(repository, &files, true)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ProjectError, validate_create_folder, write_defaults};
    use rosen_core::{ProjectConfig, ReviewMode};
    use std::fs;

    // Risk (#661): Create in a non-Git folder that already contains rosen.toml must preserve
    // existing settings and write both top-level collapse and [mirror] remote before seed_new.
    #[test]
    fn create_preserves_existing_config_and_links_collapse()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let config_path = directory.path().join("rosen.toml");
        let existing = "# Project policy\nreview = \"after-land\"\nsensitive = [\"Cargo.toml\"]\n\n[warm_pool]\nsize = 4\n";
        fs::write(&config_path, existing)?;

        let mirror = "https://api.collapse.sh/example/project.git";
        let root = "https://api.collapse.sh/example/project";
        validate_create_folder(directory.path(), true)?;
        write_defaults(directory.path(), Some(mirror), Some(root), true)?;

        let updated = fs::read_to_string(&config_path)?;
        assert!(updated.contains("# Project policy"));
        let parsed = ProjectConfig::parse(&updated)?;
        assert_eq!(parsed.review, ReviewMode::AfterLand);
        assert_eq!(parsed.warm_pool.size, 4);
        assert_eq!(parsed.collapse.as_deref(), Some(root));
        assert_eq!(parsed.mirror.remote.as_deref(), Some(mirror));
        Ok(())
    }

    // Risk (#661): A table-first rosen.toml must still place collapse at top level rather than
    // inside the leading table, and Open must leave an existing Git checkout's file untouched.
    #[test]
    fn create_places_collapse_above_leading_table_and_open_keeps_existing()
    -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let config_path = directory.path().join("rosen.toml");
        let existing = "[warm_pool]\nsize = 3\n";
        fs::write(&config_path, existing)?;

        let mirror = "https://api.collapse.sh/example/project.git";
        let root = "https://api.collapse.sh/example/project";
        write_defaults(directory.path(), Some(mirror), Some(root), false)?;
        assert_eq!(fs::read_to_string(&config_path)?, existing);

        write_defaults(directory.path(), Some(mirror), Some(root), true)?;
        let parsed = ProjectConfig::parse(&fs::read_to_string(&config_path)?)?;
        assert_eq!(parsed.warm_pool.size, 3);
        assert_eq!(parsed.collapse.as_deref(), Some(root));
        assert_eq!(parsed.mirror.remote.as_deref(), Some(mirror));
        Ok(())
    }

    // Risk (#661): An invalid or non-file rosen.toml must be refused before provisioning
    // creates a Collapse repository.
    #[test]
    fn validate_create_folder_refuses_unlinkable_existing_config()
    -> Result<(), Box<dyn std::error::Error>> {
        let invalid_toml = tempfile::tempdir()?;
        fs::write(invalid_toml.path().join("rosen.toml"), "review = [\n")?;
        assert!(matches!(
            validate_create_folder(invalid_toml.path(), true),
            Err(ProjectError::Refused(_))
        ));

        let unknown_key = tempfile::tempdir()?;
        fs::write(
            unknown_key.path().join("rosen.toml"),
            "review = \"after-land\"\nunknown_setting = true\n",
        )?;
        assert!(matches!(
            validate_create_folder(unknown_key.path(), true),
            Err(ProjectError::Refused(_))
        ));

        let folder_as_config = tempfile::tempdir()?;
        fs::create_dir(folder_as_config.path().join("rosen.toml"))?;
        assert!(matches!(
            validate_create_folder(folder_as_config.path(), true),
            Err(ProjectError::Refused(_))
        ));
        Ok(())
    }
}
