//! A Collapse repository for a Rosen project's mirror (Collapse C1, C2, C3; #427).
//!
//! The provisioning token creates the repository in the account's namespace and mints its
//! push token. The personal token polls the repository until setup finishes. The push
//! token goes straight to the machine's git credential helper, which Rosen's mirror push
//! uses, so Velocity keeps no copy of it.

use std::time::Duration;

use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use serde::{Deserialize, Serialize};

use crate::CollapseError;
use crate::credential::CollapseCredentials;
use crate::url_text::encode;

const API: &str = "https://api.collapse.sh";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Setup of an empty repository takes moments; a stuck one is reported, not waited on.
const READY_TIMEOUT: Duration = Duration::from_secs(90);
const READY_POLL: Duration = Duration::from_secs(1);

/// The Collapse repository a project mirrors to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvisionedMirror {
    pub owner: String,
    pub name: String,
    /// The HTTPS git remote Rosen pushes to: `https://api.collapse.sh/<owner>/<name>.git`.
    pub mirror_url: String,
    /// Velocity created the repository; `false` reused one the account already had.
    pub created: bool,
}

impl ProvisionedMirror {
    /// `owner/name`, as Open on Collapse and the mirror notices name it.
    pub fn repository(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }

    /// The root `rosen.toml`'s `collapse` key names: `https://api.collapse.sh/<owner>/<name>`.
    /// Rosen sends its state feed and ledger entries, and reads relayed actions, under it.
    pub fn collapse_root(&self) -> String {
        format!("{API}/{}/{}", self.owner, self.name)
    }
}

/// The endpoints under a Collapse root that Rosen asks git's credential helper about: the
/// state feed, whose token the action relay shares, and the ledger push (Rosen R9, R13, R14).
fn feed_urls(root: &str) -> Vec<String> {
    let root = root.trim_end_matches('/');
    vec![format!("{root}/rosen/state"), format!("{root}/ledgers")]
}

/// Reports whether git's credential helper holds push tokens for Rosen's state feed
/// (`<root>/rosen/state`) and ledger push (`<root>/ledgers`) endpoints (#576).
pub async fn check_push_token(root: &str) -> Result<(bool, bool), CollapseError> {
    let urls = feed_urls(root);
    tokio::task::spawn_blocking(move || {
        let state_feed = crate::git_credential::has_push_secret(&urls[0])?;
        let ledger_push = crate::git_credential::has_push_secret(&urls[1])?;
        Ok((state_feed, ledger_push))
    })
    .await
    .map_err(|error| CollapseError::Git(error.to_string()))?
}

/// Gives Rosen's state feed, ledger push and relay under `root` the push token git's
/// credential helper already holds for `remote`, the project's mirror (#517). `false` when
/// the helper holds none, so the caller mints one with [`provision_mirror`].
pub async fn share_push_token(remote: &str, root: &str) -> Result<bool, CollapseError> {
    let remote = remote.to_owned();
    let urls = feed_urls(root);
    tokio::task::spawn_blocking(move || crate::git_credential::share_push_secret(&remote, &urls))
        .await
        .map_err(|error| CollapseError::Git(error.to_string()))?
}

/// What to provision: `owner` defaults to the connected account. Only the account's own
/// namespace can be created from the desktop; a repository elsewhere must already exist.
pub struct MirrorRequest<'request> {
    pub owner: Option<&'request str>,
    pub name: &'request str,
    /// The branch Rosen's base mirrors to, such as `main`.
    pub default_branch: &'request str,
    pub description: &'request str,
}

#[derive(Serialize)]
struct CreateRepository<'request> {
    owner: &'request str,
    name: &'request str,
    description: &'request str,
    visibility: &'static str,
    default_branch: &'request str,
}

#[derive(Deserialize)]
struct RepositoryReply {
    owner: String,
    name: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    mirror_url: String,
}

// Deliberately no `Debug`: the secret must never reach a log.
#[derive(Deserialize)]
struct MintedToken {
    secret: String,
}

/// Checks whether `owner/name` already exists on Collapse and is visible to `credentials`
/// without creating it (#592). Returns `None` on HTTP 404.
pub async fn repository_exists(
    credentials: &CollapseCredentials,
    owner: &str,
    name: &str,
) -> Result<Option<ProvisionedMirror>, CollapseError> {
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(failed)?;
    let path = format!("/api/repositories/{}/{}", encode(owner), encode(name));
    let response = authorized(&client, Method::GET, &path, &credentials.mcp_token)
        .send()
        .await
        .map_err(failed)?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let reply: RepositoryReply = success(response, "read the repository")
        .await?
        .json()
        .await
        .map_err(failed)?;
    Ok(Some(mirror_from(reply, false)))
}

/// Ensures `request` has a ready Collapse repository with a fresh push token stored in git's
/// credential helper (#592). When the repository already exists on Collapse (for example an
/// older import under a personal or organization namespace), this links and prepares it
/// without attempting to create a new repository.
pub async fn ensure_mirror(
    credentials: &CollapseCredentials,
    request: &MirrorRequest<'_>,
) -> Result<ProvisionedMirror, CollapseError> {
    let owner = request.owner.unwrap_or(&credentials.account);
    if let Some(existing) = repository_exists(credentials, owner, request.name).await? {
        return prepare_existing_mirror(credentials, &existing.owner, &existing.name).await;
    }
    provision_mirror(credentials, request).await
}

/// Creates the repository when it is missing, waits until Collapse has set it up, mints a
/// push token and stores it in the git credential helper under the mirror URL and the
/// endpoints Rosen's state feed, ledger push and relay authenticate at.
pub async fn provision_mirror(
    credentials: &CollapseCredentials,
    request: &MirrorRequest<'_>,
) -> Result<ProvisionedMirror, CollapseError> {
    // Check Git supports http.proactiveAuth before creating a repository on Collapse (#571).
    tokio::task::spawn_blocking(crate::git_credential::check_git_version)
        .await
        .map_err(|error| CollapseError::Git(error.to_string()))??;
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(failed)?;
    let owner = request.owner.unwrap_or(&credentials.account);
    let mut mirror = if owner.eq_ignore_ascii_case(&credentials.account) {
        create_repository(&client, credentials, owner, request).await?
    } else {
        existing(owner, request.name)
    };
    wait_until_ready(&client, credentials, &mut mirror).await?;
    let secret = mint_push_token(&client, credentials, &mirror).await?;
    let mut urls = vec![mirror.mirror_url.clone()];
    urls.extend(feed_urls(&mirror.collapse_root()));
    tokio::task::spawn_blocking(move || crate::git_credential::store_push_secret(&urls, &secret))
        .await
        .map_err(|error| CollapseError::Git(error.to_string()))??;
    Ok(mirror)
}

/// Verifies an existing Collapse repository is ready, mints a push token for it and stores
/// the token in git's credential helper with proactive HTTP auth enabled (#575). Unlike
/// [`provision_mirror`], this never creates a missing repository.
pub async fn prepare_existing_mirror(
    credentials: &CollapseCredentials,
    owner: &str,
    name: &str,
) -> Result<ProvisionedMirror, CollapseError> {
    tokio::task::spawn_blocking(crate::git_credential::check_git_version)
        .await
        .map_err(|error| CollapseError::Git(error.to_string()))??;
    let client = Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(failed)?;
    let mut mirror = existing(owner, name);
    wait_until_ready(&client, credentials, &mut mirror).await?;
    let secret = mint_push_token(&client, credentials, &mirror).await?;
    let mut urls = vec![mirror.mirror_url.clone()];
    urls.extend(feed_urls(&mirror.collapse_root()));
    tokio::task::spawn_blocking(move || crate::git_credential::store_push_secret(&urls, &secret))
        .await
        .map_err(|error| CollapseError::Git(error.to_string()))??;
    Ok(mirror)
}

/// A repository Velocity did not create; its exact name comes from the ready check.
fn existing(owner: &str, name: &str) -> ProvisionedMirror {
    ProvisionedMirror {
        owner: owner.to_owned(),
        name: name.to_owned(),
        mirror_url: format!("{API}/{owner}/{name}.git"),
        created: false,
    }
}

async fn create_repository(
    client: &Client,
    credentials: &CollapseCredentials,
    owner: &str,
    request: &MirrorRequest<'_>,
) -> Result<ProvisionedMirror, CollapseError> {
    let body = CreateRepository {
        owner,
        name: request.name,
        description: request.description,
        visibility: "private",
        default_branch: request.default_branch,
    };
    let response = authorized(
        client,
        Method::POST,
        "/api/repositories",
        &credentials.access_token,
    )
    .json(&body)
    .send()
    .await
    .map_err(failed)?;
    // A repository the account already owns under this name is reused: the push token
    // mint below proves ownership, and Rosen refuses to rewrite unrelated history.
    if response.status() == StatusCode::CONFLICT {
        return Ok(existing(owner, request.name));
    }
    let reply: RepositoryReply = success(response, "create the repository")
        .await?
        .json()
        .await
        .map_err(failed)?;
    Ok(mirror_from(reply, true))
}

fn mirror_from(reply: RepositoryReply, created: bool) -> ProvisionedMirror {
    let mirror_url = if reply.mirror_url.starts_with(&format!("{API}/")) {
        reply.mirror_url
    } else {
        // Only Collapse's own git host is trusted as a mirror remote.
        format!("{API}/{}/{}.git", reply.owner, reply.name)
    };
    ProvisionedMirror {
        owner: reply.owner,
        name: reply.name,
        mirror_url,
        created,
    }
}

async fn wait_until_ready(
    client: &Client,
    credentials: &CollapseCredentials,
    mirror: &mut ProvisionedMirror,
) -> Result<(), CollapseError> {
    let path = format!(
        "/api/repositories/{}/{}",
        encode(&mirror.owner),
        encode(&mirror.name)
    );
    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
    loop {
        let response = authorized(client, Method::GET, &path, &credentials.mcp_token)
            .send()
            .await
            .map_err(failed)?;
        if response.status() == StatusCode::NOT_FOUND {
            return Err(CollapseError::Api(format!(
                "{}/{} was not found on Collapse, or your account cannot see it",
                mirror.owner, mirror.name
            )));
        }
        let reply: RepositoryReply = success(response, "read the repository")
            .await?
            .json()
            .await
            .map_err(failed)?;
        match reply.state.as_str() {
            "ready" => {
                let created = mirror.created;
                *mirror = mirror_from(reply, created);
                return Ok(());
            }
            "failed" => {
                return Err(CollapseError::Api(format!(
                    "Collapse could not set up {}/{}; retry it from the repository page",
                    mirror.owner, mirror.name
                )));
            }
            _ if tokio::time::Instant::now() >= deadline => {
                return Err(CollapseError::Api(format!(
                    "{}/{} is still being set up on Collapse; try again shortly",
                    mirror.owner, mirror.name
                )));
            }
            _ => tokio::time::sleep(READY_POLL).await,
        }
    }
}

async fn mint_push_token(
    client: &Client,
    credentials: &CollapseCredentials,
    mirror: &ProvisionedMirror,
) -> Result<String, CollapseError> {
    let path = format!(
        "/api/tokens/repositories/{}/{}",
        encode(&mirror.owner),
        encode(&mirror.name)
    );
    let response = authorized(client, Method::POST, &path, &credentials.access_token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .map_err(failed)?;
    if response.status() == StatusCode::NOT_FOUND {
        return Err(CollapseError::Api(format!(
            "{}/{} is not a Collapse repository your account manages",
            mirror.owner, mirror.name
        )));
    }
    let minted: MintedToken = success(response, "create a push token")
        .await?
        .json()
        .await
        .map_err(failed)?;
    if minted.secret.is_empty() {
        return Err(CollapseError::Api(
            "Collapse returned an empty push token".into(),
        ));
    }
    Ok(minted.secret)
}

/// A native request: no `Origin` or cookie, as Collapse requires of desktop credentials.
pub(crate) fn authorized(
    client: &Client,
    method: Method,
    path: &str,
    token: &str,
) -> RequestBuilder {
    client
        .request(method, format!("{API}{path}"))
        .bearer_auth(token)
        .header(reqwest::header::ACCEPT, "application/json")
}

pub(crate) async fn success(response: Response, action: &str) -> Result<Response, CollapseError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(CollapseError::Api(format!(
            "Collapse refused to {action} ({status}); disconnect and connect Collapse again in Settings"
        )));
    }
    let explanation = response.text().await.unwrap_or_default();
    let explanation: String = explanation.trim().chars().take(200).collect();
    Err(CollapseError::Api(format!(
        "Could not {action} on Collapse: {status} {explanation}"
    )))
}

pub(crate) fn failed(error: reqwest::Error) -> CollapseError {
    CollapseError::Api(error.without_url().to_string())
}
