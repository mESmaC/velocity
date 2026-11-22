//! A Collapse repository for a Rosen project's mirror (Collapse C1, C2, C3; #427).
//!
//! The provisioning token creates the repository in the account's namespace and mints its
//! push token. The personal token polls the repository until setup finishes. The push
//! token goes straight to the machine's git credential helper, which Rosen's mirror push
//! uses, so Velocity keeps no copy of it.

use std::time::Duration;

use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::CollapseError;
use crate::credential::CollapseCredentials;
use crate::url_text::encode;

const API: &str = "https://api.collapse.sh";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// Setup of an empty repository takes moments; a stuck one is reported, not waited on.
const READY_TIMEOUT: Duration = Duration::from_secs(90);
const READY_POLL: Duration = Duration::from_secs(1);

/// How Velocity obtained the Collapse repository for a project mirror (#610).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProvisionDisposition {
    /// Reused an existing live Collapse repository.
    #[default]
    Reused,
    /// Created a new Collapse repository.
    Created,
    /// Restored a deleted Collapse repository pending purge.
    Restored,
    /// Purged a deleted Collapse repository and created a fresh one.
    PurgedAndCreated,
}

/// The Collapse repository a project mirrors to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProvisionedMirror {
    pub owner: String,
    pub name: String,
    /// The HTTPS git remote Rosen pushes to: `https://api.collapse.sh/<owner>/<name>.git`.
    pub mirror_url: String,
    /// Velocity created the repository; `false` reused or restored an existing one.
    pub created: bool,
    /// Whether the repository was created, reused, restored, or purged and re-created (#610).
    pub disposition: ProvisionDisposition,
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

/// A Collapse repository name held by a deleted copy pending purge (#610, collapse.sh#217).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeletedPendingRepository {
    pub owner: String,
    pub name: String,
    /// RFC 3339 deadline until which Collapse holds the deleted copy, when Collapse reported it.
    pub delete_after: Option<String>,
    /// RFC 3339 deletion time, when Collapse reported it.
    pub deleted_at: Option<String>,
    /// `true` when Collapse returned a `deleted_pending` reply (collapse.sh#217);
    /// `false` when inferred from `409` on create followed by `404` on read.
    pub confirmed: bool,
    /// `true` when Collapse supports immediate purge for this repository (collapse.sh#217).
    pub purge_available: bool,
    /// Optional settings page URL returned by Collapse.
    pub settings_url: Option<String>,
}

impl DeletedPendingRepository {
    pub fn inferred(owner: &str, name: &str) -> Self {
        Self {
            owner: owner.to_owned(),
            name: name.to_owned(),
            delete_after: None,
            deleted_at: None,
            confirmed: false,
            purge_available: false,
            settings_url: None,
        }
    }

    pub fn repository(&self) -> String {
        format!("{}/{}", self.owner, self.name)
    }
}

impl std::fmt::Display for DeletedPendingRepository {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if let Some(deadline) = self.delete_after.as_deref() {
            return write!(
                formatter,
                "{}/{} was deleted on Collapse and is held until {deadline}",
                self.owner, self.name
            );
        }
        if self.confirmed {
            write!(
                formatter,
                "{}/{} was deleted on Collapse and is held until its purge deadline",
                self.owner, self.name
            )
        } else {
            write!(
                formatter,
                "{}/{} was probably deleted on Collapse (pending purge) and is held until its 7-day purge deadline",
                self.owner, self.name
            )
        }
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
/// without creating it (#592). Returns `None` on HTTP 404 unless Collapse reports a
/// deleted repository pending purge (#610).
pub async fn repository_exists(
    credentials: &CollapseCredentials,
    owner: &str,
    name: &str,
) -> Result<Option<ProvisionedMirror>, CollapseError> {
    let client = build_client()?;
    repository_exists_with_client(&client, credentials, owner, name).await
}

async fn repository_exists_with_client(
    client: &Client,
    credentials: &CollapseCredentials,
    owner: &str,
    name: &str,
) -> Result<Option<ProvisionedMirror>, CollapseError> {
    let path = format!("/api/repositories/{}/{}", encode(owner), encode(name));
    let response = authorized(client, Method::GET, &path, &credentials.mcp_token)
        .send()
        .await
        .map_err(failed)?;
    let status = response.status();
    if matches!(
        status,
        StatusCode::NOT_FOUND | StatusCode::CONFLICT | StatusCode::GONE
    ) {
        let body = response.text().await.unwrap_or_default();
        if let Some(deleted) = parse_deleted_pending(&body, owner, name) {
            return Err(CollapseError::DeletedPending(Box::new(deleted)));
        }
        if status == StatusCode::NOT_FOUND {
            return Ok(None);
        }
        return Err(error_from_status(status, &body, "read the repository"));
    }
    let reply: RepositoryReply = success(response, "read the repository")
        .await?
        .json()
        .await
        .map_err(failed)?;
    Ok(Some(mirror_from(reply, ProvisionDisposition::Reused)))
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
    check_git_version().await?;
    let client = build_client()?;
    let owner = request.owner.unwrap_or(&credentials.account);
    let allow_non_owner_fallback = !owner.eq_ignore_ascii_case(&credentials.account);
    let (mut mirror, from_create_conflict) = create_repository(
        &client,
        credentials,
        owner,
        request,
        allow_non_owner_fallback,
    )
    .await?;
    finalize_mirror(&client, credentials, &mut mirror, from_create_conflict).await?;
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
    check_git_version().await?;
    let client = build_client()?;
    let mut mirror = existing(owner, name);
    finalize_mirror(&client, credentials, &mut mirror, false).await?;
    Ok(mirror)
}

/// Restores a deleted Collapse repository pending purge, waits until it is ready, mints a
/// push token and stores it in git's credential helper (#610, collapse.sh#217).
pub async fn restore_mirror(
    credentials: &CollapseCredentials,
    owner: &str,
    name: &str,
) -> Result<ProvisionedMirror, CollapseError> {
    check_git_version().await?;
    let client = build_client()?;
    let path = format!(
        "/api/repositories/{}/{}/restore",
        encode(owner),
        encode(name)
    );
    let response = authorized(&client, Method::POST, &path, &credentials.access_token)
        .json(&serde_json::json!({}))
        .send()
        .await
        .map_err(failed)?;
    let status = response.status();
    if matches!(
        status,
        StatusCode::FORBIDDEN | StatusCode::METHOD_NOT_ALLOWED
    ) {
        return Err(CollapseError::Api(format!(
            "Collapse does not accept desktop restore for {owner}/{name} yet ({status}); restore it from https://collapse.sh/{owner}/{name}/settings and re-run"
        )));
    }
    if status == StatusCode::NOT_FOUND {
        return Err(CollapseError::Api(format!(
            "Collapse could not restore {owner}/{name} (404 Not Found — past its restoration deadline, already purged, or desktop restore is unavailable); check https://collapse.sh/{owner}/{name}/settings and re-run"
        )));
    }
    let _ = success(response, "restore the repository").await?;
    let mut mirror = ProvisionedMirror {
        owner: owner.to_owned(),
        name: name.to_owned(),
        mirror_url: format!("{API}/{owner}/{name}.git"),
        created: false,
        disposition: ProvisionDisposition::Restored,
    };
    finalize_mirror(&client, credentials, &mut mirror, false).await?;
    Ok(mirror)
}

/// Permanently purges a deleted Collapse repository pending purge, creates a fresh copy
/// under the same name, waits until it is ready, mints a push token and stores it in git's
/// credential helper (#610, collapse.sh#217).
pub async fn purge_and_create_mirror(
    credentials: &CollapseCredentials,
    request: &MirrorRequest<'_>,
) -> Result<ProvisionedMirror, CollapseError> {
    check_git_version().await?;
    let client = build_client()?;
    let owner = request.owner.unwrap_or(&credentials.account);
    let (mut mirror, from_create_conflict) =
        purge_then_create(&client, credentials, owner, request).await?;
    if mirror.created {
        mirror.disposition = ProvisionDisposition::PurgedAndCreated;
    }
    finalize_mirror(&client, credentials, &mut mirror, from_create_conflict).await?;
    Ok(mirror)
}

async fn check_git_version() -> Result<(), CollapseError> {
    // Check Git supports http.proactiveAuth before creating or mutating on Collapse (#571).
    tokio::task::spawn_blocking(crate::git_credential::check_git_version)
        .await
        .map_err(|error| CollapseError::Git(error.to_string()))??;
    Ok(())
}

fn build_client() -> Result<Client, CollapseError> {
    Client::builder()
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(failed)
}

async fn finalize_mirror(
    client: &Client,
    credentials: &CollapseCredentials,
    mirror: &mut ProvisionedMirror,
    from_create_conflict: bool,
) -> Result<(), CollapseError> {
    wait_until_ready(client, credentials, mirror, from_create_conflict).await?;
    let secret = mint_push_token(client, credentials, mirror).await?;
    let mut urls = vec![mirror.mirror_url.clone()];
    urls.extend(feed_urls(&mirror.collapse_root()));
    tokio::task::spawn_blocking(move || crate::git_credential::store_push_secret(&urls, &secret))
        .await
        .map_err(|error| CollapseError::Git(error.to_string()))?
}

/// A repository Velocity did not create; its exact name comes from the ready check.
fn existing(owner: &str, name: &str) -> ProvisionedMirror {
    ProvisionedMirror {
        owner: owner.to_owned(),
        name: name.to_owned(),
        mirror_url: format!("{API}/{owner}/{name}.git"),
        created: false,
        disposition: ProvisionDisposition::Reused,
    }
}

enum PurgeOutcome {
    Purged,
    AlreadyAbsent,
    BadConfirmation,
}

async fn purge_then_create(
    client: &Client,
    credentials: &CollapseCredentials,
    owner: &str,
    request: &MirrorRequest<'_>,
) -> Result<(ProvisionedMirror, bool), CollapseError> {
    match purge_repository(client, credentials, owner, request.name).await? {
        PurgeOutcome::Purged => create_repository(client, credentials, owner, request, false).await,
        PurgeOutcome::AlreadyAbsent => {
            match create_repository(client, credentials, owner, request, false).await {
                Err(CollapseError::DeletedPending(_)) => Err(desktop_purge_unavailable(
                    owner,
                    request.name,
                    StatusCode::NOT_FOUND,
                )),
                other => other,
            }
        }
        PurgeOutcome::BadConfirmation => {
            retry_purge_with_canonical_casing(client, credentials, owner, request).await
        }
    }
}

/// Collapse checks `full_name == target.owner + "/" + target.name` case-sensitively.
/// Probes via create to learn Collapse's canonical casing when a lowercased project slug
/// mismatched a mixed-case deleted repository name (#610).
async fn retry_purge_with_canonical_casing(
    client: &Client,
    credentials: &CollapseCredentials,
    owner: &str,
    request: &MirrorRequest<'_>,
) -> Result<(ProvisionedMirror, bool), CollapseError> {
    match create_repository(client, credentials, owner, request, false).await {
        Ok(created) => Ok(created),
        Err(CollapseError::DeletedPending(deleted))
            if deleted.owner != owner || deleted.name != request.name =>
        {
            let canonical = MirrorRequest {
                owner: Some(&deleted.owner),
                name: &deleted.name,
                default_branch: request.default_branch,
                description: request.description,
            };
            match purge_repository(client, credentials, &deleted.owner, &deleted.name).await? {
                PurgeOutcome::Purged | PurgeOutcome::AlreadyAbsent => {
                    create_repository(client, credentials, &deleted.owner, &canonical, false).await
                }
                PurgeOutcome::BadConfirmation => {
                    Err(purge_confirmation_rejected(&deleted.owner, &deleted.name))
                }
            }
        }
        Err(CollapseError::DeletedPending(_)) => {
            Err(purge_confirmation_rejected(owner, request.name))
        }
        Err(error) => Err(error),
    }
}

fn purge_confirmation_rejected(owner: &str, name: &str) -> CollapseError {
    CollapseError::Api(format!(
        "Collapse rejected the purge confirmation for {owner}/{name}; purge or restore it from https://collapse.sh/{owner}/{name}/settings and re-run"
    ))
}

fn desktop_purge_unavailable(owner: &str, name: &str, status: StatusCode) -> CollapseError {
    CollapseError::Api(format!(
        "Collapse does not accept desktop purge for {owner}/{name} yet ({status}); purge or restore it from https://collapse.sh/{owner}/{name}/settings and re-run"
    ))
}

async fn purge_repository(
    client: &Client,
    credentials: &CollapseCredentials,
    owner: &str,
    name: &str,
) -> Result<PurgeOutcome, CollapseError> {
    let path = format!("/api/repositories/{}/{}/purge", encode(owner), encode(name));
    let full_name = format!("{owner}/{name}");
    let response = authorized(client, Method::POST, &path, &credentials.access_token)
        .json(&serde_json::json!({ "full_name": full_name }))
        .send()
        .await
        .map_err(failed)?;
    let status = response.status();
    if status == StatusCode::BAD_REQUEST {
        return Ok(PurgeOutcome::BadConfirmation);
    }
    if status == StatusCode::NOT_FOUND {
        return Ok(PurgeOutcome::AlreadyAbsent);
    }
    if matches!(
        status,
        StatusCode::FORBIDDEN | StatusCode::METHOD_NOT_ALLOWED
    ) {
        return Err(desktop_purge_unavailable(owner, name, status));
    }
    let _ = success(response, "purge the deleted repository").await?;
    Ok(PurgeOutcome::Purged)
}

async fn create_repository(
    client: &Client,
    credentials: &CollapseCredentials,
    owner: &str,
    request: &MirrorRequest<'_>,
    allow_non_owner_fallback: bool,
) -> Result<(ProvisionedMirror, bool), CollapseError> {
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
    let status = response.status();
    // A 409 either reports a deleted repository pending purge (collapse.sh#217) or means
    // the account already owns a live repository under this name, which the ready check reads.
    if status == StatusCode::CONFLICT {
        let text = response.text().await.unwrap_or_default();
        if let Some(deleted) = parse_deleted_pending(&text, owner, request.name) {
            return Err(CollapseError::DeletedPending(Box::new(deleted)));
        }
        return Ok((existing(owner, request.name), true));
    }
    // For non-account namespaces (such as an organization the user does not own), Collapse
    // refuses creation with 400/403; fall back to checking whether an existing repo is readable.
    if allow_non_owner_fallback && matches!(status, StatusCode::BAD_REQUEST | StatusCode::FORBIDDEN)
    {
        return Ok((existing(owner, request.name), false));
    }
    // A gateway error (502, 503, 504) can arrive after Collapse made the repository, and a
    // retry would then meet a 409. Adopt what was made; otherwise report the original error.
    if status.is_server_error()
        && let Some(made) =
            adopt_after_server_error(client, credentials, owner, request.name).await?
    {
        return Ok((made, false));
    }
    let reply: RepositoryReply = success(response, "create the repository")
        .await?
        .json()
        .await
        .map_err(failed)?;
    Ok((mirror_from(reply, ProvisionDisposition::Created), false))
}

/// Checks whether Collapse created `owner/name` despite returning a 5xx on create (#686).
/// Ignores non-`DeletedPending` probe failures so the caller reports the original create error.
async fn adopt_after_server_error(
    client: &Client,
    credentials: &CollapseCredentials,
    owner: &str,
    name: &str,
) -> Result<Option<ProvisionedMirror>, CollapseError> {
    match repository_exists_with_client(client, credentials, owner, name).await {
        Ok(Some(mut made)) => {
            made.created = true;
            made.disposition = ProvisionDisposition::Created;
            Ok(Some(made))
        }
        Err(CollapseError::DeletedPending(deleted)) => Err(CollapseError::DeletedPending(deleted)),
        Ok(None) | Err(_) => Ok(None),
    }
}

fn mirror_from(reply: RepositoryReply, disposition: ProvisionDisposition) -> ProvisionedMirror {
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
        created: matches!(
            disposition,
            ProvisionDisposition::Created | ProvisionDisposition::PurgedAndCreated
        ),
        disposition,
    }
}

async fn wait_until_ready(
    client: &Client,
    credentials: &CollapseCredentials,
    mirror: &mut ProvisionedMirror,
    from_create_conflict: bool,
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
        let status = response.status();
        if matches!(
            status,
            StatusCode::NOT_FOUND | StatusCode::CONFLICT | StatusCode::GONE
        ) {
            let body = response.text().await.unwrap_or_default();
            if let Some(deleted) = parse_deleted_pending(&body, &mirror.owner, &mirror.name) {
                return Err(CollapseError::DeletedPending(Box::new(deleted)));
            }
            if status == StatusCode::NOT_FOUND && from_create_conflict {
                // Never report "not found" for a name the create call said exists (#610).
                return Err(CollapseError::DeletedPending(Box::new(
                    DeletedPendingRepository::inferred(&mirror.owner, &mirror.name),
                )));
            }
            if status == StatusCode::NOT_FOUND {
                return Err(CollapseError::Api(format!(
                    "{}/{} was not found on Collapse, or your account cannot see it",
                    mirror.owner, mirror.name
                )));
            }
            return Err(error_from_status(status, &body, "read the repository"));
        }
        let reply: RepositoryReply = success(response, "read the repository")
            .await?
            .json()
            .await
            .map_err(failed)?;
        match reply.state.as_str() {
            "ready" => {
                let disposition = mirror.disposition;
                *mirror = mirror_from(reply, disposition);
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

/// Parses Collapse's `409` `deleted_pending` payload (collapse.sh#217):
/// `{ "error": "deleted_pending", "owner", "name", "delete_after": <unix_ms>, "restore", "purge" }`.
/// Uses untyped JSON lookup so numeric millisecond timestamps and RFC 3339 strings both parse
/// without failing deserialization on unexpected field types.
fn parse_deleted_pending(
    body: &str,
    fallback_owner: &str,
    fallback_name: &str,
) -> Option<DeletedPendingRepository> {
    let value = serde_json::from_str::<Value>(body).ok()?;
    let object = value.as_object()?;
    let delete_after = first_timestamp(
        object,
        &[
            "delete_after",
            "purge_after",
            "purge_at",
            "held_until",
            "deadline",
        ],
    );
    let deleted_at = first_timestamp(object, &["deleted_at"]);
    let marker = object.get("deleted_pending").and_then(Value::as_bool) == Some(true)
        || ["code", "error", "reason", "state", "status"]
            .into_iter()
            .filter_map(|key| object.get(key).and_then(Value::as_str))
            .any(has_deleted_pending_marker)
        || delete_after.is_some()
        || deleted_at.is_some();
    if !marker {
        return None;
    }
    let owner = string_field(object, "owner").unwrap_or_else(|| fallback_owner.to_owned());
    let name = string_field(object, "name").unwrap_or_else(|| fallback_name.to_owned());
    Some(DeletedPendingRepository {
        owner,
        name,
        delete_after,
        deleted_at,
        confirmed: true,
        purge_available: detect_purge_available(object),
        settings_url: string_field(object, "settings_url"),
    })
}

fn detect_purge_available(object: &Map<String, Value>) -> bool {
    if let Some(explicit) = object
        .get("can_purge")
        .or_else(|| object.get("purge_supported"))
        .and_then(Value::as_bool)
    {
        return explicit;
    }
    if string_field(object, "purge").is_some() || string_field(object, "purge_url").is_some() {
        return true;
    }
    if let Some(actions) = object.get("actions").and_then(Value::as_array)
        && !actions.is_empty()
    {
        return actions
            .iter()
            .filter_map(Value::as_str)
            .any(|action| action.eq_ignore_ascii_case("purge"));
    }
    true
}

fn has_deleted_pending_marker(text: &str) -> bool {
    let lower = text.trim().to_ascii_lowercase();
    lower.contains("deleted_pending") || lower.contains("pending_purge")
}

fn string_field(object: &Map<String, Value>, key: &str) -> Option<String> {
    let trimmed = object.get(key)?.as_str()?.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

fn first_timestamp(object: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter()
        .filter_map(|key| object.get(*key))
        .find_map(parse_timestamp)
}

fn parse_timestamp(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return None;
            }
            if trimmed.bytes().all(|byte| byte.is_ascii_digit())
                && let Ok(raw) = trimmed.parse::<u64>()
            {
                return Some(format_unix_timestamp(raw));
            }
            Some(trimmed.to_owned())
        }
        Value::Number(number) => number.as_u64().map(format_unix_timestamp),
        _ => None,
    }
}

fn format_unix_timestamp(raw: u64) -> String {
    let seconds = if raw >= 10_000_000_000 {
        raw / 1_000
    } else {
        raw
    };
    let days = (seconds / 86_400) as i64;
    let remaining = seconds % 86_400;
    let hour = remaining / 3_600;
    let minute = (remaining % 3_600) / 60;
    let second = remaining % 60;
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year_base = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = if month_part < 10 {
        month_part + 3
    } else {
        month_part - 9
    };
    let year = if month <= 2 { year_base + 1 } else { year_base };
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z")
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
    let explanation = response.text().await.unwrap_or_default();
    Err(error_from_status(status, &explanation, action))
}

fn error_from_status(status: StatusCode, body: &str, action: &str) -> CollapseError {
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return CollapseError::Api(format!(
            "Collapse refused to {action} ({status}); disconnect and connect Collapse again in Settings"
        ));
    }
    let explanation: String = body.trim().chars().take(200).collect();
    CollapseError::Api(format!(
        "Could not {action} on Collapse: {status} {explanation}"
    ))
}

pub(crate) fn failed(error: reqwest::Error) -> CollapseError {
    CollapseError::Api(error.without_url().to_string())
}

#[cfg(test)]
mod tests {
    use super::{DeletedPendingRepository, parse_deleted_pending};

    /// Risk (#610, collapse.sh#217): Collapse returns `delete_after` as a Unix-millisecond
    /// integer alongside `restore` and `purge` paths; typing `delete_after` as a JSON string
    /// caused deserialization to fail silently and drop the deadline, purge action and casing.
    #[test]
    fn parses_collapse_deleted_pending_unix_millis_reply() {
        let body = r#"{
            "error": "deleted_pending",
            "owner": "mESmaC",
            "name": "Rosen",
            "delete_after": 1795290000000,
            "restore": "/api/repositories/mESmaC/Rosen/restore",
            "purge": "/api/repositories/mESmaC/Rosen/purge"
        }"#;
        let parsed = parse_deleted_pending(body, "mesmac", "rosen");
        assert_eq!(
            parsed,
            Some(DeletedPendingRepository {
                owner: "mESmaC".into(),
                name: "Rosen".into(),
                delete_after: Some("2026-11-21T19:40:00Z".into()),
                deleted_at: None,
                confirmed: true,
                purge_available: true,
                settings_url: None,
            })
        );
        if let Some(repo) = parsed {
            assert_eq!(
                repo.to_string(),
                "mESmaC/Rosen was deleted on Collapse and is held until 2026-11-21T19:40:00Z"
            );
        }
    }

    #[test]
    fn ignores_live_conflict_and_accepts_rfc3339_deadline() {
        assert_eq!(
            parse_deleted_pending(r#"{"error":"Conflict"}"#, "mESmaC", "rosen"),
            None
        );
        let parsed = parse_deleted_pending(
            r#"{"error":"deleted_pending","delete_after":"2026-11-29T12:00:00Z","can_purge":false}"#,
            "mESmaC",
            "rosen",
        );
        assert_eq!(
            parsed,
            Some(DeletedPendingRepository {
                owner: "mESmaC".into(),
                name: "rosen".into(),
                delete_after: Some("2026-11-29T12:00:00Z".into()),
                deleted_at: None,
                confirmed: true,
                purge_available: false,
                settings_url: None,
            })
        );
    }
}
