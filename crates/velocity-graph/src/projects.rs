//! Read-only access to sibling projects registered in Velocity (#600):
//! - Lists configured projects from Velocity's atomically replaced state snapshots without
//!   mutating or migrating files on disk.
//! - Resolves a `project` argument by name, `owner/repository`, short repo name, or ID.
//! - Implements `project_read { project, path, offset?, limit? }` over a project's current base
//!   (Rosen `base` tree when running, otherwise its base checkout on disk), refusing paths
//!   outside the checkout and files Rosen marks secret or `.env`.

use std::collections::HashMap;
use std::fmt::Write;
use std::fs;
use std::path::{Component, Path, PathBuf};

use ignore::gitignore::GitignoreBuilder;
use rosen_core::project_config::{CONFIG_PATH, ProjectConfig};
use serde::Deserialize;
use serde_json::Value;

use crate::query::BUDGET;
use crate::rosen::{RosenBaseError, RosenBaseRead};
use crate::tools::CallError;
use crate::usage::ToolOutcome;
use crate::{Error, Index, Tools};

const STATE_FILE_NAME: &str = "state.json";
const PROJECTS_DIR_NAME: &str = "projects";
const DEFAULT_READ_LIMIT: usize = 200;
const MAX_READ_LIMIT: usize = 400;
const MAX_READ_BYTES: u64 = 4 * 1024 * 1024;

/// Summary of one project registered in Velocity, loaded read-only from app data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredProject {
    pub id: u32,
    pub name: String,
    pub repository_path: PathBuf,
    pub default_branch: String,
    pub github_remote: Option<String>,
    pub collapse_repository: Option<String>,
    pub is_rosen: bool,
}

impl RegisteredProject {
    fn repo_identifiers(&self) -> impl Iterator<Item = &str> {
        self.github_remote
            .as_deref()
            .into_iter()
            .chain(self.collapse_repository.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }

    fn disambiguator(&self) -> String {
        self.repo_identifiers()
            .next()
            .map(str::to_owned)
            .unwrap_or_else(|| format!("#{}", self.id))
    }
}

#[derive(Deserialize)]
struct RootProjectIndex {
    #[serde(default)]
    projects: Vec<RawProject>,
    #[serde(default)]
    project_ids: Vec<u32>,
}

#[derive(Deserialize)]
struct ProjectActiveFile {
    project: RawProject,
}

#[derive(Deserialize)]
struct RawProject {
    id: u32,
    name: String,
    repository_path: String,
    #[serde(default)]
    default_branch: String,
    #[serde(default)]
    github_remote: Option<String>,
    #[serde(default)]
    collapse_repository: Option<String>,
    #[serde(default = "default_legacy_version_control")]
    version_control: String,
}

fn default_legacy_version_control() -> String {
    String::from("git")
}

impl From<RawProject> for RegisteredProject {
    fn from(raw: RawProject) -> Self {
        let default_branch = if raw.default_branch.trim().is_empty() {
            String::from("master")
        } else {
            raw.default_branch.trim().to_owned()
        };
        Self {
            id: raw.id,
            name: raw.name.trim().to_owned(),
            repository_path: PathBuf::from(raw.repository_path),
            default_branch,
            github_remote: raw.github_remote,
            collapse_repository: raw.collapse_repository,
            is_rosen: raw.version_control.eq_ignore_ascii_case("rosen"),
        }
    }
}

/// Lists all projects registered in `app_data` read-only, supporting both split
/// (`state.json` + `projects/<id>.json`) and legacy monolithic `state.json` layouts.
pub fn list_projects(app_data: &Path) -> Vec<RegisteredProject> {
    let root_path = extended_windows_path(&app_data.join(STATE_FILE_NAME));
    let Ok(bytes) = fs::read(&root_path) else {
        return Vec::new();
    };
    let Ok(root) = serde_json::from_slice::<RootProjectIndex>(&bytes) else {
        return Vec::new();
    };
    let mut projects: Vec<RegisteredProject> = if !root.projects.is_empty() {
        root.projects.into_iter().map(Into::into).collect()
    } else {
        root.project_ids
            .into_iter()
            .filter_map(|id| read_split_project(app_data, id))
            .collect()
    };
    projects.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
            .then_with(|| left.id.cmp(&right.id))
    });
    projects
}

fn read_split_project(app_data: &Path, id: u32) -> Option<RegisteredProject> {
    let path = extended_windows_path(&app_data.join(PROJECTS_DIR_NAME).join(format!("{id}.json")));
    let bytes = fs::read(path).ok()?;
    let active: ProjectActiveFile = serde_json::from_slice(&bytes).ok()?;
    Some(active.project.into())
}

/// Formats registered project names concisely for MCP tool descriptions.
pub fn format_readable_projects(projects: &[RegisteredProject]) -> String {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for project in projects {
        *counts.entry(project.name.to_ascii_lowercase()).or_insert(0) += 1;
    }
    let labels: Vec<String> = projects
        .iter()
        .map(|project| {
            let duplicate = counts
                .get(&project.name.to_ascii_lowercase())
                .copied()
                .unwrap_or(0)
                > 1;
            if duplicate {
                format!("{} ({})", project.name, project.disambiguator())
            } else {
                project.name.clone()
            }
        })
        .collect();
    labels.join(", ")
}

/// Resolves a `project` argument against Velocity's registered projects.
pub fn resolve_project(
    projects: &[RegisteredProject],
    requested: &str,
) -> Result<RegisteredProject, String> {
    let query = requested.trim();
    if query.is_empty() {
        return Err("`project` must be a non-empty project name".into());
    }
    if let Some(found) = match_unique(projects, query, |project| {
        project.name.eq_ignore_ascii_case(query)
    })? {
        return Ok(found);
    }
    if let Some(found) = match_unique(projects, query, |project| {
        project
            .repo_identifiers()
            .any(|repo| repo.eq_ignore_ascii_case(query))
    })? {
        return Ok(found);
    }
    if let Some(found) = match_unique(projects, query, |project| {
        project.repo_identifiers().any(|repo| {
            repo.rsplit_once('/')
                .is_some_and(|(_, short)| short.eq_ignore_ascii_case(query))
        })
    })? {
        return Ok(found);
    }
    if let Ok(id) = query.parse::<u32>()
        && let Some(project) = projects.iter().find(|project| project.id == id)
    {
        return Ok(project.clone());
    }
    if projects.is_empty() {
        return Err(format!(
            "Unknown project `{query}`. No projects are currently registered in Velocity."
        ));
    }
    Err(format!(
        "Unknown project `{query}`. Readable projects: {}.",
        format_readable_projects(projects)
    ))
}

fn match_unique(
    projects: &[RegisteredProject],
    query: &str,
    predicate: impl Fn(&RegisteredProject) -> bool,
) -> Result<Option<RegisteredProject>, String> {
    let matches: Vec<&RegisteredProject> = projects.iter().filter(|p| predicate(p)).collect();
    match matches.as_slice() {
        [] => Ok(None),
        [one] => Ok(Some((*one).clone())),
        many => {
            let candidates: Vec<String> = many
                .iter()
                .map(|project| format!("{} ({})", project.name, project.disambiguator()))
                .collect();
            Err(format!(
                "Ambiguous project `{query}`; matches: {}. Pass owner/repo or project ID.",
                candidates.join(", ")
            ))
        }
    }
}

/// Normalizes a repository-relative path and refuses absolute paths, `..` traversal,
/// NUL bytes, and internal `.git` / `.rosen` metadata directories.
pub fn normalize_relative_path(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("`path` is required".into());
    }
    if trimmed.contains('\0') {
        return Err(format!("path `{raw}` is invalid"));
    }
    if trimmed.starts_with('/')
        || trimmed.starts_with('\\')
        || Path::new(trimmed).is_absolute()
        || has_windows_drive_prefix(trimmed)
    {
        return Err(format!("path `{raw}` is outside the project checkout"));
    }
    let slashed = trimmed.replace('\\', "/");
    let mut parts = Vec::new();
    for component in Path::new(&slashed).components() {
        match component {
            Component::Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(format!("path `{raw}` is outside the project checkout"));
            }
        }
    }
    if parts.is_empty() {
        return Err(format!("path `{raw}` must name a file inside the checkout"));
    }
    if matches!(parts[0].to_ascii_lowercase().as_str(), ".git" | ".rosen") {
        return Err(format!(
            "Refused to read `{raw}`: internal repository metadata cannot be read"
        ));
    }
    Ok(parts.join("/"))
}

fn has_windows_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Returns true when `relative_path` matches `.env` or standard secret/key filenames.
pub fn is_baseline_secret(relative_path: &str) -> bool {
    for part in relative_path.split('/') {
        let lower = part.to_ascii_lowercase();
        if lower == ".env" || lower.starts_with(".env.") || lower == ".envrc" {
            return true;
        }
        if matches!(
            lower.as_str(),
            "id_rsa"
                | "id_ed25519"
                | "id_ecdsa"
                | "id_dsa"
                | ".npmrc"
                | ".netrc"
                | "_netrc"
                | ".pypirc"
        ) {
            return true;
        }
        if [".pem", ".pfx", ".p12", ".key", ".p8", ".jks", ".keystore"]
            .iter()
            .any(|suffix| lower.ends_with(suffix))
        {
            return true;
        }
    }
    false
}

/// Returns true when `relative_path` is a `.env`/secret file or matches Rosen's `ignore` list.
pub fn is_refused_path(relative_path: &str, rosen_ignore: &[String]) -> bool {
    if is_baseline_secret(relative_path) {
        return true;
    }
    if rosen_ignore.is_empty() {
        return false;
    }
    let mut builder = GitignoreBuilder::new("");
    for pattern in rosen_ignore {
        let _ = builder.add_line(None, pattern);
    }
    let Ok(matcher) = builder.build() else {
        return false;
    };
    matcher
        .matched_path_or_any_parents(relative_path, false)
        .is_ignore()
}

/// Reads `rosen.toml` from a checkout folder and returns its `ignore` patterns, if any.
pub fn rosen_ignore_from_checkout(root: &Path) -> Vec<String> {
    let Ok(source) = fs::read_to_string(root.join(CONFIG_PATH)) else {
        return Vec::new();
    };
    ProjectConfig::parse(&source)
        .map(|config| config.ignore)
        .unwrap_or_default()
}

/// Implements `project_read { project, path, offset?, limit? }` for one registered project.
pub fn read_project_file(
    app_data: &Path,
    project: &RegisteredProject,
    arguments: &Value,
) -> Result<String, String> {
    let raw_path = arguments["path"]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "`path` is required".to_owned())?;
    let relative = normalize_relative_path(raw_path)?;
    if is_baseline_secret(&relative) {
        return Err(format!(
            "Refused to read `{relative}`: secret and .env files cannot be read"
        ));
    }
    let offset = parse_optional_usize(arguments, "offset", 1)?.max(1);
    let limit =
        parse_optional_usize(arguments, "limit", DEFAULT_READ_LIMIT)?.clamp(1, MAX_READ_LIMIT);
    let bytes = load_base_bytes(app_data, project, raw_path, &relative)?;
    format_file_lines(&project.name, &relative, &bytes, offset, limit)
}

fn parse_optional_usize(arguments: &Value, key: &str, default: usize) -> Result<usize, String> {
    match arguments.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(value) => value
            .as_u64()
            .and_then(|number| usize::try_from(number).ok())
            .ok_or_else(|| format!("`{key}` must be a non-negative integer")),
    }
}

fn load_base_bytes(
    app_data: &Path,
    project: &RegisteredProject,
    raw_path: &str,
    relative: &str,
) -> Result<Vec<u8>, String> {
    if project.is_rosen {
        let endpoint = velocity_rosen::ProjectServer::new(app_data, project.id)
            .endpoint()
            .clone();
        match crate::rosen::read_base_file(&endpoint, relative) {
            Ok(RosenBaseRead::File(bytes)) => return Ok(bytes),
            Ok(RosenBaseRead::Directory) => {
                return Err(format!("`{relative}` is a directory, not a file"));
            }
            Ok(RosenBaseRead::NotFound) => {
                return Err(format!(
                    "File `{relative}` not found in project `{}` base",
                    project.name
                ));
            }
            Ok(RosenBaseRead::Refused) => {
                return Err(format!(
                    "Refused to read `{relative}`: path is ignored or marked secret by Rosen"
                ));
            }
            Err(RosenBaseError::NotRunning) => {
                if project.repository_path.is_dir() {
                    return read_checkout_file(&project.repository_path, raw_path, relative);
                }
                return Err(format!(
                    "Project `{}` Rosen server is not running and no local base checkout exists",
                    project.name
                ));
            }
            Err(RosenBaseError::Other(message)) => return Err(message),
        }
    }
    read_checkout_file(&project.repository_path, raw_path, relative)
}

/// Reads `relative` from a base checkout folder on disk, enforcing symlink confinement
/// and Rosen `ignore` / `.env` refusal on both the requested and canonical paths.
pub fn read_checkout_file(root: &Path, raw_path: &str, relative: &str) -> Result<Vec<u8>, String> {
    let rosen_ignore = rosen_ignore_from_checkout(root);
    if is_refused_path(relative, &rosen_ignore) {
        return Err(format!(
            "Refused to read `{relative}`: path is ignored or marked secret by Rosen"
        ));
    }
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| format!("Project checkout is unavailable: {error}"))?;
    let candidate = root.join(relative);
    let canonical_file = fs::canonicalize(&candidate).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!("File `{relative}` not found")
        } else {
            format!("Could not open `{relative}`: {error}")
        }
    })?;
    let canonical_relative = canonical_file
        .strip_prefix(&canonical_root)
        .map_err(|_| format!("path `{raw_path}` resolves outside the project checkout"))?;
    let canonical_relative = canonical_relative.to_string_lossy().replace('\\', "/");
    let canonical_normalized = normalize_relative_path(&canonical_relative)?;
    if is_refused_path(&canonical_normalized, &rosen_ignore) {
        return Err(format!(
            "Refused to read `{relative}`: path is ignored or marked secret by Rosen"
        ));
    }
    let metadata = fs::metadata(&canonical_file)
        .map_err(|error| format!("Could not read metadata for `{relative}`: {error}"))?;
    if !metadata.is_file() {
        return Err(format!("`{relative}` is a directory, not a file"));
    }
    if metadata.len() > MAX_READ_BYTES {
        return Err(format!(
            "`{relative}` is too large ({} bytes; max {MAX_READ_BYTES} bytes)",
            metadata.len()
        ));
    }
    fs::read(&canonical_file).map_err(|error| format!("Could not read `{relative}`: {error}"))
}

fn format_file_lines(
    project: &str,
    relative: &str,
    bytes: &[u8],
    offset: usize,
    limit: usize,
) -> Result<String, String> {
    if bytes.iter().take(8000).any(|byte| *byte == 0) {
        return Err(format!("`{relative}` is a binary file"));
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| format!("`{relative}` is not valid UTF-8 text"))?;
    let all_lines: Vec<&str> = text.lines().collect();
    if all_lines.is_empty() {
        return Ok(format!("--- {project}:{relative} (empty file)\n"));
    }
    let total = all_lines.len();
    if offset > total {
        return Ok(format!(
            "--- {project}:{relative} has {total} lines (offset {offset} is past end of file)\n"
        ));
    }
    let mut body = String::new();
    let mut shown = 0usize;
    let max_body = BUDGET.saturating_sub(240);
    for (index, line) in all_lines.iter().enumerate().skip(offset - 1).take(limit) {
        let formatted = format!("{:>5}| {line}\n", index + 1);
        if shown > 0 && body.len() + formatted.len() > max_body {
            break;
        }
        body.push_str(&formatted);
        shown += 1;
    }
    let end = offset + shown - 1;
    let mut out = format!("--- {project}:{relative}:{offset}-{end} (of {total} lines)\n");
    out.push_str(&body);
    if end < total {
        let _ = writeln!(
            out,
            "… {} more lines; pass offset={} to continue",
            total - end,
            end + 1
        );
    }
    Ok(out)
}

/// Resolves a Git repository's base revision name (`origin/<branch>`, `<branch>`, or `HEAD`).
pub(crate) fn resolve_git_base_revision(repository: &Path, branch: Option<&str>) -> Option<String> {
    let branch = branch
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("master");
    let candidates = [
        format!("refs/remotes/origin/{branch}"),
        format!("refs/heads/{branch}"),
        String::from("HEAD"),
    ];
    candidates
        .into_iter()
        .find(|candidate| crate::git_tree::GitTree::open(repository, candidate).is_ok())
}

/// Runs `graph_find` or `graph_read` against a registered project's base in-process
/// (used by local backends and unit tests).
pub(crate) fn query_local_base(
    app_data: &Path,
    project: &RegisteredProject,
    name: &str,
    arguments: &Value,
) -> Result<(String, ToolOutcome), CallError> {
    let cache = crate::usage::folder(app_data, &project.id.to_string());
    let index = open_base_index(app_data, project, &cache).map_err(|error| error.to_string())?;
    let mut tools = Tools::new(index);
    if name == "graph_read" {
        refuse_secret_graph_read(project, &tools, arguments)?;
    }
    tools.measured_call(name, arguments)
}

fn open_base_index(
    app_data: &Path,
    project: &RegisteredProject,
    cache: &Path,
) -> Result<Index, Error> {
    if project.is_rosen {
        let endpoint = velocity_rosen::ProjectServer::new(app_data, project.id)
            .endpoint()
            .clone();
        match Index::open_revision(endpoint, "base", cache) {
            Ok(index) => return Ok(index),
            Err(Error::Rosen(rosen_client::Error::NotRunning(_)))
                if project.repository_path.is_dir() => {}
            Err(error) => return Err(error),
        }
    }
    if let Some(revision) =
        resolve_git_base_revision(&project.repository_path, Some(&project.default_branch))
    {
        return Index::open_git(&project.repository_path, &revision, cache);
    }
    Index::open_snapshot(&project.repository_path, cache, |_, _| {})
}

/// Ensures `graph_read` on a sibling base view cannot return lines from a secret or `.env` file.
pub(crate) fn refuse_secret_graph_read(
    project: &RegisteredProject,
    tools: &Tools,
    arguments: &Value,
) -> Result<(), CallError> {
    let Some(target) = arguments.get("target").and_then(Value::as_str) else {
        return Ok(());
    };
    let ignore = if project.repository_path.is_dir() {
        rosen_ignore_from_checkout(&project.repository_path)
    } else {
        Vec::new()
    };
    let trimmed = target.trim().trim_matches('`').replace('\\', "/");
    if is_refused_path(trimmed.trim_start_matches("./"), &ignore) {
        return Err(format!(
            "Refused to read `{target}`: path is ignored or marked secret by Rosen"
        )
        .into());
    }
    if let Some(record) = tools.index().graph.by_address.get(trimmed.as_str()) {
        let path = &tools.index().graph.records[*record].path;
        if is_refused_path(path, &ignore) {
            return Err(format!(
                "Refused to read `{path}`: path is ignored or marked secret by Rosen"
            )
            .into());
        }
    }
    Ok(())
}

/// Refuses `graph_read` on secret or Rosen-ignored paths in `project_service` base requests.
pub(crate) fn refuse_secret_base_read(
    workspace: &Path,
    tools: &Tools,
    arguments: &Value,
) -> Result<(), String> {
    let Some(target) = arguments.get("target").and_then(Value::as_str) else {
        return Ok(());
    };
    let ignore = if workspace.is_dir() {
        rosen_ignore_from_checkout(workspace)
    } else {
        Vec::new()
    };
    let trimmed = target.trim().trim_matches('`').replace('\\', "/");
    if is_refused_path(trimmed.trim_start_matches("./"), &ignore) {
        return Err(format!(
            "Refused to read `{target}`: path is ignored or marked secret by Rosen"
        ));
    }
    if let Some(record) = tools.index().graph.by_address.get(trimmed.as_str()) {
        let path = &tools.index().graph.records[*record].path;
        if is_refused_path(path, &ignore) {
            return Err(format!(
                "Refused to read `{path}`: path is ignored or marked secret by Rosen"
            ));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn extended_windows_path(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    if text.starts_with(r"\\?\") || text.starts_with(r"\\.\") {
        return path.to_path_buf();
    }
    if path.is_absolute() && text.len() >= 240 {
        if let Some(unc) = text.strip_prefix(r"\\") {
            return PathBuf::from(format!(r"\\?\UNC\{unc}"));
        }
        return PathBuf::from(format!(r"\\?\{text}"));
    }
    path.to_path_buf()
}

#[cfg(not(windows))]
fn extended_windows_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn normalize_refuses_traversal_absolute_and_vcs_metadata() {
        assert_eq!(
            normalize_relative_path("spec/C14-issues.md")
                .ok()
                .as_deref(),
            Some("spec/C14-issues.md")
        );
        assert_eq!(
            normalize_relative_path(r".\server\issues.go")
                .ok()
                .as_deref(),
            Some("server/issues.go")
        );
        for bad in [
            "",
            ".",
            "../secret.txt",
            "spec/../../outside.txt",
            "/etc/passwd",
            r"\Windows\System32\drivers\etc\hosts",
            r"C:\secret.txt",
            "C:secret.txt",
            ".git/config",
            ".rosen/server/metadata.sqlite",
            "a\0b",
        ] {
            assert!(
                normalize_relative_path(bad).is_err(),
                "expected `{bad}` to be refused"
            );
        }
    }

    #[test]
    fn secret_and_rosen_ignore_patterns_are_refused() {
        let rosen_ignore = vec![
            ".env".into(),
            ".env.*".into(),
            "*.pem".into(),
            "secrets/".into(),
            "config/private.toml".into(),
        ];
        for secret in [
            ".env",
            ".env.local",
            "server/.env.production",
            "certs/server.pem",
            "tls/app.KEY",
            "secrets/api-token.txt",
            "config/private.toml",
            ".npmrc",
        ] {
            assert!(
                is_refused_path(secret, &rosen_ignore),
                "expected `{secret}` to be refused"
            );
        }
        for allowed in [
            "spec/C14-issues.md",
            "server/routes/issues.go",
            "rosen.toml",
            "config/public.toml",
        ] {
            assert!(
                !is_refused_path(allowed, &rosen_ignore),
                "expected `{allowed}` to be allowed"
            );
        }
    }

    #[test]
    fn read_checkout_file_paginates_and_blocks_secrets() -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "velocity-project-read-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("spec"))?;
        fs::create_dir_all(root.join("secrets"))?;
        fs::write(
            root.join("rosen.toml"),
            "ignore = [\".env\", \"secrets/\"]\n",
        )?;
        fs::write(
            root.join("spec/C14-issues.md"),
            "line 1\nline 2\nline 3\nline 4\nline 5\n",
        )?;
        fs::write(root.join(".env"), "SECRET=1\n")?;
        fs::write(root.join("secrets/token.txt"), "tok_123\n")?;

        let project = RegisteredProject {
            id: 2,
            name: "collapse.sh".into(),
            repository_path: root.clone(),
            default_branch: "main".into(),
            github_remote: Some("mESmaC/collapse.sh".into()),
            collapse_repository: None,
            is_rosen: false,
        };
        let app_data = root.join("app_data");
        let page = read_project_file(
            &app_data,
            &project,
            &json!({"path": "spec/C14-issues.md", "offset": 2, "limit": 2}),
        )?;
        assert!(page.contains("--- collapse.sh:spec/C14-issues.md:2-3 (of 5 lines)"));
        assert!(page.contains("    2| line 2"));
        assert!(page.contains("    3| line 3"));
        assert!(page.contains("pass offset=4 to continue"));

        assert!(
            read_project_file(&app_data, &project, &json!({"path": ".env"})).is_err(),
            ".env must be refused"
        );
        assert!(
            read_project_file(&app_data, &project, &json!({"path": "secrets/token.txt"})).is_err(),
            "rosen.toml ignored path must be refused"
        );
        assert!(
            read_project_file(&app_data, &project, &json!({"path": "../outside.txt"})).is_err(),
            "path traversal must be refused"
        );

        let _ = fs::remove_dir_all(&root);
        Ok(())
    }

    #[test]
    fn list_and_resolve_projects_from_split_storage() -> Result<(), Box<dyn std::error::Error>> {
        let dir = std::env::temp_dir().join(format!(
            "velocity-projects-split-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("projects"))?;
        fs::write(dir.join("state.json"), r#"{"project_ids":[1,2]}"#)?;
        fs::write(
            dir.join("projects/1.json"),
            r#"{"project":{"id":1,"name":"velocity","repository_path":"Z:/velocity","default_branch":"master","github_remote":"mESmaC/velocity","version_control":"rosen"}}"#,
        )?;
        fs::write(
            dir.join("projects/2.json"),
            r#"{"project":{"id":2,"name":"collapse.sh","repository_path":"Z:/collapse.sh","default_branch":"main","github_remote":"mESmaC/collapse.sh","version_control":"rosen"}}"#,
        )?;

        let projects = list_projects(&dir);
        assert_eq!(projects.len(), 2);
        assert_eq!(format_readable_projects(&projects), "collapse.sh, velocity");
        assert_eq!(resolve_project(&projects, "collapse.sh")?.id, 2);
        assert_eq!(resolve_project(&projects, "mESmaC/collapse.sh")?.id, 2);
        assert_eq!(resolve_project(&projects, "2")?.id, 2);
        assert!(resolve_project(&projects, "unknown").is_err());

        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }
}
