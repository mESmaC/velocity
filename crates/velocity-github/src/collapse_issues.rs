//! Collapse C14 Issues backend for [`crate::IssueClient`] (#589).
//!
//! Uses Collapse's no-wake edge cursor (`/issues/cursor`) + incremental change feed
//! (`/issues/changes?after=<cursor>`) instead of polling full issue lists, supports
//! batch reads (`?numbers=1,2,3`) and ETag conditional requests, and never touches the
//! GitHub `gh` CLI or `gh_rate` request counter.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::{LazyLock, Mutex, OnceLock, mpsc};

use velocity_collapse::CollapseError;
use velocity_collapse::issues::{
    CollapseComment, CollapseIssue, CollapseIssueApi, Conditional, CreateIssueRequest,
    DEFAULT_CHANGES_LIMIT, DependencyTarget, MAX_BATCH_NUMBERS, MAX_LIST_LIMIT, UpdateIssueRequest,
};
use velocity_core::collapse::{IssueHost, issue_comment_page, issue_page};
use velocity_core::issue_dependencies::IssueReference;

use crate::agent_issues::{IssueCommentEntry, IssueDependency, IssueDetail};
use crate::issue_cache;
use crate::issues::{relative_blocker_name, scout_attribution};
use crate::rosen_issue::unix_seconds;
use crate::{GitHubActor, GitHubCommandError, GitHubIssue, IntegrationCoverage, IssueState};

type BoxedJob = Box<dyn FnOnce() -> Pin<Box<dyn Future<Output = ()> + Send>> + Send>;

static WORKER: OnceLock<Option<tokio::sync::mpsc::UnboundedSender<BoxedJob>>> = OnceLock::new();
static APIS: LazyLock<Mutex<HashMap<String, CollapseIssueApi>>> = LazyLock::new(Mutex::default);
static REPO_CACHES: LazyLock<Mutex<HashMap<(String, String), CollapseRepoCache>>> =
    LazyLock::new(Mutex::default);

#[derive(Default)]
struct CollapseRepoCache {
    initialized: bool,
    cursor: u64,
    cursor_etag: Option<String>,
    open_issues: BTreeMap<u32, GitHubIssue>,
    reopened_at: BTreeMap<u32, String>,
    comments: HashMap<u32, CachedComments>,
    labels: Option<Vec<String>>,
    labels_etag: Option<String>,
    mirrored_commits: HashSet<String>,
}

#[derive(Clone, Default)]
struct CachedComments {
    etag: Option<String>,
    items: Vec<CollapseComment>,
}

/// Collapse C14 Issues transport for one repository (`owner/name`).
pub(crate) struct CollapseIssueTransport {
    owner: String,
    name: String,
    repository: String,
    base_url: String,
    explicit_token: Option<String>,
}

impl CollapseIssueTransport {
    pub(crate) fn for_repository(repository: &str) -> Result<Self, GitHubCommandError> {
        Self::with_endpoint(
            velocity_collapse::issues::API_HOST.to_owned(),
            None,
            repository,
        )
    }

    pub(crate) fn with_endpoint(
        base_url: String,
        token: Option<String>,
        repository: &str,
    ) -> Result<Self, GitHubCommandError> {
        let (owner, name) =
            velocity_core::collapse::parse_repository(repository).ok_or_else(|| {
                GitHubCommandError::Invalid(format!("Invalid Collapse repository {repository:?}"))
            })?;
        Ok(Self {
            repository: format!("{owner}/{name}"),
            owner,
            name,
            base_url: base_url.trim_end_matches('/').to_owned(),
            explicit_token: token,
        })
    }

    pub(crate) fn repository(&self) -> &str {
        &self.repository
    }

    pub(crate) fn owner(&self) -> &str {
        &self.owner
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Reads every open and closed issue in this repository (`state=all`), paginated (#590).
    pub(crate) fn all_issues(&self) -> Result<Vec<GitHubIssue>, GitHubCommandError> {
        let api = self.api()?;
        let token = self.read_token()?;
        let mut issues = Vec::new();
        let mut offset = 0_usize;
        loop {
            let api = api.clone();
            let token = token.clone();
            let owner = self.owner.clone();
            let name = self.name.clone();
            let response = block_on_worker(async move {
                api.list_issues(
                    token.as_deref(),
                    &owner,
                    &name,
                    Some("all"),
                    None,
                    MAX_LIST_LIMIT,
                    offset,
                    None,
                )
                .await
            })
            .map_err(from_collapse_error)?;
            let Conditional::Modified { value, .. } = response else {
                break;
            };
            let page_len = value.issues.len();
            for raw in value.issues {
                let reopened = self.with_cache(|cache| cache.reopened_at.get(&raw.number).cloned());
                issues.push(convert_issue(&self.repository, &raw, reopened));
            }
            if page_len < MAX_LIST_LIMIT {
                break;
            }
            offset += page_len;
        }
        Ok(issues)
    }

    /// Reads a raw UTF-8 file from Collapse's codeview API at `reference` (default branch
    /// when `None`) without calling GitHub (#590).
    pub(crate) fn read_raw_file(
        &self,
        path: &str,
        reference: Option<&str>,
    ) -> Result<Option<String>, GitHubCommandError> {
        let api = self.api()?;
        let token = self.read_code_token()?;
        let owner = self.owner.clone();
        let name = self.name.clone();
        let path_owned = path.to_owned();
        let reference_owned = reference.map(str::to_owned);
        block_on_worker(async move {
            api.get_raw_file(
                token.as_deref(),
                &owner,
                &name,
                &path_owned,
                reference_owned.as_deref(),
            )
            .await
        })
        .map_err(from_collapse_error)
    }

    /// Reads up to `limit` newest commits on the repository's default branch from Collapse
    /// without calling GitHub (#590).
    pub(crate) fn read_history_commits(
        &self,
        limit: usize,
    ) -> Result<Vec<(String, String)>, GitHubCommandError> {
        let api = self.api()?;
        let token = self.read_code_token()?;
        let mut commits = Vec::new();
        let mut page = 1_usize;
        while commits.len() < limit {
            let api = api.clone();
            let token = token.clone();
            let owner = self.owner.clone();
            let name = self.name.clone();
            let result = block_on_worker(async move {
                api.list_history(token.as_deref(), &owner, &name, page)
                    .await
            })
            .map_err(from_collapse_error)?;
            if result.lands.is_empty() {
                break;
            }
            for land in result.lands {
                let message = if land.message.trim().is_empty() {
                    land.title
                } else {
                    land.message
                };
                commits.push((land.commit, message));
                if commits.len() >= limit {
                    break;
                }
            }
            if !result.has_next {
                break;
            }
            page += 1;
        }
        Ok(commits)
    }

    /// Whether the connected Collapse account manages this repository and can write issues
    /// (`Ok(None)` when the repository is not found on the account) (#590).
    pub(crate) fn can_maintain(&self) -> Result<Option<bool>, GitHubCommandError> {
        if self.explicit_token.is_some() {
            return Ok(Some(true));
        }
        if self.read_token()?.is_none() {
            return Ok(Some(false));
        }
        let credentials = match velocity_collapse::load_credentials() {
            Ok(Some(credentials)) => credentials,
            Ok(None) => return Ok(Some(false)),
            Err(error) => return Err(from_collapse_error(error)),
        };
        let rows =
            block_on_worker(
                async move { velocity_collapse::repository_activity(&credentials).await },
            )
            .map_err(from_collapse_error)?;
        let owned = rows.iter().any(|row| {
            row.owner.eq_ignore_ascii_case(&self.owner) && row.name.eq_ignore_ascii_case(&self.name)
        });
        Ok(owned.then_some(true))
    }

    /// Returns open issues by watching Collapse's edge cursor and syncing deltas via
    /// `GET .../issues/changes?after=<cursor>`.
    pub(crate) fn open_issues(&self) -> Result<Vec<GitHubIssue>, GitHubCommandError> {
        let lock = issue_cache::fetch_lock(IssueHost::Collapse, &self.repository);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (initialized, cursor, cursor_etag) = self.snapshot_cursor_state();
        if !initialized {
            return self.load_initial_open_issues_locked();
        }
        let api = self.api()?;
        let token = self.read_token()?;
        let owner = self.owner.clone();
        let name = self.name.clone();
        let cursor_result = block_on_worker(async move {
            api.get_cursor(token.as_deref(), &owner, &name, cursor_etag.as_deref())
                .await
        })
        .map_err(from_collapse_error)?;
        match cursor_result {
            Some(Conditional::NotModified) => Ok(self.cached_open_issues()),
            Some(Conditional::Modified { value, etag }) => {
                self.store_cursor_etag(etag);
                if value <= cursor {
                    Ok(self.cached_open_issues())
                } else {
                    self.sync_changes_locked(cursor)
                }
            }
            // Edge KV entry absent (brand-new repo or local test server): fall back to
            // conditional `changes?after=` so an unchanged origin still answers 304.
            None => self.sync_changes_locked(cursor),
        }
    }

    /// Forces an origin delta sync (`changes?after=`) bypassing edge KV TTL, used right
    /// before duplicate-checking a newly filed issue.
    pub(crate) fn open_issues_fresh(&self) -> Result<Vec<GitHubIssue>, GitHubCommandError> {
        let lock = issue_cache::fetch_lock(IssueHost::Collapse, &self.repository);
        let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let (initialized, cursor, _) = self.snapshot_cursor_state();
        if !initialized {
            return self.load_initial_open_issues_locked();
        }
        self.sync_changes_locked(cursor)
    }

    /// Batch-reads issues by number (`?numbers=1,2,3`), chunked to 100 per request.
    pub(crate) fn get_batch(
        &self,
        numbers: &[u32],
    ) -> Result<Vec<GitHubIssue>, GitHubCommandError> {
        let mut unique: Vec<u32> = numbers.iter().copied().filter(|n| *n > 0).collect();
        unique.sort_unstable();
        unique.dedup();
        if unique.is_empty() {
            return Ok(Vec::new());
        }
        let api = self.api()?;
        let token = self.read_token()?;
        let mut results = Vec::with_capacity(unique.len());
        for chunk in unique.chunks(MAX_BATCH_NUMBERS) {
            let api = api.clone();
            let token = token.clone();
            let owner = self.owner.clone();
            let name = self.name.clone();
            let batch = chunk.to_vec();
            let response = block_on_worker(async move {
                api.list_issues(
                    token.as_deref(),
                    &owner,
                    &name,
                    Some("all"),
                    Some(&batch),
                    batch.len(),
                    0,
                    None,
                )
                .await
            })
            .map_err(from_collapse_error)?;
            if let Conditional::Modified { value, .. } = response {
                for raw in value.issues {
                    let converted = self.upsert_collapse_issue(raw);
                    results.push(converted);
                }
            }
        }
        Ok(results)
    }

    /// Reads one issue with labels, dependencies and comments (`GET .../issues/{number}`).
    pub(crate) fn read_issue(&self, number: u32) -> Result<GitHubIssue, GitHubCommandError> {
        if number == 0 {
            return Err(GitHubCommandError::Invalid(
                "issue must be a positive issue number".into(),
            ));
        }
        let api = self.api()?;
        let token = self.read_token()?;
        let owner = self.owner.clone();
        let name = self.name.clone();
        let response = block_on_worker(async move {
            api.get_issue(token.as_deref(), &owner, &name, number, None)
                .await
        })
        .map_err(from_collapse_error)?;
        let Conditional::Modified { value: raw, .. } = response else {
            return Err(GitHubCommandError::Missing("issue response".into()));
        };
        Ok(self.upsert_collapse_issue(raw))
    }

    /// Reads full issue details (including dependencies and recent comments) in one request.
    pub(crate) fn read_issue_detail_for(
        &self,
        owner: &str,
        name: &str,
        repository: &str,
        number: u32,
        recent_comments: usize,
    ) -> Result<IssueDetail, GitHubCommandError> {
        if number == 0 {
            return Err(GitHubCommandError::Invalid(
                "issue must be a positive issue number".into(),
            ));
        }
        let api = self.api()?;
        let token = self.read_token()?;
        let owner_owned = owner.to_owned();
        let name_owned = name.to_owned();
        let response = block_on_worker(async move {
            api.get_issue(token.as_deref(), &owner_owned, &name_owned, number, None)
                .await
        })
        .map_err(from_collapse_error)?;
        let Conditional::Modified { value: raw, .. } = response else {
            return Err(GitHubCommandError::Missing("issue detail".into()));
        };
        let detail = build_issue_detail(repository, &raw, recent_comments);
        if repository.eq_ignore_ascii_case(&self.repository) {
            self.upsert_collapse_issue(raw);
        }
        Ok(detail)
    }

    /// Reads all comments on `number` (oldest first), using `If-None-Match` ETag caching.
    pub(crate) fn issue_comments(
        &self,
        number: u32,
    ) -> Result<Vec<CollapseComment>, GitHubCommandError> {
        let cached_etag = self.with_cache(|cache| {
            cache
                .comments
                .get(&number)
                .and_then(|entry| entry.etag.clone())
        });
        let api = self.api()?;
        let token = self.read_token()?;
        let owner = self.owner.clone();
        let name = self.name.clone();
        let response = block_on_worker(async move {
            api.list_comments(
                token.as_deref(),
                &owner,
                &name,
                number,
                cached_etag.as_deref(),
            )
            .await
        })
        .map_err(from_collapse_error)?;
        match response {
            Conditional::NotModified => Ok(self.with_cache(|cache| {
                cache
                    .comments
                    .get(&number)
                    .map(|entry| entry.items.clone())
                    .unwrap_or_default()
            })),
            Conditional::Modified { value, etag } => {
                let items = value.comments;
                self.with_cache(|cache| {
                    cache.comments.insert(
                        number,
                        CachedComments {
                            etag,
                            items: items.clone(),
                        },
                    );
                });
                Ok(items)
            }
        }
    }

    pub(crate) fn create_issue(
        &self,
        title: &str,
        body: &str,
        labels: &[String],
        blocked_by: &[String],
    ) -> Result<GitHubIssue, GitHubCommandError> {
        let api = self.api()?;
        let token = self.write_token()?;
        let owner = self.owner.clone();
        let name = self.name.clone();
        let title_owned = title.to_owned();
        let body_owned = body.to_owned();
        let labels_owned = labels.to_vec();
        let blocked_owned = blocked_by.to_vec();
        let created = block_on_worker(async move {
            api.create_issue(
                &token,
                &owner,
                &name,
                &CreateIssueRequest {
                    title: &title_owned,
                    body: &body_owned,
                    labels: labels_owned,
                    blocked_by: blocked_owned,
                },
            )
            .await
        })
        .map_err(from_collapse_error)?;
        Ok(self.upsert_collapse_issue(created))
    }

    pub(crate) fn add_comment(
        &self,
        number: u32,
        body: &str,
    ) -> Result<String, GitHubCommandError> {
        let api = self.api()?;
        let token = self.write_token()?;
        let owner = self.owner.clone();
        let name = self.name.clone();
        let body_owned = body.to_owned();
        let comment = block_on_worker(async move {
            api.create_comment(&token, &owner, &name, number, &body_owned)
                .await
        })
        .map_err(from_collapse_error)?;
        let comment_id = comment.id;
        self.with_cache(|cache| {
            if let Some(entry) = cache.comments.get_mut(&number) {
                entry.etag = None;
                upsert_comment(&mut entry.items, comment);
            }
        });
        issue_cache::invalidate_discussion(IssueHost::Collapse, &self.repository, number);
        Ok(issue_comment_page(&self.repository, number, comment_id)
            .unwrap_or_else(|| format!("https://collapse.sh/{}/issues/{number}", self.repository)))
    }

    pub(crate) fn update_issue(
        &self,
        number: u32,
        patch: UpdateIssueRequest,
    ) -> Result<GitHubIssue, GitHubCommandError> {
        let api = self.api()?;
        let token = self.write_token()?;
        let owner = self.owner.clone();
        let name = self.name.clone();
        let updated = block_on_worker(async move {
            api.update_issue(&token, &owner, &name, number, &patch)
                .await
        })
        .map_err(from_collapse_error)?;
        Ok(self.upsert_collapse_issue(updated))
    }

    pub(crate) fn close_issue(
        &self,
        number: u32,
        reason: &str,
        comment: &str,
    ) -> Result<GitHubIssue, GitHubCommandError> {
        let api = self.api()?;
        let token = self.write_token()?;
        let owner = self.owner.clone();
        let name = self.name.clone();
        let reason_owned = reason.to_owned();
        let comment_owned = comment.to_owned();
        let closed = block_on_worker(async move {
            api.close_issue(&token, &owner, &name, number, &reason_owned, &comment_owned)
                .await
        })
        .map_err(from_collapse_error)?;
        Ok(self.upsert_collapse_issue(closed))
    }

    pub(crate) fn label_names(&self) -> Result<Vec<String>, GitHubCommandError> {
        let cached_etag = self.with_cache(|cache| cache.labels_etag.clone());
        let api = self.api()?;
        let token = self.read_token()?;
        let owner = self.owner.clone();
        let name = self.name.clone();
        let response = block_on_worker(async move {
            api.list_labels(token.as_deref(), &owner, &name, cached_etag.as_deref())
                .await
        })
        .map_err(from_collapse_error)?;
        match response {
            Conditional::NotModified => {
                Ok(self.with_cache(|cache| cache.labels.clone().unwrap_or_default()))
            }
            Conditional::Modified { value, etag } => {
                let names: Vec<String> = value.labels.into_iter().map(|l| l.name).collect();
                self.with_cache(|cache| {
                    cache.labels = Some(names.clone());
                    cache.labels_etag = etag;
                });
                Ok(names)
            }
        }
    }

    pub(crate) fn ensure_label(&self, name: &str) -> Result<(), GitHubCommandError> {
        self.ensure_label_with(name, "ededed", "")
    }

    pub(crate) fn ensure_label_with(
        &self,
        name: &str,
        color: &str,
        description: &str,
    ) -> Result<(), GitHubCommandError> {
        let existing = self.label_names()?;
        if existing
            .iter()
            .any(|label| label.eq_ignore_ascii_case(name))
        {
            return Ok(());
        }
        let api = self.api()?;
        let token = self.write_token()?;
        let owner = self.owner.clone();
        let repo = self.name.clone();
        let label_name = name.to_owned();
        let label_color = color.to_owned();
        let label_description = description.to_owned();
        let _ = block_on_worker(async move {
            api.create_label(
                &token,
                &owner,
                &repo,
                &label_name,
                &label_color,
                &label_description,
            )
            .await
        })
        .map_err(from_collapse_error)?;
        self.with_cache(|cache| {
            cache.labels_etag = None;
            let labels = cache.labels.get_or_insert_with(Vec::new);
            if !labels.iter().any(|item| item.eq_ignore_ascii_case(name)) {
                labels.push(name.to_owned());
            }
        });
        Ok(())
    }

    pub(crate) fn add_dependency(
        &self,
        number: u32,
        blocker: &IssueReference,
    ) -> Result<(), GitHubCommandError> {
        let repo = blocker.repository_or(&self.repository);
        let (dep_owner, dep_repo) = if !repo.eq_ignore_ascii_case(&self.repository) {
            let (owner, name) =
                velocity_core::collapse::parse_repository(repo).ok_or_else(|| {
                    GitHubCommandError::Invalid(format!("Invalid blocker repository `{repo}`"))
                })?;
            (Some(owner), Some(name))
        } else {
            (None, None)
        };
        let api = self.api()?;
        let token = self.write_token()?;
        let owner = self.owner.clone();
        let name = self.name.clone();
        let dep_number = blocker.number();
        block_on_worker(async move {
            api.add_dependency(
                &token,
                &owner,
                &name,
                number,
                &DependencyTarget {
                    owner: dep_owner.as_deref(),
                    repo: dep_repo.as_deref(),
                    number: dep_number,
                },
            )
            .await
        })
        .map_err(from_collapse_error)?;
        Ok(())
    }

    pub(crate) fn validate_blockers(
        &self,
        blockers: &[IssueReference],
    ) -> Result<Vec<String>, GitHubCommandError> {
        if blockers.is_empty() {
            return Ok(Vec::new());
        }
        let mut same_repo = Vec::new();
        let mut cross_repo = Vec::new();
        for blocker in blockers {
            let repo = blocker.repository_or(&self.repository);
            if repo.eq_ignore_ascii_case(&self.repository) {
                same_repo.push(blocker.number());
            } else {
                cross_repo.push((
                    repo.to_owned(),
                    blocker.number(),
                    blocker.relative_to(&self.repository),
                ));
            }
        }
        if !same_repo.is_empty() {
            let found = self.get_batch(&same_repo)?;
            let found_numbers: HashSet<u32> = found.iter().map(|issue| issue.number).collect();
            for number in &same_repo {
                if !found_numbers.contains(number) {
                    return Err(GitHubCommandError::Invalid(format!(
                        "Could not read blocker #{number}: not found on Collapse"
                    )));
                }
            }
        }
        for (repo, number, label) in cross_repo {
            let (owner, name) =
                velocity_core::collapse::parse_repository(&repo).ok_or_else(|| {
                    GitHubCommandError::Invalid(format!(
                        "Could not read blocker {label}: invalid repository"
                    ))
                })?;
            let api = self.api()?;
            let token = self.read_token()?;
            block_on_worker(async move {
                api.get_issue(token.as_deref(), &owner, &name, number, None)
                    .await
            })
            .map_err(|error| {
                GitHubCommandError::Invalid(format!("Could not read blocker {label}: {error}"))
            })?;
        }
        Ok(blockers
            .iter()
            .map(|blocker| blocker.relative_to(&self.repository))
            .collect())
    }

    pub(crate) fn commit_mirrored(&self, commit: &str) -> Result<bool, GitHubCommandError> {
        let lower = commit.to_ascii_lowercase();
        if self.with_cache(|cache| cache.mirrored_commits.contains(&lower)) {
            return Ok(true);
        }
        let api = self.api()?;
        let token = self.read_token()?;
        let owner = self.owner.clone();
        let name = self.name.clone();
        let sha = lower.clone();
        let exists = block_on_worker(async move {
            api.commit_exists(token.as_deref(), &owner, &name, &sha)
                .await
        })
        .map_err(from_collapse_error)?;
        if exists {
            self.with_cache(|cache| {
                cache.mirrored_commits.insert(lower);
            });
        }
        Ok(exists)
    }

    pub(crate) fn integration_status_covering(
        &self,
        commit: &str,
    ) -> Result<Option<IntegrationCoverage>, GitHubCommandError> {
        let credentials = match velocity_collapse::load_credentials() {
            Ok(Some(credentials)) => credentials,
            Ok(None) => return Ok(None),
            Err(error) => return Err(from_collapse_error(error)),
        };
        let owner = self.owner.clone();
        let name = self.name.clone();
        let post_land = block_on_worker(async move {
            velocity_collapse::repository_post_land(&credentials, &owner, &name).await
        })
        .map_err(from_collapse_error)?;
        let Some(post_land) = post_land else {
            return Ok(None);
        };
        let lower = commit.to_ascii_lowercase();
        if let Some(at) = post_land.green_events.get(&lower) {
            return Ok(Some(IntegrationCoverage {
                commit: lower,
                passed_at: unix_seconds(at),
            }));
        }
        if post_land
            .green_commit
            .as_deref()
            .is_some_and(|green| green.eq_ignore_ascii_case(&lower))
        {
            return Ok(Some(IntegrationCoverage {
                commit: lower,
                passed_at: post_land.green_at.as_deref().and_then(unix_seconds),
            }));
        }
        Ok(None)
    }

    fn load_initial_open_issues_locked(&self) -> Result<Vec<GitHubIssue>, GitHubCommandError> {
        let api = self.api()?;
        let token = self.read_token()?;
        let mut collected = BTreeMap::new();
        let mut cursor = 0_u64;
        let mut offset = 0_usize;
        loop {
            let api = api.clone();
            let token = token.clone();
            let owner = self.owner.clone();
            let name = self.name.clone();
            let response = block_on_worker(async move {
                api.list_issues(
                    token.as_deref(),
                    &owner,
                    &name,
                    Some("open"),
                    None,
                    MAX_LIST_LIMIT,
                    offset,
                    None,
                )
                .await
            })
            .map_err(from_collapse_error)?;
            let Conditional::Modified { value, .. } = response else {
                break;
            };
            cursor = cursor.max(value.cursor);
            let page_len = value.issues.len();
            for raw in value.issues {
                let converted = convert_issue(&self.repository, &raw, None);
                collected.insert(converted.number, converted);
            }
            if page_len < MAX_LIST_LIMIT {
                break;
            }
            offset += page_len;
        }
        let issues: Vec<GitHubIssue> = collected.values().cloned().collect();
        self.with_cache(|cache| {
            cache.initialized = true;
            cache.cursor = cursor;
            cache.cursor_etag = None;
            cache.open_issues = collected;
        });
        issue_cache::store(IssueHost::Collapse, &self.repository, &issues);
        Ok(issues)
    }

    fn sync_changes_locked(
        &self,
        start_after: u64,
    ) -> Result<Vec<GitHubIssue>, GitHubCommandError> {
        let api = self.api()?;
        let token = self.read_token()?;
        let mut after = start_after;
        let mut final_cursor = start_after;
        loop {
            let api = api.clone();
            let token = token.clone();
            let owner = self.owner.clone();
            let name = self.name.clone();
            let response = block_on_worker(async move {
                api.get_changes(
                    token.as_deref(),
                    &owner,
                    &name,
                    after,
                    DEFAULT_CHANGES_LIMIT,
                    None,
                )
                .await
            })
            .map_err(from_collapse_error)?;
            let Conditional::Modified { value, .. } = response else {
                break;
            };
            final_cursor = final_cursor.max(value.cursor);
            let next_after = value
                .changes
                .last()
                .map(|change| change.cursor)
                .unwrap_or(value.cursor);
            self.apply_changes(value.changes, value.issues, value.comments, value.labels);
            if !value.has_more || next_after <= after {
                break;
            }
            after = next_after;
        }
        let issues = self.with_cache(|cache| {
            cache.cursor = cache.cursor.max(final_cursor);
            cache.open_issues.values().cloned().collect::<Vec<_>>()
        });
        issue_cache::store(IssueHost::Collapse, &self.repository, &issues);
        Ok(issues)
    }

    fn apply_changes(
        &self,
        changes: Vec<velocity_collapse::issues::IssueChange>,
        issues: Vec<CollapseIssue>,
        comments: Vec<CollapseComment>,
        labels: Vec<velocity_collapse::issues::CollapseLabel>,
    ) {
        let mut invalidated_discussions = Vec::new();
        self.with_cache(|cache| {
            for change in &changes {
                if change.kind == "issue.reopened"
                    && let Some(number) = change.issue_number
                    && !change.created_at.is_empty()
                {
                    cache.reopened_at.insert(number, change.created_at.clone());
                }
                if change.kind.starts_with("comment.")
                    && let Some(number) = change.issue_number
                {
                    invalidated_discussions.push(number);
                }
            }
            if !labels.is_empty() {
                cache.labels = Some(labels.into_iter().map(|label| label.name).collect());
                cache.labels_etag = None;
            }
            for raw in issues {
                let reopened = cache.reopened_at.get(&raw.number).cloned();
                let converted = convert_issue(&self.repository, &raw, reopened);
                if converted.state == IssueState::Closed {
                    cache.open_issues.remove(&converted.number);
                } else {
                    cache.open_issues.insert(converted.number, converted);
                }
            }
            for comment in comments {
                invalidated_discussions.push(comment.issue_number);
                if let Some(entry) = cache.comments.get_mut(&comment.issue_number) {
                    entry.etag = None;
                    upsert_comment(&mut entry.items, comment);
                }
            }
        });
        for number in invalidated_discussions {
            issue_cache::invalidate_discussion(IssueHost::Collapse, &self.repository, number);
        }
    }

    fn upsert_collapse_issue(&self, raw: CollapseIssue) -> GitHubIssue {
        let number = raw.number;
        let comments = raw.comments.clone();
        let converted = self.with_cache(|cache| {
            let reopened = cache.reopened_at.get(&number).cloned();
            let converted = convert_issue(&self.repository, &raw, reopened);
            if cache.initialized {
                if converted.state == IssueState::Closed {
                    cache.open_issues.remove(&number);
                } else {
                    cache.open_issues.insert(number, converted.clone());
                }
            }
            if !comments.is_empty() || raw.comment_count == 0 {
                cache.comments.insert(
                    number,
                    CachedComments {
                        etag: None,
                        items: comments,
                    },
                );
            }
            converted
        });
        issue_cache::upsert_issue(IssueHost::Collapse, &self.repository, &converted);
        converted
    }

    fn snapshot_cursor_state(&self) -> (bool, u64, Option<String>) {
        self.with_cache(|cache| (cache.initialized, cache.cursor, cache.cursor_etag.clone()))
    }

    fn store_cursor_etag(&self, etag: Option<String>) {
        self.with_cache(|cache| {
            cache.cursor_etag = etag;
        });
    }

    fn cached_open_issues(&self) -> Vec<GitHubIssue> {
        self.with_cache(|cache| cache.open_issues.values().cloned().collect())
    }

    fn cache_key(&self) -> (String, String) {
        (self.base_url.clone(), self.repository.to_ascii_lowercase())
    }

    fn with_cache<T>(&self, edit: impl FnOnce(&mut CollapseRepoCache) -> T) -> T {
        let mut caches = REPO_CACHES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let entry = caches.entry(self.cache_key()).or_default();
        edit(entry)
    }

    fn api(&self) -> Result<CollapseIssueApi, GitHubCommandError> {
        let mut apis = APIS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(existing) = apis.get(&self.base_url) {
            return Ok(existing.clone());
        }
        let created =
            CollapseIssueApi::with_base_url(&self.base_url).map_err(from_collapse_error)?;
        apis.insert(self.base_url.clone(), created.clone());
        Ok(created)
    }

    fn read_token(&self) -> Result<Option<String>, GitHubCommandError> {
        if let Some(token) = &self.explicit_token {
            return Ok(Some(token.clone()));
        }
        velocity_collapse::resolve_issues_token().map_err(from_collapse_error)
    }

    fn read_code_token(&self) -> Result<Option<String>, GitHubCommandError> {
        if let Some(token) = &self.explicit_token {
            return Ok(Some(token.clone()));
        }
        if let Some(credentials) =
            velocity_collapse::load_credentials().map_err(from_collapse_error)?
            && !credentials.mcp_token.trim().is_empty()
        {
            return Ok(Some(credentials.mcp_token));
        }
        self.read_token()
    }

    fn write_token(&self) -> Result<String, GitHubCommandError> {
        self.read_token()?.ok_or_else(|| {
            GitHubCommandError::Invalid(
                "No Collapse token is configured. Save a Collapse personal access token with the `issues` scope in Settings → Collapse."
                    .into(),
            )
        })
    }
}

fn build_issue_detail(
    repository: &str,
    raw: &CollapseIssue,
    recent_comments: usize,
) -> IssueDetail {
    let total_comments = raw.comment_count.max(raw.comments.len());
    let comments = if recent_comments == 0 {
        Vec::new()
    } else {
        let skip = raw.comments.len().saturating_sub(recent_comments);
        raw.comments
            .iter()
            .skip(skip)
            .map(|comment| IssueCommentEntry {
                author: comment_author(&comment.author.name),
                created_at: comment.created_at.clone(),
                body: comment.body.clone(),
            })
            .collect()
    };
    let blocked_by = raw
        .blocked_by
        .iter()
        .map(|dep| IssueDependency {
            issue: relative_blocker_name(
                repository,
                &format!("{}/{}", dep.owner, dep.repo),
                dep.number,
            ),
            state: parse_state(&dep.state),
        })
        .collect();
    let converted = convert_issue(repository, raw, None);
    IssueDetail {
        repository: repository.to_owned(),
        number: converted.number,
        url: converted.url,
        state: converted.state,
        title: converted.title,
        body: converted.body,
        author: converted.author.map(|actor| actor.login),
        labels: converted.labels,
        blocked_by,
        total_comments,
        comments,
    }
}

fn convert_issue(
    repository: &str,
    raw: &CollapseIssue,
    reopened_at: Option<String>,
) -> GitHubIssue {
    let mut labels: Vec<String> = raw.labels.iter().map(|label| label.name.clone()).collect();
    labels.sort();
    let mut open_blockers = Vec::new();
    let mut closed_blockers = Vec::new();
    for dep in &raw.blocked_by {
        let dep_repo = format!("{}/{}", dep.owner, dep.repo);
        let name = relative_blocker_name(repository, &dep_repo, dep.number);
        match parse_state(&dep.state) {
            IssueState::Open => open_blockers.push(name),
            IssueState::Closed => closed_blockers.push(name),
        }
    }
    let open_blocker_count = u32::try_from(open_blockers.len()).unwrap_or(u32::MAX);
    let url = issue_page(repository, raw.number)
        .unwrap_or_else(|| format!("https://collapse.sh/{repository}/issues/{}", raw.number));
    let author = (!raw.author.name.trim().is_empty()).then(|| GitHubActor {
        login: raw.author.name.clone(),
    });
    GitHubIssue {
        id: format!("collapse:{repository}#{}", raw.number),
        number: raw.number,
        title: raw.title.clone(),
        body: raw.body.clone(),
        url,
        state: parse_state(&raw.state),
        author,
        created_at: raw.created_at.clone(),
        updated_at: raw.updated_at.clone(),
        closed_at: raw.closed_at.clone(),
        reopened_at,
        labels,
        scout: scout_attribution(&raw.body),
        milestone: None,
        open_blocker_count,
        open_blockers,
        closed_blockers,
        host: IssueHost::Collapse,
    }
}

fn parse_state(raw: &str) -> IssueState {
    if raw.eq_ignore_ascii_case("closed") {
        IssueState::Closed
    } else {
        IssueState::Open
    }
}

fn comment_author(name: &str) -> String {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        "collapse".to_owned()
    } else {
        trimmed.to_owned()
    }
}

fn upsert_comment(items: &mut Vec<CollapseComment>, comment: CollapseComment) {
    if let Some(existing) = items.iter_mut().find(|item| item.id == comment.id) {
        *existing = comment;
    } else {
        items.push(comment);
        items.sort_by_key(|item| item.id);
    }
}

fn from_collapse_error(error: CollapseError) -> GitHubCommandError {
    let message = error.to_string();
    if let Some(rest) = message.strip_prefix("HTTP ")
        && let Some((code_text, _)) = rest.split_once(':')
        && let Ok(code) = code_text.trim().parse::<i32>()
    {
        return GitHubCommandError::Command {
            code: Some(code),
            message,
        };
    }
    GitHubCommandError::Invalid(message)
}

fn block_on_worker<F, T>(future: F) -> Result<T, CollapseError>
where
    F: Future<Output = Result<T, CollapseError>> + Send + 'static,
    T: Send + 'static,
{
    let worker = worker_sender()
        .ok_or_else(|| CollapseError::Api("Collapse HTTP worker could not start".into()))?;
    let (tx, rx) = mpsc::sync_channel(1);
    let job: BoxedJob = Box::new(move || {
        Box::pin(async move {
            let result = future.await;
            let _ = tx.send(result);
        })
    });
    worker
        .send(job)
        .map_err(|_| CollapseError::Api("Collapse HTTP worker stopped".into()))?;
    rx.recv()
        .map_err(|_| CollapseError::Api("Collapse HTTP worker dropped reply".into()))?
}

fn worker_sender() -> Option<&'static tokio::sync::mpsc::UnboundedSender<BoxedJob>> {
    WORKER
        .get_or_init(|| {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<BoxedJob>();
            let started = std::thread::Builder::new()
                .name("velocity-collapse-issues".into())
                .spawn(move || {
                    let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                        .enable_all()
                        .build()
                    else {
                        return;
                    };
                    runtime.block_on(async move {
                        while let Some(job) = rx.recv().await {
                            tokio::spawn(job());
                        }
                    });
                });
            started.ok().map(|_| tx)
        })
        .as_ref()
}
