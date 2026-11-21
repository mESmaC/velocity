//! Keeping Collapse's `velocity-graph` pin current (SPEC.md F19 "Standalone indexer", #577):
//! the GitHub REST reads and writes behind `velocity_core::graph_indexer_pin`. One look reads
//! Collapse's `ops/Dockerfile`, finds the newest published Velocity release carrying the
//! Linux `velocity-graph` archive and `.sha256`, compares their parser and schema versions,
//! and keeps exactly one open pin-bump issue on Collapse's repository. Everything is REST,
//! which keeps working when a swarm has spent the GraphQL budget.

use std::collections::BTreeMap;

use serde::Deserialize;
use serde_json::{Value, json};
use velocity_core::graph_indexer_pin::{
    self, CollapseGraphPin, DOCKERFILE_PATH, GraphPinBump, IndexerVersion, OpenIssue,
    PARSER_VERSION_PATH, PinBumpStep, ReleaseAssetCandidate, ReleaseCandidate, SCHEMA_VERSION_PATH,
};

use crate::gh_command::{GitHubCommand, GitHubCommandError};

/// Default Collapse GitHub repository candidates checked when no Collapse project remote is
/// configured locally.
pub const COLLAPSE_REPOSITORIES: [&str; 2] = ["mESmaC/collapse.sh", "mESmaC/collapse"];

/// Asks GitHub for a file's raw text instead of its base64 wrapper.
const RAW_CONTENT: &str = "Accept: application/vnd.github.raw+json";
/// Asks GitHub for a release asset's raw bytes (used for `.sha256` checksum files).
const OCTET_STREAM: &str = "Accept: application/octet-stream";

/// Immutable release metadata remembered between looks so each tag's parser/schema versions
/// and each checksum asset ID are read once per process.
#[derive(Default)]
pub struct GraphPinCache {
    versions: BTreeMap<(String, String), IndexerVersion>,
    checksums: BTreeMap<u64, String>,
}

/// What a look found Collapse's indexer pin-bump issue to be.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinIssue {
    /// No newer published release changes the parser or schema version Collapse pins.
    Current,
    Opened(u32),
    Rewritten(u32),
    /// The open issue already names the current target, URL and SHA-256.
    Kept(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueLook {
    pub issue: PinIssue,
    /// Pin-bump issues closed because their bump landed or another issue is kept.
    pub closed: Vec<u32>,
}

/// The GitHub client for keeping Collapse's `velocity-graph` pin-bump issue current.
pub struct GraphIndexerPinWatch {
    command: GitHubCommand,
    velocity: String,
    collapse: String,
}

impl GraphIndexerPinWatch {
    /// `velocity` is Velocity's `owner/name` repository and `collapse` is Collapse's
    /// `owner/name` repository where `ops/Dockerfile` and the pin-bump issue live.
    #[track_caller]
    pub fn new(velocity: &str, collapse: &str) -> Result<Self, GitHubCommandError> {
        let velocity = crate::RepositoryName::parse(velocity)
            .map_err(|error| GitHubCommandError::Invalid(error.to_string()))?;
        let collapse = crate::RepositoryName::parse(collapse)
            .map_err(|error| GitHubCommandError::Invalid(error.to_string()))?;
        Ok(Self {
            command: GitHubCommand::for_repository(&collapse),
            velocity: velocity.full_name(),
            collapse: collapse.full_name(),
        })
    }

    pub fn collapse_repository(&self) -> &str {
        &self.collapse
    }

    /// Whether the signed-in account may push to Collapse's repository, or `None` when the
    /// repository does not exist or is invisible to the signed-in account.
    pub fn can_maintain(&self) -> Result<Option<bool>, GitHubCommandError> {
        #[derive(Deserialize)]
        struct Permissions {
            #[serde(default)]
            push: bool,
        }
        #[derive(Deserialize)]
        struct RepoPermissions {
            #[serde(default)]
            permissions: Option<Permissions>,
        }
        let endpoint = format!("repos/{}", self.collapse);
        match self.command.read::<RepoPermissions>(&["api", &endpoint]) {
            Ok(repo) => Ok(Some(repo.permissions.is_some_and(|perm| perm.push))),
            Err(error) if not_found(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Compares Collapse's pinned `velocity-graph` release against the newest published
    /// Velocity release with an indexer archive and keeps at most one current pin-bump issue.
    pub fn look(&self, cache: &mut GraphPinCache) -> Result<IssueLook, GitHubCommandError> {
        let pin = self.pin()?;
        let velocity = pin.repository.as_deref().unwrap_or(&self.velocity);
        let open = self.open_issues()?;
        let releases = self.releases(velocity)?;
        let Some(latest) = graph_indexer_pin::latest_indexer_release(&releases) else {
            return Ok(IssueLook {
                issue: PinIssue::Current,
                closed: self.close_surplus(&open)?,
            });
        };
        if !graph_indexer_pin::is_newer_version(&latest.version, &pin.version) {
            return Ok(IssueLook {
                issue: PinIssue::Current,
                closed: self.close_surplus(&open)?,
            });
        }
        let pin_indexer = self.indexer_version(cache, velocity, &pin.tag, None)?;
        let target_indexer =
            self.indexer_version(cache, velocity, &latest.tag, Some(latest.archive_asset_id))?;
        if !graph_indexer_pin::should_bump(
            &pin.version,
            pin_indexer,
            &latest.version,
            target_indexer,
        ) {
            return Ok(IssueLook {
                issue: PinIssue::Current,
                closed: self.close_surplus(&open)?,
            });
        }
        let sha256 = self.archive_sha256(
            cache,
            velocity,
            latest.sha256_asset_id,
            &latest.archive_name,
        )?;
        let bump = GraphPinBump {
            velocity_repository: velocity.to_owned(),
            pin,
            pin_indexer,
            target_version: latest.version,
            target_tag: latest.tag,
            target_indexer,
            archive_url: latest.archive_url,
            sha256,
        };
        let (issue, closed) = self.keep_one(&open, &bump)?;
        Ok(IssueLook { issue, closed })
    }

    fn pin(&self) -> Result<CollapseGraphPin, GitHubCommandError> {
        let dockerfile = self.file(&self.collapse, DOCKERFILE_PATH, None)?;
        CollapseGraphPin::parse(&dockerfile).ok_or_else(|| {
            GitHubCommandError::Missing(format!(
                "a velocity-graph pin in {}'s {DOCKERFILE_PATH}",
                self.collapse
            ))
        })
    }

    fn file(
        &self,
        repository: &str,
        path: &str,
        reference: Option<&str>,
    ) -> Result<String, GitHubCommandError> {
        let query = reference.map_or_else(String::new, |tag| format!("?ref={tag}"));
        let endpoint = format!("repos/{repository}/contents/{path}{query}");
        self.command.text(&["api", "-H", RAW_CONTENT, &endpoint])
    }

    fn releases(&self, repository: &str) -> Result<Vec<ReleaseCandidate>, GitHubCommandError> {
        let endpoint = format!("repos/{repository}/releases?per_page=100");
        let listed: Vec<ApiRelease> = self.command.read(&["api", &endpoint])?;
        Ok(listed.into_iter().map(ApiRelease::into_candidate).collect())
    }

    fn indexer_version(
        &self,
        cache: &mut GraphPinCache,
        repository: &str,
        tag: &str,
        archive_asset_id: Option<u64>,
    ) -> Result<IndexerVersion, GitHubCommandError> {
        let key = (
            repository.to_ascii_lowercase(),
            format!("{tag}#{}", archive_asset_id.unwrap_or(0)),
        );
        if let Some(version) = cache.versions.get(&key).copied() {
            return Ok(version);
        }
        let cache_source = self.file(repository, PARSER_VERSION_PATH, Some(tag))?;
        let parser = graph_indexer_pin::parse_parser_version(&cache_source).ok_or_else(|| {
            GitHubCommandError::Missing(format!(
                "PARSER_VERSION in {repository}@{tag}:{PARSER_VERSION_PATH}"
            ))
        })?;
        let cli_source = self.file(repository, SCHEMA_VERSION_PATH, Some(tag))?;
        let schema = graph_indexer_pin::parse_schema_version(&cli_source).ok_or_else(|| {
            GitHubCommandError::Missing(format!(
                "SCHEMA in {repository}@{tag}:{SCHEMA_VERSION_PATH}"
            ))
        })?;
        let version = IndexerVersion { parser, schema };
        cache.versions.insert(key, version);
        Ok(version)
    }

    fn archive_sha256(
        &self,
        cache: &mut GraphPinCache,
        repository: &str,
        asset_id: u64,
        archive_name: &str,
    ) -> Result<String, GitHubCommandError> {
        if let Some(sha256) = cache.checksums.get(&asset_id) {
            return Ok(sha256.clone());
        }
        let endpoint = format!("repos/{repository}/releases/assets/{asset_id}");
        let body = self.command.text(&["api", "-H", OCTET_STREAM, &endpoint])?;
        let sha256 = graph_indexer_pin::parse_sha256(&body, archive_name).ok_or_else(|| {
            GitHubCommandError::Missing(format!("a valid SHA-256 checksum for {archive_name}"))
        })?;
        cache.checksums.insert(asset_id, sha256.clone());
        Ok(sha256)
    }

    fn open_issues(&self) -> Result<Vec<OpenIssue>, GitHubCommandError> {
        if let Some(cached) =
            crate::issue_cache::get_fresh(&self.collapse, crate::OPEN_ISSUES_REFRESH)
        {
            return Ok(cached
                .into_iter()
                .map(|issue| OpenIssue {
                    repository: self.collapse.clone(),
                    number: issue.number,
                    title: issue.title,
                    body: issue.body,
                })
                .collect());
        }
        let endpoint = format!("repos/{}/issues?state=open&per_page=100", self.collapse);
        let listed: Vec<Listed> = self.command.get_paginated(&endpoint)?;
        Ok(listed
            .into_iter()
            .filter(|issue| issue.pull_request.is_none())
            .map(|issue| OpenIssue {
                repository: self.collapse.clone(),
                number: issue.number,
                title: issue.title,
                body: issue.body.unwrap_or_default(),
            })
            .collect())
    }

    fn keep_one(
        &self,
        open: &[OpenIssue],
        bump: &GraphPinBump,
    ) -> Result<(PinIssue, Vec<u32>), GitHubCommandError> {
        let plan = graph_indexer_pin::plan(open, bump);
        let text = json!({"title": bump.title(), "body": bump.body()});
        let issues = format!("repos/{}/issues", self.collapse);
        let (kept, issue) = match plan.step {
            PinBumpStep::Open => {
                let opened: Numbered = self.command.api("POST", &issues, &text)?;
                crate::issue_cache::invalidate(&self.collapse);
                (opened.number, PinIssue::Opened(opened.number))
            }
            PinBumpStep::Rewrite(number) => {
                let _: Value = self
                    .command
                    .api("PATCH", &format!("{issues}/{number}"), &text)?;
                crate::issue_cache::invalidate(&self.collapse);
                (number, PinIssue::Rewritten(number))
            }
            PinBumpStep::Keep(number) => (number, PinIssue::Kept(number)),
        };
        let landed = format!(
            "`{DOCKERFILE_PATH}` already pins `{}`, so this bump is complete. A newer \
             `velocity-graph` release is tracked in #{kept}.",
            bump.pin.version
        );
        for number in &plan.finished {
            self.close(*number, "completed", &landed)?;
        }
        for number in &plan.duplicates {
            self.close(*number, "not_planned", &one_issue_notice(kept))?;
        }
        Ok((issue, [plan.finished, plan.duplicates].concat()))
    }

    fn close_surplus(&self, open: &[OpenIssue]) -> Result<Vec<u32>, GitHubCommandError> {
        let Some((kept, surplus)) = graph_indexer_pin::oldest_and_surplus(open) else {
            return Ok(Vec::new());
        };
        for number in &surplus {
            self.close(*number, "not_planned", &one_issue_notice(kept))?;
        }
        Ok(surplus)
    }

    /// Comment first, state second: a failure never leaves a closed issue without its reason.
    fn close(&self, number: u32, reason: &str, comment: &str) -> Result<(), GitHubCommandError> {
        let issue = format!("repos/{}/issues/{number}", self.collapse);
        let _: Value = self.command.api(
            "POST",
            &format!("{issue}/comments"),
            &json!({"body": comment}),
        )?;
        let _: Value = self.command.api(
            "PATCH",
            &issue,
            &json!({"state": "closed", "state_reason": reason}),
        )?;
        crate::issue_cache::invalidate(&self.collapse);
        Ok(())
    }
}

fn one_issue_notice(kept: u32) -> String {
    format!("Velocity keeps one open velocity-graph pin-bump issue: #{kept}.")
}

fn not_found(error: &GitHubCommandError) -> bool {
    matches!(error, GitHubCommandError::Command { message, .. } if message.contains("HTTP 404"))
}

#[derive(Deserialize)]
struct ApiAsset {
    id: u64,
    name: String,
    #[serde(default)]
    browser_download_url: String,
}

#[derive(Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<ApiAsset>,
}

impl ApiRelease {
    fn into_candidate(self) -> ReleaseCandidate {
        ReleaseCandidate {
            tag: self.tag_name,
            draft: self.draft,
            assets: self
                .assets
                .into_iter()
                .map(|asset| ReleaseAssetCandidate {
                    id: asset.id,
                    name: asset.name,
                    download_url: asset.browser_download_url,
                })
                .collect(),
        }
    }
}

#[derive(Deserialize)]
struct Listed {
    number: u32,
    title: String,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    pull_request: Option<Value>,
}

#[derive(Deserialize)]
struct Numbered {
    number: u32,
}
