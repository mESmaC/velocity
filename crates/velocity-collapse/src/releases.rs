//! Collapse Releases REST client (collapse.sh#194, #593).
//!
//! Talks to `https://api.collapse.sh/api/repositories/{owner}/{name}/releases` to list,
//! create, update and upload assets on versioned and rolling (`updater`) releases.
//! Public repositories can be read without authentication; mutating calls require a
//! Collapse bearer token with the `releases` scope.

use std::time::Duration;

use reqwest::{Client, Method, RequestBuilder, Response, StatusCode};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::CollapseError;
use crate::provision::failed;
use crate::url_text::encode;

/// Collapse API origin serving both API endpoints and release asset downloads.
pub const API_HOST: &str = "https://api.collapse.sh";
/// The rolling release tag holding Velocity's `latest.json` update manifest.
pub const UPDATER_TAG: &str = "updater";
/// Default Collapse repository hosting Velocity releases.
pub const VELOCITY_REPOSITORY: &str = "mESmaC/velocity";
/// Default Collapse repository hosting Rosen pin releases.
pub const ROSEN_REPOSITORY: &str = "mESmaC/rosen";

const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// Release installers can be ~100 MiB and stream through Collapse to R2.
const UPLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// One downloadable asset attached to a Collapse release.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollapseReleaseAsset {
    pub id: u64,
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub content_type: String,
    #[serde(default)]
    pub sha256: String,
    #[serde(default)]
    pub download_url: String,
}

impl CollapseReleaseAsset {
    /// Absolute HTTPS download URL on `https://api.collapse.sh` for this asset.
    pub fn absolute_download_url(&self, owner: &str, name: &str, tag: &str) -> String {
        let raw = self.download_url.trim();
        if raw.starts_with("https://") {
            return raw.to_owned();
        }
        if raw.starts_with('/') {
            return format!("{API_HOST}{raw}");
        }
        download_url(owner, name, tag, &self.name)
    }
}

/// One release returned by Collapse's releases API.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollapseRelease {
    pub id: u64,
    pub tag: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub prerelease: bool,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub rolling: bool,
    #[serde(default)]
    pub latest: bool,
    #[serde(default)]
    pub assets: Vec<CollapseReleaseAsset>,
}

/// Paginated response from `GET /api/repositories/{owner}/{name}/releases`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
pub struct ReleasePage {
    #[serde(default)]
    pub releases: Vec<CollapseRelease>,
    #[serde(default)]
    pub next_before: Option<u64>,
}

/// Body for `POST /api/repositories/{owner}/{name}/releases`.
#[derive(Clone, Debug, Serialize)]
pub struct CreateReleaseRequest<'request> {
    pub tag: &'request str,
    pub name: &'request str,
    pub notes: &'request str,
    pub prerelease: bool,
    pub draft: bool,
    pub rolling: bool,
}

/// Body for `PATCH /api/repositories/{owner}/{name}/releases/{id}`.
#[derive(Clone, Debug, Serialize)]
pub struct UpdateReleaseRequest<'request> {
    pub name: &'request str,
    pub notes: &'request str,
    pub prerelease: bool,
    pub draft: bool,
    pub rolling: bool,
}

/// Public download URL for a Collapse release asset.
pub fn download_url(owner: &str, name: &str, tag: &str, asset: &str) -> String {
    format!(
        "{API_HOST}/{}/{}/releases/download/{}/{}",
        encode(owner),
        encode(name),
        encode(tag),
        encode(asset)
    )
}

/// Base URL (`.../releases/download/<tag>`) for assets of `tag` on Collapse.
pub fn tag_download_base(owner: &str, name: &str, tag: &str) -> String {
    format!(
        "{API_HOST}/{}/{}/releases/download/{}",
        encode(owner),
        encode(name),
        encode(tag)
    )
}

/// Lowercase hex SHA-256 digest of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut text = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(&mut text, "{byte:02x}");
    }
    text
}

/// Lists releases for `owner/name`. Public repositories work with `token = None`.
pub async fn list_releases(
    token: Option<&str>,
    owner: &str,
    name: &str,
    before: Option<u64>,
) -> Result<ReleasePage, CollapseError> {
    let client = http_client(READ_TIMEOUT)?;
    let base = releases_path(owner, name);
    let path = match before.filter(|cursor| *cursor > 0) {
        Some(cursor) => format!("{base}?before={cursor}"),
        None => base,
    };
    let response = request(&client, Method::GET, &path, token)
        .send()
        .await
        .map_err(failed)?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(ReleasePage::default());
    }
    release_success(response, "list releases")
        .await?
        .json()
        .await
        .map_err(failed)
}

/// Reads a single release by `tag`, or `Ok(None)` when no release exists for that tag.
pub async fn get_release_by_tag(
    token: Option<&str>,
    owner: &str,
    name: &str,
    tag: &str,
) -> Result<Option<CollapseRelease>, CollapseError> {
    let client = http_client(READ_TIMEOUT)?;
    let path = format!("{}/tags/{}", releases_path(owner, name), encode(tag));
    let response = request(&client, Method::GET, &path, token)
        .send()
        .await
        .map_err(failed)?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let release = release_success(response, "read the release")
        .await?
        .json()
        .await
        .map_err(failed)?;
    Ok(Some(release))
}

/// Creates a release for `request.tag` on `owner/name`.
pub async fn create_release(
    token: &str,
    owner: &str,
    name: &str,
    body: &CreateReleaseRequest<'_>,
) -> Result<CollapseRelease, CollapseError> {
    let client = http_client(READ_TIMEOUT)?;
    let path = releases_path(owner, name);
    let response = request(&client, Method::POST, &path, Some(token))
        .json(body)
        .send()
        .await
        .map_err(failed)?;
    release_success(response, "create the release")
        .await?
        .json()
        .await
        .map_err(failed)
}

/// Updates an existing release's title, notes and flags on `owner/name`.
pub async fn update_release(
    token: &str,
    owner: &str,
    name: &str,
    release_id: u64,
    body: &UpdateReleaseRequest<'_>,
) -> Result<CollapseRelease, CollapseError> {
    let client = http_client(READ_TIMEOUT)?;
    let path = format!("{}/{release_id}", releases_path(owner, name));
    let response = request(&client, Method::PATCH, &path, Some(token))
        .json(body)
        .send()
        .await
        .map_err(failed)?;
    release_success(response, "update the release")
        .await?
        .json()
        .await
        .map_err(failed)
}

/// Creates `body.tag` when absent, or updates it to match `body` when it already exists.
pub async fn ensure_release(
    token: &str,
    owner: &str,
    name: &str,
    body: &CreateReleaseRequest<'_>,
) -> Result<CollapseRelease, CollapseError> {
    if let Some(existing) = get_release_by_tag(Some(token), owner, name, body.tag).await? {
        return update_release(
            token,
            owner,
            name,
            existing.id,
            &UpdateReleaseRequest {
                name: body.name,
                notes: body.notes,
                prerelease: body.prerelease,
                draft: body.draft,
                rolling: body.rolling,
            },
        )
        .await;
    }
    create_release(token, owner, name, body).await
}

/// Deletes an asset from a Collapse release.
pub async fn delete_asset(
    token: &str,
    owner: &str,
    name: &str,
    release_id: u64,
    asset_id: u64,
) -> Result<(), CollapseError> {
    let client = http_client(READ_TIMEOUT)?;
    let path = format!(
        "{}/{release_id}/assets/{asset_id}",
        releases_path(owner, name)
    );
    let response = request(&client, Method::DELETE, &path, Some(token))
        .send()
        .await
        .map_err(failed)?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(());
    }
    release_success(response, "delete the release asset").await?;
    Ok(())
}

/// Uploads `bytes` as `asset_name` on `release_id`. On a rolling release Collapse replaces
/// an existing asset with the same filename in place.
pub async fn upload_asset(
    token: &str,
    owner: &str,
    name: &str,
    release_id: u64,
    asset_name: &str,
    content_type: &str,
    bytes: Vec<u8>,
) -> Result<CollapseReleaseAsset, CollapseError> {
    let client = http_client(UPLOAD_TIMEOUT)?;
    let path = format!(
        "{}/{release_id}/assets/{}",
        releases_path(owner, name),
        encode(asset_name)
    );
    let mime = if content_type.trim().is_empty() {
        "application/octet-stream"
    } else {
        content_type
    };
    let response = request(&client, Method::PUT, &path, Some(token))
        .header(reqwest::header::CONTENT_TYPE, mime)
        .body(bytes)
        .send()
        .await
        .map_err(failed)?;
    release_success(response, &format!("upload release asset {asset_name}"))
        .await?
        .json()
        .await
        .map_err(failed)
}

/// Uploads `asset_name` to `release`, deleting an existing asset of the same name first on
/// non-rolling releases so re-publishing is idempotent.
pub async fn upload_or_replace_asset(
    token: &str,
    owner: &str,
    name: &str,
    release: &CollapseRelease,
    asset_name: &str,
    content_type: &str,
    bytes: Vec<u8>,
) -> Result<CollapseReleaseAsset, CollapseError> {
    if !release.rolling
        && let Some(existing) = release.assets.iter().find(|asset| asset.name == asset_name)
    {
        delete_asset(token, owner, name, release.id, existing.id).await?;
    }
    upload_asset(
        token,
        owner,
        name,
        release.id,
        asset_name,
        content_type,
        bytes,
    )
    .await
}

/// Downloads a text release asset (such as `latest.json` or `.sha256`) from `url`.
pub async fn download_asset_text(
    token: Option<&str>,
    url: &str,
) -> Result<Option<String>, CollapseError> {
    let client = http_client(READ_TIMEOUT)?;
    let mut builder = client.get(url);
    if let Some(bearer) = token.filter(|text| !text.is_empty()) {
        builder = builder.bearer_auth(bearer);
    }
    let response = builder.send().await.map_err(failed)?;
    if response.status() == StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let text = release_success(response, "download the release asset")
        .await?
        .text()
        .await
        .map_err(failed)?;
    Ok(Some(text))
}

fn releases_path(owner: &str, name: &str) -> String {
    format!(
        "/api/repositories/{}/{}/releases",
        encode(owner),
        encode(name)
    )
}

fn http_client(timeout: Duration) -> Result<Client, CollapseError> {
    Client::builder()
        .timeout(timeout)
        .user_agent("velocity-collapse")
        .build()
        .map_err(failed)
}

fn request(client: &Client, method: Method, path: &str, token: Option<&str>) -> RequestBuilder {
    let mut builder = client
        .request(method, format!("{API_HOST}{path}"))
        .header(reqwest::header::ACCEPT, "application/json");
    if let Some(bearer) = token.filter(|text| !text.is_empty()) {
        builder = builder.bearer_auth(bearer);
    }
    builder
}

async fn release_success(response: Response, action: &str) -> Result<Response, CollapseError> {
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let body = response.text().await.unwrap_or_default();
    let detail = extract_error_message(&body);
    if status == StatusCode::UNAUTHORIZED || status == StatusCode::FORBIDDEN {
        return Err(CollapseError::ReleaseUnauthorized(format!(
            "Collapse refused to {action} ({status}{detail}). Save a Collapse personal access token with the `releases` scope (https://collapse.sh/settings/tokens) in Settings → Collapse → Release credentials."
        )));
    }
    Err(CollapseError::Api(format!(
        "Could not {action} on Collapse: {status}{detail}"
    )))
}

pub(crate) fn extract_error_message(body: &str) -> String {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(body)
        && let Some(message) = value.get("error").and_then(serde_json::Value::as_str)
        && !message.trim().is_empty()
    {
        return format!(": {}", message.trim());
    }
    let trimmed: String = body.trim().chars().take(200).collect();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!(" {trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_page_and_builds_absolute_download_urls()
    -> Result<(), Box<dyn std::error::Error>> {
        let page: ReleasePage = serde_json::from_str(
            r#"{
                "releases": [
                    {
                        "id": 42,
                        "tag": "v0.1.0-beta.21",
                        "name": "Velocity 0.1.0-beta.21",
                        "notes": "Signed release",
                        "prerelease": true,
                        "draft": false,
                        "rolling": false,
                        "latest": false,
                        "assets": [
                            {
                                "id": 7,
                                "name": "latest.json",
                                "size": 512,
                                "content_type": "application/json",
                                "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                                "download_url": "/mESmaC/velocity/releases/download/v0.1.0-beta.21/latest.json"
                            }
                        ]
                    }
                ]
            }"#,
        )?;
        assert_eq!(page.releases.len(), 1);
        let release = &page.releases[0];
        assert_eq!(release.tag, "v0.1.0-beta.21");
        assert_eq!(
            release.assets[0].absolute_download_url("mESmaC", "velocity", &release.tag),
            "https://api.collapse.sh/mESmaC/velocity/releases/download/v0.1.0-beta.21/latest.json"
        );
        assert_eq!(
            download_url("mESmaC", "velocity", UPDATER_TAG, "latest.json"),
            "https://api.collapse.sh/mESmaC/velocity/releases/download/updater/latest.json"
        );
        assert_eq!(
            sha256_hex(b"hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        Ok(())
    }
}
