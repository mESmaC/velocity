use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::gh_command::GitHubCommand;
use crate::http_transport::TransportAttempt;
use crate::{GitHubCommandError, RepositoryName};

/// How long a repository's resolved identity (`nameWithOwner`, `defaultBranch`) is kept in
/// the session-wide cache (#556). Effectively once per repository per session.
const IDENTITY_LIFETIME: Duration = Duration::from_secs(6 * 60 * 60);

#[derive(Clone)]
struct CachedIdentity {
    repository: String,
    owner: String,
    name: String,
    default_branch: Option<String>,
    fetched_at: Instant,
}

static IDENTITY_CACHE: LazyLock<Mutex<HashMap<String, CachedIdentity>>> =
    LazyLock::new(Mutex::default);
static IDENTITY_LOCKS: LazyLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> =
    LazyLock::new(Mutex::default);

/// One GitHub repository's identity and its transport, shared by the issue reads and
/// writes (F9) and the Rosen issue and status mirrors.
pub struct RepositoryClient {
    pub(crate) command: GitHubCommand,
    pub(crate) repository: String,
    pub(crate) default_branch: Option<String>,
    owner: String,
    name: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RepositoryIdentity {
    name_with_owner: String,
    default_branch_ref: Option<DefaultBranch>,
}

#[derive(Deserialize)]
struct DefaultBranch {
    name: String,
}

#[derive(Deserialize)]
struct RestRepositoryIdentity {
    full_name: String,
    default_branch: Option<String>,
}

#[derive(Deserialize)]
pub(crate) struct Connection<T> {
    nodes: Vec<T>,
    #[serde(rename = "pageInfo")]
    page: PageInfo,
}

impl<T> Connection<T> {
    /// This page's nodes, without reading further pages.
    pub(crate) fn into_nodes(self) -> Vec<T> {
        self.nodes
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

impl RepositoryClient {
    /// Resolves `directory`'s repository identity, reusing the session cache so `repo view`
    /// runs at most once per repository (#556).
    #[track_caller]
    pub fn discover(directory: &Path) -> Result<Self, GitHubCommandError> {
        let (command, repository) = GitHubCommand::new(directory)?;
        let parsed = parse_github_origin(&repository);
        Self::resolve_identity(command, &repository, parsed.as_ref())
    }

    /// Resolves `repository`'s identity, reusing the session cache so repeated client
    /// construction across dispatch, project health and UI never repeats `repo view` (#556).
    #[track_caller]
    pub fn for_repository(repository: &str) -> Result<Self, GitHubCommandError> {
        let parsed = RepositoryName::parse(repository)
            .map_err(|error| GitHubCommandError::Invalid(error.to_string()))?;
        let command = GitHubCommand::for_repository(&parsed);
        let full = parsed.full_name();
        Self::resolve_identity(command, &full, Some(&parsed))
    }

    fn resolve_identity(
        command: GitHubCommand,
        raw_repository: &str,
        parsed: Option<&RepositoryName>,
    ) -> Result<Self, GitHubCommandError> {
        let cache_key = parsed
            .map(|name| name.full_name().to_ascii_lowercase())
            .unwrap_or_else(|| raw_repository.to_ascii_lowercase());
        if let Some(cached) = cached_identity(&cache_key) {
            return Ok(Self::from_cached(command, cached));
        }
        let lock = identity_lock(&cache_key);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(cached) = cached_identity(&cache_key) {
            return Ok(Self::from_cached(command, cached));
        }
        let identity = Self::fetch_identity(&command, raw_repository, parsed)?;
        store_identity(&cache_key, &identity);
        Ok(Self::from_cached(command, identity))
    }

    fn fetch_identity(
        command: &GitHubCommand,
        raw_repository: &str,
        parsed: Option<&RepositoryName>,
    ) -> Result<CachedIdentity, GitHubCommandError> {
        if let Some(parsed) = parsed
            && command.is_github_com()
        {
            let endpoint = format!("repos/{}", parsed.full_name());
            if let TransportAttempt::Handled(result) =
                command.try_http_get::<RestRepositoryIdentity>(&endpoint)
            {
                let rest = result?;
                return build_identity(rest.full_name, rest.default_branch);
            }
        }
        let identity: RepositoryIdentity = command.read(&[
            "repo",
            "view",
            "--json",
            "nameWithOwner,defaultBranchRef",
            "--",
            raw_repository,
        ])?;
        build_identity(
            identity.name_with_owner,
            identity.default_branch_ref.map(|branch| branch.name),
        )
    }

    fn from_cached(command: GitHubCommand, identity: CachedIdentity) -> Self {
        Self {
            owner: identity.owner,
            name: identity.name,
            default_branch: identity.default_branch,
            repository: identity.repository,
            command,
        }
    }

    /// Updates the cached default branch when a combined GraphQL query returns it.
    pub(crate) fn update_default_branch(repository: &str, branch: &str) {
        let mut cache = IDENTITY_CACHE
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(entry) = cache.get_mut(&repository.to_ascii_lowercase()) {
            entry.default_branch = Some(branch.to_owned());
        }
    }

    /// Whether this client still describes `directory`'s repository. Callers that keep a client
    /// between reads check this before each use: a changed `origin` is seen at once, from the
    /// local Git config, and a renamed default branch, which only GitHub knows, within
    /// `IDENTITY_LIFETIME` (#229).
    pub fn is_current(&self, directory: &Path) -> bool {
        self.command.is_current(directory, IDENTITY_LIFETIME)
    }

    pub(crate) fn owner(&self) -> &str {
        &self.owner
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn variables(&self, number: u32) -> Value {
        self.variables_for(&self.owner, &self.name, number)
    }

    pub(crate) fn variables_for(&self, owner: &str, name: &str, number: u32) -> Value {
        json!({"owner": owner, "name": name, "number": number})
    }

    pub(crate) fn graphql_at<T: DeserializeOwned>(
        &self,
        query: &str,
        variables: Value,
        path: &str,
    ) -> Result<T, GitHubCommandError> {
        let response: Value = self
            .command
            .graphql(path, &json!({"query":query,"variables":variables}))?;
        if let Some(errors) = response.get("errors") {
            return Err(GitHubCommandError::Invalid(format!(
                "GitHub GraphQL errors: {errors}"
            )));
        }
        let value = response
            .pointer(path)
            .filter(|value| !value.is_null())
            .ok_or_else(|| GitHubCommandError::Missing(path.into()))?;
        Ok(serde_json::from_value(value.clone())?)
    }

    pub(crate) fn connection<T: DeserializeOwned>(
        &self,
        query: &str,
        variables: Value,
        path: &str,
    ) -> Result<Vec<T>, GitHubCommandError> {
        let first_page = self.graphql_at(query, variables.clone(), path)?;
        self.complete_connection(query, variables, path, first_page)
    }

    pub(crate) fn complete_connection<T: DeserializeOwned>(
        &self,
        query: &str,
        mut variables: Value,
        path: &str,
        mut page: Connection<T>,
    ) -> Result<Vec<T>, GitHubCommandError> {
        let mut nodes = Vec::new();
        loop {
            nodes.extend(page.nodes);
            if !page.page.has_next_page {
                return Ok(nodes);
            }
            let cursor = page
                .page
                .end_cursor
                .filter(|cursor| !cursor.is_empty())
                .ok_or_else(|| GitHubCommandError::Missing("pagination cursor".into()))?;
            if variables.get("cursor").and_then(Value::as_str) == Some(&cursor) {
                return Err(GitHubCommandError::Invalid(
                    "GitHub repeated a pagination cursor".into(),
                ));
            }
            variables["cursor"] = json!(cursor);
            page = self.graphql_at(query, variables.clone(), path)?;
        }
    }
}

fn build_identity(
    name_with_owner: String,
    default_branch: Option<String>,
) -> Result<CachedIdentity, GitHubCommandError> {
    let (owner, name) = name_with_owner
        .split_once('/')
        .ok_or_else(|| GitHubCommandError::Invalid("Invalid GitHub repository name".into()))?;
    if [owner, name].iter().any(|part| {
        part.is_empty()
            || !part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    }) {
        return Err(GitHubCommandError::Invalid(
            "Invalid GitHub repository name".into(),
        ));
    }
    Ok(CachedIdentity {
        owner: owner.into(),
        name: name.into(),
        default_branch,
        repository: name_with_owner,
        fetched_at: Instant::now(),
    })
}

fn parse_github_origin(origin: &str) -> Option<RepositoryName> {
    let trimmed = origin.trim();
    let rest = trimmed
        .strip_prefix("https://github.com/")
        .or_else(|| trimmed.strip_prefix("http://github.com/"))
        .or_else(|| trimmed.strip_prefix("ssh://git@github.com/"))
        .or_else(|| trimmed.strip_prefix("ssh://github.com/"))
        .or_else(|| trimmed.strip_prefix("git@github.com:"))?;
    RepositoryName::parse(rest).ok()
}

fn cached_identity(key: &str) -> Option<CachedIdentity> {
    let cache = IDENTITY_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .get(key)
        .filter(|entry| entry.fetched_at.elapsed() < IDENTITY_LIFETIME)
        .cloned()
}

fn store_identity(request_key: &str, identity: &CachedIdentity) {
    let mut cache = IDENTITY_CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.insert(request_key.to_owned(), identity.clone());
    let canonical = identity.repository.to_ascii_lowercase();
    if canonical != request_key {
        cache.insert(canonical, identity.clone());
    }
}

fn identity_lock(key: &str) -> Arc<Mutex<()>> {
    let mut locks = IDENTITY_LOCKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(locks.entry(key.to_owned()).or_default())
}
