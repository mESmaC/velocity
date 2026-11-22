//! Collapse pages for a project whose Rosen mirror is a Collapse repository (#429).
//!
//! A land's page is immutable on Collapse, so it is the link to share. Only a
//! recognised Collapse remote with a plain `owner/repository` path produces a page, so
//! an arbitrary or credential-bearing mirror URL never reaches the browser.

use std::time::Duration;

/// How often Collapse's dashboard is polled during the active window after a land (#572).
pub const DASHBOARD_POLL_INTERVAL: Duration = Duration::from_secs(60);

/// How long after the last land Velocity continues polling Collapse's dashboard (#572).
pub const DASHBOARD_POST_LAND_WINDOW: Duration = Duration::from_secs(5 * 60);

/// Next sleep before reading Collapse's dashboard again, or `None` once the post-land
/// window has elapsed and polling should stop until the next land (#572).
pub fn dashboard_poll_delay(since_last_land: Option<Duration>) -> Option<Duration> {
    let elapsed = since_last_land?;
    // Stop once the next read would fall at or past the 5-minute boundary, leaving a
    // full 5-minute quiet period so Collapse's server stops within 10 minutes of a land.
    (elapsed.saturating_add(DASHBOARD_POLL_INTERVAL) < DASHBOARD_POST_LAND_WINDOW)
        .then_some(DASHBOARD_POLL_INTERVAL)
}

/// Whether `since_last_land` is still inside the 5-minute post-land dashboard window (#572).
pub fn dashboard_poll_active(since_last_land: Option<Duration>) -> bool {
    since_last_land.is_some_and(|elapsed| elapsed < DASHBOARD_POST_LAND_WINDOW)
}

/// Hosts a Collapse mirror remote can name. New projects push to the API host (F2).
const REMOTE_HOSTS: [&str; 3] = [
    "https://collapse.sh/",
    "https://git.collapse.sh/",
    "https://api.collapse.sh/",
];

const PAGE_HOST: &str = "https://collapse.sh/";

/// The `owner/repository` a Collapse mirror remote points at, or `None` for any other remote.
pub fn repository_from_remote(remote: &str) -> Option<String> {
    let trimmed = remote.trim();
    let path = REMOTE_HOSTS
        .iter()
        .find_map(|host| strip_prefix_ignoring_case(trimmed, host))?;
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repository) = path.split_once('/')?;
    (valid_segment(owner) && valid_segment(repository)).then(|| format!("{owner}/{repository}"))
}

/// Parses a Collapse `owner/repository` or recognized Collapse URL into `(owner, name)` (#575).
/// Rejects credentials, query strings, fragments and non-Collapse hosts.
pub fn parse_repository(input: &str) -> Option<(String, String)> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = REMOTE_HOSTS
        .iter()
        .find_map(|host| strip_prefix_ignoring_case(trimmed, host))
        .or_else(|| strip_prefix_ignoring_case(trimmed, "collapse.sh/"))
        .or_else(|| strip_prefix_ignoring_case(trimmed, "api.collapse.sh/"))
        .unwrap_or(trimmed);
    if path.contains([':', '@', '?', '#', '\\']) {
        return None;
    }
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, name) = path.split_once('/')?;
    (valid_segment(owner) && valid_segment(name)).then(|| (owner.to_owned(), name.to_owned()))
}

/// The Collapse API origin (`https://api.collapse.sh`), which serves git remotes, API routes
/// and release asset downloads (#593).
pub const COLLAPSE_API_HOST: &str = "https://api.collapse.sh";

/// The rolling release tag holding Velocity's `latest.json` update manifest (#593).
pub const UPDATER_RELEASE_TAG: &str = "updater";

/// The repository's page, for a value `repository_from_remote` returned.
pub fn repository_page(repository: &str) -> Option<String> {
    let (owner, name) = repository.split_once('/')?;
    (valid_segment(owner) && valid_segment(name)).then(|| format!("{PAGE_HOST}{owner}/{name}"))
}

/// An issue's web page on Collapse (`https://collapse.sh/<owner>/<name>/issues/<number>`) (#589).
pub fn issue_page(repository: &str, number: u32) -> Option<String> {
    (number > 0).then_some(())?;
    Some(format!("{}/issues/{number}", repository_page(repository)?))
}

/// An issue comment's web anchor on Collapse
/// (`https://collapse.sh/<owner>/<name>/issues/<number>#comment-<id>`) (#589).
pub fn issue_comment_page(repository: &str, number: u32, comment_id: u64) -> Option<String> {
    (comment_id > 0).then_some(())?;
    Some(format!(
        "{}#comment-{comment_id}",
        issue_page(repository, number)?
    ))
}

/// Where a project's issues are hosted (#589): GitHub or Collapse (C14).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize,
)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum IssueHost {
    #[default]
    #[serde(rename = "github")]
    GitHub,
    Collapse,
}

/// A release's web page on Collapse (`https://collapse.sh/<owner>/<name>/releases/<tag>`) (#593).
pub fn release_page(repository: &str, tag: &str) -> Option<String> {
    let trimmed = tag.trim();
    if trimmed.is_empty() || !valid_release_segment(trimmed) {
        return None;
    }
    Some(format!(
        "{}/releases/{trimmed}",
        repository_page(repository)?
    ))
}

/// Public download URL for a release asset hosted on Collapse
/// (`https://api.collapse.sh/<owner>/<name>/releases/download/<tag>/<asset>`) (#593).
pub fn release_download_url(repository: &str, tag: &str, asset: &str) -> Option<String> {
    let (owner, name) = repository.split_once('/')?;
    let tag = tag.trim();
    let asset = asset.trim();
    if !valid_segment(owner)
        || !valid_segment(name)
        || !valid_release_segment(tag)
        || !valid_release_segment(asset)
    {
        return None;
    }
    Some(format!(
        "{COLLAPSE_API_HOST}/{owner}/{name}/releases/download/{tag}/{asset}"
    ))
}

/// Public Collapse URL for the rolling `updater/latest.json` manifest (#593).
pub fn updater_manifest_url(repository: &str) -> Option<String> {
    release_download_url(repository, UPDATER_RELEASE_TAG, "latest.json")
}

/// Whether `url` points to a Collapse host (`api.collapse.sh` or `collapse.sh`) (#593).
pub fn is_collapse_download_url(url: &str) -> bool {
    let trimmed = url.trim();
    strip_prefix_ignoring_case(trimmed, "https://api.collapse.sh/").is_some()
        || strip_prefix_ignoring_case(trimmed, "https://collapse.sh/").is_some()
}

fn valid_release_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && !segment.contains(['/', '\\', '?', '#', '\r', '\n', '\0'])
        && !segment.chars().any(char::is_whitespace)
}

/// A parsed `velocity://collapse/...` link (#479). Only `<owner>/<name>` and optional
/// `/land/<commit>` parse; unknown paths, query parameters and fragments are ignored so
/// arbitrary link text never reaches a prompt or a shell.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct CollapseDeepLink {
    /// `<owner>/<name>` matching `ProjectItem::collapse_repository`.
    pub repository: String,
    /// Lowercase hex commit from `/land/<commit>`, when the link named a land.
    pub commit: Option<String>,
}

/// Parses a `velocity://collapse/<owner>/<name>` or
/// `velocity://collapse/<owner>/<name>/land/<commit>` deep link (#479).
/// Unknown paths, query parameters and fragments are ignored; invalid segments return `None`.
pub fn parse_deep_link(raw: &str) -> Option<CollapseDeepLink> {
    let after_scheme = strip_prefix_ignoring_case(raw.trim(), "velocity://")?;
    let without_fragment = after_scheme.split('#').next()?;
    let without_query = without_fragment.split('?').next()?;
    let path = without_query.strip_suffix('/').unwrap_or(without_query);
    let mut segments = path.split('/');
    let host = segments.next()?;
    if !host.eq_ignore_ascii_case("collapse") {
        return None;
    }
    let owner = segments.next()?;
    let name = segments.next()?;
    if !valid_segment(owner) || !valid_segment(name) {
        return None;
    }
    let commit = match (segments.next(), segments.next(), segments.next()) {
        (None, None, None) => None,
        (Some("land"), Some(commit), None) if is_commit(commit) => {
            Some(commit.to_ascii_lowercase())
        }
        _ => return None,
    };
    Some(CollapseDeepLink {
        repository: format!("{owner}/{name}"),
        commit,
    })
}

/// The draft context label `"Land <short commit>"` for a validated hex commit (#479).
pub fn land_context_label(commit: &str) -> Option<String> {
    if !is_commit(commit) {
        return None;
    }
    let short = commit.get(..7).unwrap_or(commit).to_ascii_lowercase();
    Some(format!("Land {short}"))
}

/// The notice shown when a `velocity://collapse/<owner>/<name>` link matches no project (#479).
pub fn unmatched_project_notice(repository: &str) -> String {
    format!(
        "No project matches {repository}. Enroll this machine with a join key from the project's owner (Settings → Workspaces → Machines)."
    )
}

/// The notice shown when a Collapse repository already has a linked Rosen server (#575).
pub fn enrollment_notice(repository: &str) -> String {
    format!(
        "{repository} already has a linked Rosen server. Enroll this machine with a join key from the project's owner (Settings → Workspaces → Machines)."
    )
}

/// A land's page (`/<owner>/<name>/land/<commit>`), which Collapse caches as immutable (#573).
pub fn land_page(repository: &str, commit: &str) -> Option<String> {
    if !is_commit(commit) {
        return None;
    }
    Some(format!(
        "{}/land/{}",
        repository_page(repository)?,
        commit.to_ascii_lowercase()
    ))
}

/// The repository's Settings page on Collapse (`https://collapse.sh/<owner>/<name>/settings`) (#610).
pub fn settings_page(repository: &str) -> Option<String> {
    Some(format!("{}/settings", repository_page(repository)?))
}

/// Validates a Collapse-supplied repository settings URL, accepting relative paths under
/// `https://collapse.sh` or HTTPS URLs on `collapse.sh`, and falling back to [`settings_page`] (#610).
pub fn allowed_settings_page(repository: &str, supplied_url: Option<&str>) -> Option<String> {
    supplied_url
        .and_then(allowed_collapse_page)
        .or_else(|| settings_page(repository))
}

fn allowed_collapse_page(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty()
        || trimmed.contains('\\')
        || trimmed
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return None;
    }
    if let Some(rest) = strip_prefix_ignoring_case(trimmed, PAGE_HOST) {
        return (!rest.is_empty()).then(|| format!("{PAGE_HOST}{rest}"));
    }
    if trimmed.starts_with('/') && !trimmed.starts_with("//") {
        let rest = trimmed.trim_start_matches('/');
        return (!rest.is_empty()).then(|| format!("{PAGE_HOST}{rest}"));
    }
    None
}

/// The repository's Settings page on Collapse (`https://collapse.sh/<owner>/<name>/settings`),
/// where the owner can configure the GitHub downstream by hand (C4, #579, #611).
pub fn downstream_settings_page(repository: &str) -> Option<String> {
    settings_page(repository)
}

/// Owner choice when a target Collapse repository name is held by a deleted copy pending purge (#610).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum DeletedRepositoryAction {
    /// Restore the deleted repository on Collapse, then link and fast-forward it from Rosen's base.
    Restore,
    /// Permanently purge the deleted repository on Collapse, then create a fresh copy.
    PurgeAndCreate,
}

/// A Collapse repository name held by a deleted copy pending purge (#610, collapse.sh#217).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct DeletedCollapseRepository {
    /// `<owner>/<name>` on Collapse.
    pub repository: String,
    pub owner: String,
    pub name: String,
    /// RFC 3339 deadline until which Collapse holds the deleted copy, when Collapse reported it.
    #[serde(default)]
    pub delete_after: Option<String>,
    /// RFC 3339 deletion time, when Collapse reported it.
    #[serde(default)]
    pub deleted_at: Option<String>,
    /// `true` when Collapse returned a `deleted_pending` reply (collapse.sh#217);
    /// `false` when inferred from `409` on create followed by `404` on read.
    pub confirmed: bool,
    /// `true` when Collapse supports immediate purge for this repository (collapse.sh#217).
    pub purge_available: bool,
    /// Repository settings page on Collapse (`https://collapse.sh/<owner>/<name>/settings`).
    pub settings_url: String,
}

impl DeletedCollapseRepository {
    /// Human-readable explanation shown in Move to Collapse and project mirror setup (#610).
    pub fn notice(&self) -> String {
        if let Some(deadline) = self
            .delete_after
            .as_deref()
            .map(str::trim)
            .filter(|text| !text.is_empty())
        {
            return format!(
                "{} was deleted on Collapse and is held until {deadline}",
                self.repository
            );
        }
        if self.confirmed {
            format!(
                "{} was deleted on Collapse and is held until its purge deadline",
                self.repository
            )
        } else {
            format!(
                "{} was probably deleted on Collapse (pending purge) and is held until its 7-day purge deadline",
                self.repository
            )
        }
    }
}

/// Prefix for the project health and title bar warning shown while a Collapse-mirrored GitHub
/// project's downstream copy is not being updated (#579, #611).
pub const COLLAPSE_FORWARDING_SIGNAL: &str = "GitHub copy is not being updated";

/// Project health and title bar warning shown when a project's issue host is Collapse but
/// Collapse's GitHub issue import has never finished for that repository (#612).
pub const COLLAPSE_ISSUES_NOT_IMPORTED_SIGNAL: &str =
    "Issues on Collapse were never imported from GitHub";

pub use crate::collapse_issue_refs::{
    RewriteOutcome, SyncReferenceDirection, invert_synced_numbers,
    rewrite_collapse_references_for_github, rewrite_github_references_for_collapse,
    rewrite_synced_issue_references,
};

/// Default duration (14 days) that Velocity keeps mirroring newly opened GitHub issues into
/// Collapse after switching a project's issue host to Collapse (#612).
pub const GITHUB_ISSUE_SYNC_DEFAULT_WINDOW_SECS: u64 = 14 * 24 * 60 * 60;

/// Persisted one-way sync state for mirroring GitHub issues opened or changed since
/// Collapse's import finished until the owner turns the sync off (#612, #614).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitHubIssueSync {
    /// Whether Velocity periodically mirrors GitHub issues opened or changed since the
    /// import into Collapse.
    #[serde(default)]
    pub enabled: bool,
    /// GitHub `owner/repository` watched for opened or changed issues.
    #[serde(default)]
    pub github_repository: String,
    /// Highest GitHub issue number covered by the initial import or already mirrored.
    #[serde(default)]
    pub high_water_number: u32,
    /// RFC 3339 `finished_at` timestamp of Collapse's last completed GitHub import (#614).
    /// Sync covers every GitHub issue opened or changed since this timestamp.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_finished_at: Option<String>,
    /// Maps mirrored GitHub issue numbers to their Collapse issue numbers.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub synced_numbers: std::collections::BTreeMap<u32, u32>,
    /// Maps GitHub issue numbers to the last GitHub `updated_at` timestamp already applied
    /// to Collapse, so unchanged issues are not re-patched on every poll (#614).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub synced_updated_at: std::collections::BTreeMap<u32, String>,
    /// Maps a synced GitHub issue number to same-repository GitHub issue numbers that were
    /// not mapped yet when its Collapse body or comments were written, so Velocity rewrites
    /// them once those issues sync on a later pass (#628).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub pending_reference_rewrites: std::collections::BTreeMap<u32, Vec<u32>>,
    /// Legacy expiry field from #612; retained for deserialization only (#614 keeps sync on
    /// until the owner turns it off).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_secs: Option<u64>,
    /// Unix milliseconds of the last successful poll, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at_ms: Option<u64>,
    /// Last sync error from the most recent poll, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// Serializable status of post-switch one-way GitHub issue sync for Collapse settings (#612, #614).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct GitHubIssueSyncView {
    pub enabled: bool,
    pub github_repository: String,
    pub high_water_number: u32,
    /// RFC 3339 timestamp of the import cutoff (`finished_at`) when known (#614).
    #[serde(default)]
    pub import_finished_at: Option<String>,
    pub synced_count: u32,
    /// Legacy expiry field, always `None` once #614 keeps sync on until turned off.
    #[serde(default)]
    pub expires_at_ms: Option<String>,
    /// Decimal Unix milliseconds when the last sync pass completed, if any.
    #[serde(default)]
    pub last_synced_at_ms: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
}

impl GitHubIssueSync {
    pub fn to_view(&self) -> GitHubIssueSyncView {
        GitHubIssueSyncView {
            enabled: self.enabled,
            github_repository: self.github_repository.clone(),
            high_water_number: self.high_water_number,
            import_finished_at: self.import_finished_at.clone(),
            synced_count: u32::try_from(self.synced_numbers.len()).unwrap_or(u32::MAX),
            expires_at_ms: None,
            last_synced_at_ms: self.last_synced_at_ms.map(|ms| ms.to_string()),
            last_error: self.last_error.clone(),
        }
    }
}

/// Whether Collapse's issue import status shows that no import has ever finished (#612).
pub fn collapse_import_never_finished(
    running: bool,
    last_synced_at: Option<&str>,
    has_result: bool,
) -> bool {
    !running && !has_result && last_synced_at.is_none_or(|timestamp| timestamp.trim().is_empty())
}

/// Whether a project should show the "Issues on Collapse were never imported from GitHub"
/// warning in project health and the title bar (#612).
///
/// Always warns when the host is Collapse, the Collapse import never finished, and either
/// a GitHub repository is known or Collapse currently holds 0 (or unknown) issues.
pub fn collapse_issues_import_warning_needed(
    issue_host: IssueHost,
    has_collapse_repository: bool,
    import_never_finished: bool,
    has_github_source: bool,
    collapse_issue_count: Option<u64>,
) -> bool {
    if issue_host != IssueHost::Collapse || !has_collapse_repository || !import_never_finished {
        return false;
    }
    has_github_source || collapse_issue_count.is_none_or(|count| count == 0)
}

/// Whether Move to Collapse may advance to the mirror switch step (#612).
/// Only an explicitly passed import step allows cut-over; skipped and failed imports stop.
pub fn move_to_collapse_can_switch_mirror(import_status: MoveToCollapseStepStatus) -> bool {
    import_status == MoveToCollapseStepStatus::Passed
}

/// Whether Move to Collapse Step 1 should run `POST .../issues/import` or skip it (#615).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportStepDecision {
    /// Import already finished; skip `POST .../issues/import` so Collapse issue states stay untouched.
    AlreadyDone,
    /// Run the GitHub issue import (`POST .../issues/import`).
    RunImport,
}

/// Decides whether Move to Collapse Step 1 should run the GitHub issue import (#615).
///
/// When Collapse already recorded a finished import (or Velocity's local state recorded one
/// and the remote status could not be checked), Move to Collapse skips Step 1 so re-running
/// cut-over never overwrites or reopens Collapse issues. Only an explicit `force_reimport`
/// ("Import again") runs the import again.
pub fn plan_move_to_collapse_import(
    remote_import_finished: Option<bool>,
    local_import_finished: bool,
    force_reimport: bool,
) -> ImportStepDecision {
    if force_reimport {
        return ImportStepDecision::RunImport;
    }
    match remote_import_finished {
        Some(true) => ImportStepDecision::AlreadyDone,
        Some(false) => ImportStepDecision::RunImport,
        None if local_import_finished => ImportStepDecision::AlreadyDone,
        None => ImportStepDecision::RunImport,
    }
}

/// Formats Step 1 of Move to Collapse when Collapse's GitHub issue import already finished (#615).
pub fn already_done_import_step(
    github_repository: Option<&str>,
    finished_at: Option<&str>,
) -> MoveToCollapseStepResult {
    let source = github_repository
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let finished = finished_at.map(str::trim).filter(|value| !value.is_empty());
    let summary = match (source, finished) {
        (Some(github), Some(at)) => {
            format!("Already done — issues from {github} were imported ({at}); skipped re-import.")
        }
        (Some(github), None) => {
            format!("Already done — issues from {github} were already imported; skipped re-import.")
        }
        (None, Some(at)) => {
            format!("Already done — Collapse issue import finished ({at}); skipped re-import.")
        }
        (None, None) => {
            "Already done — Collapse issue import already finished; skipped re-import.".to_owned()
        }
    };
    MoveToCollapseStepResult {
        step: MoveToCollapseStepId::ImportIssues,
        title: "Import GitHub issues".to_owned(),
        status: MoveToCollapseStepStatus::Passed,
        summary,
        detail: Some(
            "Skipped re-import so Collapse issue states are not overwritten. Use \"Import again\" only if you want to overwrite Collapse issues from GitHub."
                .to_owned(),
        ),
        action_url: None,
        action_label: None,
    }
}

/// Hidden idempotency marker appended to GitHub issues mirrored one-way into Collapse (#612).
pub fn github_sync_marker(github_repository: &str, number: u32) -> String {
    format!("<!-- velocity-github-sync: {github_repository}#{number} -->")
}

/// Hidden idempotency marker appended to GitHub issue comments mirrored into Collapse (#628).
pub fn github_comment_sync_marker(
    github_repository: &str,
    issue_number: u32,
    comment_id: &str,
) -> String {
    format!(
        "<!-- velocity-github-comment-sync: {github_repository}#{issue_number}:{comment_id} -->"
    )
}

/// Formats the body of a newly opened GitHub issue mirrored one-way into Collapse (#612).
pub fn format_synced_github_issue_body(
    original_body: &str,
    github_repository: &str,
    number: u32,
) -> String {
    let marker = github_sync_marker(github_repository, number);
    let footer = format!(
        "---\n_Synced from GitHub: https://github.com/{github_repository}/issues/{number}_\n{marker}"
    );
    let trimmed = original_body.trim_end();
    if trimmed.is_empty() {
        footer
    } else {
        format!("{trimmed}\n\n{footer}")
    }
}

/// Formats a GitHub issue comment mirrored into Collapse, keeping the comment body first so
/// owner-comment blocker detection stays intact (#628).
pub fn format_synced_github_comment_body(
    original_body: &str,
    author: Option<&str>,
    github_repository: &str,
    issue_number: u32,
    comment_id: &str,
    comment_url: Option<&str>,
) -> String {
    let marker = github_comment_sync_marker(github_repository, issue_number, comment_id);
    let source_url = comment_url
        .map(str::trim)
        .filter(|url| !url.is_empty())
        .map(str::to_owned)
        .unwrap_or_else(|| format!("https://github.com/{github_repository}/issues/{issue_number}"));
    let attribution = match author.map(str::trim).filter(|name| !name.is_empty()) {
        Some(login) => format!("_Synced from GitHub comment by @{login}: {source_url}_"),
        None => format!("_Synced from GitHub comment: {source_url}_"),
    };
    let footer = format!("---\n{attribution}\n{marker}");
    let trimmed = original_body.trim_end();
    if trimmed.is_empty() {
        footer
    } else {
        format!("{trimmed}\n\n{footer}")
    }
}

/// Synced GitHub issue numbers in `sync.pending_reference_rewrites` that now have at least
/// one previously unmapped reference present in `sync.synced_numbers` (#628).
pub fn ready_reference_rewrite_numbers(sync: &GitHubIssueSync) -> Vec<u32> {
    sync.pending_reference_rewrites
        .iter()
        .filter(|(_, unresolved)| {
            unresolved
                .iter()
                .any(|number| sync.synced_numbers.contains_key(number))
        })
        .map(|(&github_number, _)| github_number)
        .collect()
}

/// Whether a project's GitHub-to-Collapse issue sync has expired. Sync stays on until
/// the owner turns it off (#614).
pub fn github_sync_expired(_sync: &GitHubIssueSync, _now_secs: u64) -> bool {
    false
}

/// Whether GitHub issue `#number` should be mirrored one-way into Collapse based on its
/// number alone (#612, #614).
pub fn should_sync_github_issue(sync: &GitHubIssueSync, number: u32, _now_secs: u64) -> bool {
    sync.enabled
        && !sync.github_repository.trim().is_empty()
        && number > sync.high_water_number
        && !sync.synced_numbers.contains_key(&number)
}

/// Whether GitHub issue `#number` (opened at `created_at`, updated at `updated_at`) should
/// be synced into Collapse: covers every GitHub issue opened or changed since Collapse's
/// import `finished_at` (as well as any issue above `high_water_number`), skipping exact
/// `updated_at` versions already applied (#614).
pub fn should_sync_github_issue_change(
    sync: &GitHubIssueSync,
    number: u32,
    created_at: &str,
    updated_at: &str,
) -> bool {
    if !sync.enabled || sync.github_repository.trim().is_empty() {
        return false;
    }
    let updated_trimmed = updated_at.trim();
    if !updated_trimmed.is_empty()
        && sync
            .synced_updated_at
            .get(&number)
            .is_some_and(|seen| seen == updated_trimmed)
    {
        return false;
    }
    if number > sync.high_water_number && !sync.synced_numbers.contains_key(&number) {
        return true;
    }
    let Some(since) = sync
        .import_finished_at
        .as_deref()
        .and_then(parse_rfc3339_seconds)
    else {
        return number > sync.high_water_number;
    };
    let created_since = parse_rfc3339_seconds(created_at).is_some_and(|ts| ts >= since);
    let updated_since = parse_rfc3339_seconds(updated_at).is_some_and(|ts| ts >= since);
    created_since || updated_since
}

/// Parses an RFC 3339 timestamp or decimal Unix-seconds string into UTC Unix seconds (#614).
pub fn parse_rfc3339_seconds(value: &str) -> Option<u64> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed.bytes().all(|byte| byte.is_ascii_digit()) {
        return trimmed.parse::<u64>().ok();
    }
    let (date, time_and_zone) = trimmed.split_once(['T', 't'])?;
    let mut date_parts = date.split('-').map(|part| part.parse::<i64>().ok());
    let (year, month, day) = (
        date_parts.next()??,
        date_parts.next()??,
        date_parts.next()??,
    );
    if date_parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (time_part, offset_seconds) = split_rfc3339_offset(time_and_zone)?;
    let clock = time_part.split('.').next()?;
    let mut clock_parts = clock.split(':').map(|part| part.parse::<i64>().ok());
    let (hour, minute, second) = (
        clock_parts.next()??,
        clock_parts.next()??,
        clock_parts.next()??,
    );
    if clock_parts.next().is_some()
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }
    let adjusted_year = if month <= 2 { year - 1 } else { year };
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let year_of_era = adjusted_year.rem_euclid(400);
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = adjusted_year.div_euclid(400) * 146_097 + day_of_era - 719_468;
    let utc_seconds = days * 86_400 + hour * 3_600 + minute * 60 + second - offset_seconds;
    u64::try_from(utc_seconds).ok()
}

fn split_rfc3339_offset(time_and_zone: &str) -> Option<(&str, i64)> {
    if let Some(clock) = time_and_zone
        .strip_suffix('Z')
        .or_else(|| time_and_zone.strip_suffix('z'))
    {
        return Some((clock, 0));
    }
    let sign_index = time_and_zone.rfind(['+', '-'])?;
    let (clock, zone) = time_and_zone.split_at(sign_index);
    let sign = if zone.starts_with('+') { 1 } else { -1 };
    let (hours, minutes) = zone[1..].split_once(':')?;
    let hours = hours.parse::<i64>().ok()?;
    let minutes = minutes.parse::<i64>().ok()?;
    if !(0..=23).contains(&hours) || !(0..=59).contains(&minutes) {
        return None;
    }
    Some((clock, sign * (hours * 3_600 + minutes * 60)))
}

/// Expected identity of a landed change's issue, verified before commenting, closing, or
/// treating a host issue as already completed (#614).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ExpectedIssueIdentity<'a> {
    pub title: Option<&'a str>,
    pub github_repository: Option<&'a str>,
    pub github_number: Option<u32>,
}

impl ExpectedIssueIdentity<'_> {
    pub fn has_expected_title(&self) -> bool {
        self.title.is_some_and(|title| !title.trim().is_empty())
    }
}

/// Whether a host issue (`host_title`, `host_body`) matches `expected` (#614).
///
/// Accepts the issue when its body links or carries the Velocity sync marker for the
/// expected GitHub source, or when its title matches (exact normalized, truncated prefix,
/// or high token overlap). Legacy records with no expected title match by default.
pub fn host_issue_matches_expected(
    expected: &ExpectedIssueIdentity<'_>,
    host_title: &str,
    host_body: &str,
) -> bool {
    if let (Some(repo), Some(number)) = (
        expected
            .github_repository
            .map(str::trim)
            .filter(|repo| !repo.is_empty()),
        expected.github_number.filter(|&number| number > 0),
    ) && body_references_github_issue(host_body, repo, number)
    {
        return true;
    }
    let Some(expected_title) = expected
        .title
        .map(str::trim)
        .filter(|title| !title.is_empty())
    else {
        return true;
    };
    titles_match_issue(expected_title, host_title)
}

/// Whether `body` carries the Velocity sync marker or GitHub issue URL for `repo#number` (#614).
pub fn body_references_github_issue(body: &str, repo: &str, number: u32) -> bool {
    let lower_body = body.to_ascii_lowercase();
    let lower_repo = repo.trim().to_ascii_lowercase();
    let marker = format!("<!-- velocity-github-sync: {lower_repo}#{number} -->");
    if lower_body.contains(&marker) {
        return true;
    }
    let needle = format!("https://github.com/{lower_repo}/issues/{number}");
    let mut start = 0;
    while let Some(offset) = lower_body[start..].find(&needle) {
        let end = start + offset + needle.len();
        let next_is_digit = lower_body
            .as_bytes()
            .get(end)
            .is_some_and(u8::is_ascii_digit);
        if !next_is_digit {
            return true;
        }
        start = end;
    }
    false
}

fn titles_match_issue(expected: &str, actual: &str) -> bool {
    let left = normalize_issue_title(expected);
    let right = normalize_issue_title(actual);
    if left.is_empty() || right.is_empty() {
        return true;
    }
    if left == right {
        return true;
    }
    if is_truncated_prefix(expected, &left, &right) || is_truncated_prefix(actual, &right, &left) {
        return true;
    }
    let left_words = title_words(&left);
    let right_words = title_words(&right);
    if left_words.len() < 3 || right_words.len() < 3 {
        return false;
    }
    let shared = left_words
        .iter()
        .filter(|word| right_words.contains(*word))
        .count();
    let union = left_words.len() + right_words.len() - shared;
    union > 0 && shared.saturating_mul(2) >= union
}

fn is_truncated_prefix(raw: &str, normalized: &str, other: &str) -> bool {
    let trimmed = raw.trim_end();
    (trimmed.ends_with('…') || trimmed.ends_with("..."))
        && normalized.len() >= 12
        && other.starts_with(normalized)
}

fn normalize_issue_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut previous_space = true;
    for ch in title.chars() {
        if ch.is_alphanumeric() {
            for lower in ch.to_lowercase() {
                out.push(lower);
            }
            previous_space = false;
        } else if !previous_space {
            out.push(' ');
            previous_space = true;
        }
    }
    out.trim_end().to_owned()
}

fn title_words(normalized: &str) -> std::collections::BTreeSet<&str> {
    normalized
        .split_whitespace()
        .filter(|word| word.len() >= 2)
        .collect()
}

/// Formats the title bar and project health warning for a missing or failing GitHub downstream
/// copy: `"GitHub copy is not being updated: <reason>"` (#611).
pub fn collapse_forwarding_signal(reason: &str) -> String {
    let trimmed = reason.trim();
    if trimmed.is_empty() {
        format!("{COLLAPSE_FORWARDING_SIGNAL}: Collapse downstream forwarding is not configured")
    } else {
        format!("{COLLAPSE_FORWARDING_SIGNAL}: {trimmed}")
    }
}

/// Hosts accepted for a Collapse-supplied GitHub App install or permission page (#579).
const DOWNSTREAM_SETUP_HOSTS: [&str; 2] = ["https://github.com/", "https://collapse.sh/"];

/// Validates a GitHub App install or permission page URL returned by Collapse (#579).
/// Accepts only HTTPS pages on `github.com` or `collapse.sh`, rejecting whitespace,
/// control characters, backslashes, credentials and other hosts.
pub fn allowed_downstream_setup_page(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty()
        || trimmed.contains('\\')
        || trimmed
            .chars()
            .any(|character| character.is_control() || character.is_whitespace())
    {
        return None;
    }
    for host in DOWNSTREAM_SETUP_HOSTS {
        if let Some(rest) = strip_prefix_ignoring_case(trimmed, host) {
            return (!rest.is_empty()).then(|| format!("{host}{rest}"));
        }
    }
    None
}

/// Resolves the browser page to open when Collapse's downstream API needs GitHub App
/// installation or permissions: prefers Collapse's validated page and falls back to the
/// repository's Downstream settings section (#579).
pub fn downstream_setup_page(
    collapse_repository: &str,
    supplied_url: Option<&str>,
) -> Option<String> {
    supplied_url
        .and_then(allowed_downstream_setup_page)
        .or_else(|| downstream_settings_page(collapse_repository))
}

/// Whether a Collapse-mirrored project should show the "GitHub copy is not being updated"
/// title bar and project health warning (#579, #611). Clears once Collapse's forwarder
/// reports `Synced` or when the project no longer mirrors to Collapse or has no GitHub
/// repository.
pub fn collapse_forwarding_needs_attention(
    has_collapse_repository: bool,
    has_github_remote: bool,
    forwarder: Option<ForwarderState>,
    setup_incomplete: bool,
) -> bool {
    if !has_collapse_repository || !has_github_remote {
        return false;
    }
    match forwarder {
        Some(ForwarderState::Synced) => false,
        Some(ForwarderState::Unconfigured | ForwarderState::Stopped) => true,
        _ => setup_incomplete,
    }
}

/// The Collapse git remote Rosen mirrors to for `owner/repository` (Collapse C3).
pub fn mirror_remote(repository: &str) -> Option<String> {
    let (owner, name) = repository.split_once('/')?;
    (valid_segment(owner) && valid_segment(name))
        .then(|| format!("https://api.collapse.sh/{owner}/{name}.git"))
}

/// The Collapse root `rosen.toml`'s `collapse` key names for `owner/repository` (#517).
/// Rosen sends its state feed to `<root>/rosen/state` and ledger entries to
/// `<root>/ledgers`, and reads the owner's relayed actions under it (Rosen R9, R13, R14).
pub fn collapse_root(repository: &str) -> Option<String> {
    let (owner, name) = repository.split_once('/')?;
    (valid_segment(owner) && valid_segment(name))
        .then(|| format!("https://api.collapse.sh/{owner}/{name}"))
}

/// A Collapse repository name for a project folder or project name (#427). Collapse takes
/// GitHub's rules: letters, digits, `-`, `_` and `.`, at most 100 characters. Other
/// characters become `-`, repeats collapse, and the ends are trimmed of separators.
pub fn repository_name_for(project: &str) -> Option<String> {
    let mut name = String::new();
    for character in project.trim().chars() {
        let kept = character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.');
        let next = if kept { character } else { '-' };
        if next == '-' && name.ends_with('-') {
            continue;
        }
        name.push(next);
    }
    let name: String = name
        .trim_matches(['-', '.', '_'])
        .chars()
        .take(100)
        .collect();
    let name = name.trim_end_matches(['-', '.', '_']);
    valid_segment(name).then(|| name.to_owned())
}

/// What Collapse's forwarder is doing with the repository's downstream remote (C4).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub enum ForwarderState {
    /// No downstream remote is configured on Collapse.
    Unconfigured,
    Synced,
    CatchingUp,
    Retrying,
    /// A rejected forward waits for the owner's Retry on Collapse.
    Stopped,
    /// Collapse could not read the forward state.
    Unavailable,
}

impl ForwarderState {
    /// Reads Collapse's forwarder state (`forward.state` or `GET /api/downstream` `state.status`);
    /// anything else is unavailable.
    pub fn from_collapse(state: &str) -> Self {
        match state {
            "unconfigured" => Self::Unconfigured,
            "synced" | "in_sync" => Self::Synced,
            "catching_up" => Self::CatchingUp,
            "retrying" => Self::Retrying,
            "stopped" => Self::Stopped,
            _ => Self::Unavailable,
        }
    }
}

/// The GitHub `owner/repository` a GitHub remote URL or `owner/repository` names (#574).
/// Rejects credentials, query strings, fragments and non-GitHub hosts.
pub fn github_repository_from_remote(remote: &str) -> Option<String> {
    let trimmed = remote.trim();
    if trimmed.is_empty() {
        return None;
    }
    let path = strip_prefix_ignoring_case(trimmed, "https://github.com/")
        .or_else(|| strip_prefix_ignoring_case(trimmed, "http://github.com/"))
        .or_else(|| strip_prefix_ignoring_case(trimmed, "ssh://git@github.com/"))
        .or_else(|| strip_prefix_ignoring_case(trimmed, "git@github.com:"))
        .or_else(|| strip_prefix_ignoring_case(trimmed, "github.com/"))
        .unwrap_or(trimmed);
    if path.contains([':', '@', '?', '#', '\\']) {
        return None;
    }
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repository) = path.split_once('/')?;
    (valid_github_owner(owner) && valid_github_repository(repository))
        .then(|| format!("{owner}/{repository}"))
}

/// Action kinds Rosen's Collapse relay can run for a project (Rosen R14, #576).
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum CollapseRelayKind {
    Approve,
    Land,
    Mark,
    Config,
    Revoke,
}

impl CollapseRelayKind {
    pub const ALL: [Self; 5] = [
        Self::Approve,
        Self::Land,
        Self::Mark,
        Self::Config,
        Self::Revoke,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Approve => "approve",
            Self::Land => "land",
            Self::Mark => "mark",
            Self::Config => "config",
            Self::Revoke => "revoke",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == text)
    }

    /// Sorts and deduplicates relay kinds into canonical order.
    pub fn normalize(kinds: impl IntoIterator<Item = Self>) -> Vec<Self> {
        let mut normalized: Vec<Self> = kinds.into_iter().collect();
        normalized.sort();
        normalized.dedup();
        normalized
    }
}

/// Last delivery time and failure for a Rosen push stream to Collapse (#576).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct CollapseFeedHealth {
    /// Unix milliseconds when the newest acknowledged event or ledger entry was recorded.
    pub last_sent_at: Option<String>,
    /// Why the latest pass failed or stopped while nothing newer arrived.
    pub last_error: Option<String>,
}

/// Whether git's credential helper holds a push token for Collapse's feed endpoints (#576).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PushTokenStatus {
    /// No Collapse root or mirror is configured yet.
    NotConfigured,
    /// Both `<root>/rosen/state` and `<root>/ledgers` hold a push token.
    Present,
    /// Only one of the two feed endpoints holds a push token.
    Partial { state_feed: bool, ledger_push: bool },
    /// Neither feed endpoint holds a push token.
    Missing,
    /// Could not query git's credential helper.
    Unavailable { error: String },
}

/// The project's Collapse mirror as Collapse reports it: its last land and the forwarder
/// to the downstream remote (#427). Read with the personal token; holds no credential.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct CollapseMirrorHealth {
    pub forwarder: ForwarderState,
    /// Lands the downstream is behind while catching up.
    pub behind: u32,
    /// The forwarder's last error, such as git's rejection while stopped.
    pub message: Option<String>,
    /// RFC 3339 time of the next forward attempt while retrying.
    pub next_attempt: Option<String>,
    /// RFC 3339 time Collapse received the latest land, and its `<nick>@<machine>` agent.
    pub last_land_at: Option<String>,
    pub last_land_agent: Option<String>,
    /// The repository is archived on Collapse, which refuses mirror pushes.
    pub archived: bool,
    /// GitHub `owner/repository` Collapse forwards to when its downstream remote is on
    /// GitHub, suggested in project settings (#574).
    #[serde(default)]
    pub downstream_github: Option<String>,
}

/// Outcome of one probe inside `rosen collapse check` (#592).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum CollapseCheckItemStatus {
    Pass,
    Moved,
    Fail,
    Skipped,
}

/// One probe row returned by `rosen collapse check` (#592).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct CollapseCheckItemResult {
    pub name: String,
    pub status: CollapseCheckItemStatus,
    pub detail: String,
    #[serde(default)]
    pub hint: Option<String>,
}

/// Serializable summary of `rosen collapse check` for project settings (#592).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct CollapseCheckSummary {
    pub ok: bool,
    #[serde(default)]
    pub mirror_remote: Option<String>,
    #[serde(default)]
    pub collapse: Option<String>,
    #[serde(default)]
    pub ledger_endpoint: Option<String>,
    pub mirror_owner: bool,
    pub mirror_proactive_auth: bool,
    pub checks: Vec<CollapseCheckItemResult>,
}

/// Identifier for each step in the Move to Collapse cut-over (#592).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum MoveToCollapseStepId {
    ImportIssues,
    SwitchMirror,
    SetIssuesAndCi,
    ConfigureDownstream,
    CollapseCheck,
}

/// Result status of one Move to Collapse cut-over step (#592).
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
#[serde(rename_all = "snake_case")]
pub enum MoveToCollapseStepStatus {
    Passed,
    Warning,
    Skipped,
    Failed,
}

/// Reported outcome for one step of Move to Collapse (#592).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct MoveToCollapseStepResult {
    pub step: MoveToCollapseStepId,
    pub title: String,
    pub status: MoveToCollapseStepStatus,
    pub summary: String,
    #[serde(default)]
    pub detail: Option<String>,
    #[serde(default)]
    pub action_url: Option<String>,
    #[serde(default)]
    pub action_label: Option<String>,
}

fn strip_prefix_ignoring_case<'text>(text: &'text str, prefix: &str) -> Option<&'text str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

/// A path segment Collapse accepts as an owner or repository name.
fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment != "."
        && segment != ".."
        && segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_github_owner(owner: &str) -> bool {
    !owner.is_empty()
        && owner.len() <= 39
        && !owner.starts_with('-')
        && !owner.ends_with('-')
        && !owner.contains("--")
        && owner
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

fn valid_github_repository(repository: &str) -> bool {
    !repository.is_empty()
        && repository.len() <= 100
        && repository != "."
        && repository != ".."
        && repository
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn is_commit(commit: &str) -> bool {
    (7..=64).contains(&commit.len()) && commit.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::{enrollment_notice, github_repository_from_remote, land_page, parse_repository};

    /// Risk (#575): Open from Collapse must accept plain `owner/name` and Collapse URLs
    /// while rejecting credentials, query strings and non-Collapse hosts.
    #[test]
    fn parse_repository_accepts_collapse_names_and_urls() {
        assert_eq!(
            parse_repository("mESmaC/velocity"),
            Some(("mESmaC".into(), "velocity".into()))
        );
        assert_eq!(
            parse_repository("https://collapse.sh/mESmaC/velocity/"),
            Some(("mESmaC".into(), "velocity".into()))
        );
        assert_eq!(
            parse_repository("https://api.collapse.sh/mESmaC/velocity.git"),
            Some(("mESmaC".into(), "velocity".into()))
        );
        assert_eq!(parse_repository("https://github.com/mESmaC/velocity"), None);
        assert_eq!(
            parse_repository("https://token@api.collapse.sh/mESmaC/velocity.git"),
            None
        );
        assert_eq!(parse_repository("mESmaC/velocity?x=1"), None);
        assert!(enrollment_notice("mESmaC/velocity").contains("Enroll this machine"));
    }

    #[test]
    fn github_repository_from_remote_accepts_github_urls_and_names() {
        assert_eq!(
            github_repository_from_remote("mESmaC/velocity").as_deref(),
            Some("mESmaC/velocity")
        );
        assert_eq!(
            github_repository_from_remote("https://github.com/mESmaC/velocity.git").as_deref(),
            Some("mESmaC/velocity")
        );
        assert_eq!(
            github_repository_from_remote("git@github.com:mESmaC/collapse.sh.git").as_deref(),
            Some("mESmaC/collapse.sh")
        );
        assert_eq!(
            github_repository_from_remote("ssh://git@github.com/mESmaC/rosen/").as_deref(),
            Some("mESmaC/rosen")
        );
        assert_eq!(
            github_repository_from_remote("https://api.collapse.sh/mESmaC/velocity.git"),
            None
        );
        assert_eq!(
            github_repository_from_remote("https://token@github.com/mESmaC/velocity.git"),
            None
        );
        assert_eq!(
            github_repository_from_remote("https://github.com/mESmaC/velocity?x=1"),
            None
        );
    }

    /// Risk (#573): Collapse serves landed commits at `/<owner>/<name>/land/<commit>`
    /// (Collapse C5), not `/commit/<commit>`, which returns 404.
    #[test]
    fn land_page_uses_land_route() {
        assert_eq!(
            land_page("mESmaC/velocity", "6079AA13df4d"),
            Some("https://collapse.sh/mESmaC/velocity/land/6079aa13df4d".into())
        );
        assert_eq!(land_page("mESmaC/velocity", "short"), None);
        assert_eq!(land_page("mESmaC/velocity", "not-a-hex-sha"), None);
        assert_eq!(land_page("not-a-repo", "6079aa13df4d"), None);
    }

    #[test]
    fn collapse_import_warning_and_cutover_gate_follow_612() {
        use super::{
            GitHubIssueSync, IssueHost, MoveToCollapseStepStatus, collapse_import_never_finished,
            collapse_issues_import_warning_needed, format_synced_github_issue_body,
            github_sync_expired, github_sync_marker, move_to_collapse_can_switch_mirror,
            should_sync_github_issue,
        };

        assert!(collapse_import_never_finished(false, None, false));
        assert!(!collapse_import_never_finished(true, None, false));
        assert!(!collapse_import_never_finished(
            false,
            Some("2026-11-22T00:00:00Z"),
            false
        ));
        assert!(!collapse_import_never_finished(false, None, true));

        assert!(collapse_issues_import_warning_needed(
            IssueHost::Collapse,
            true,
            true,
            true,
            Some(0),
        ));
        assert!(collapse_issues_import_warning_needed(
            IssueHost::Collapse,
            true,
            true,
            false,
            Some(0),
        ));
        assert!(!collapse_issues_import_warning_needed(
            IssueHost::Collapse,
            true,
            true,
            false,
            Some(3),
        ));
        assert!(!collapse_issues_import_warning_needed(
            IssueHost::GitHub,
            true,
            true,
            true,
            Some(0),
        ));
        assert!(!collapse_issues_import_warning_needed(
            IssueHost::Collapse,
            true,
            false,
            true,
            Some(0),
        ));

        assert!(move_to_collapse_can_switch_mirror(
            MoveToCollapseStepStatus::Passed
        ));
        assert!(!move_to_collapse_can_switch_mirror(
            MoveToCollapseStepStatus::Skipped
        ));
        assert!(!move_to_collapse_can_switch_mirror(
            MoveToCollapseStepStatus::Failed
        ));
        assert!(!move_to_collapse_can_switch_mirror(
            MoveToCollapseStepStatus::Warning
        ));

        let mut sync = GitHubIssueSync {
            enabled: true,
            github_repository: "mESmaC/velocity".into(),
            high_water_number: 612,
            import_finished_at: Some("2026-11-22T01:00:00Z".into()),
            expires_at_secs: Some(1_000),
            ..GitHubIssueSync::default()
        };
        assert!(!should_sync_github_issue(&sync, 612, 500));
        assert!(should_sync_github_issue(&sync, 613, 500));
        // Sync stays active until the owner turns it off (#614).
        assert!(should_sync_github_issue(&sync, 613, 1_000));
        assert!(!github_sync_expired(&sync, 1_000));
        sync.synced_numbers.insert(613, 614);
        assert!(!should_sync_github_issue(&sync, 613, 500));

        // Issues opened or updated on GitHub after import_finished_at are covered (#614).
        assert!(super::should_sync_github_issue_change(
            &sync,
            613,
            "2026-11-22T01:05:00Z",
            "2026-11-22T01:05:00Z",
        ));
        assert!(super::should_sync_github_issue_change(
            &sync,
            600,
            "2026-11-20T00:00:00Z",
            "2026-11-22T01:10:00Z",
        ));
        assert!(!super::should_sync_github_issue_change(
            &sync,
            600,
            "2026-11-20T00:00:00Z",
            "2026-11-21T23:59:59Z",
        ));
        sync.synced_updated_at
            .insert(613, "2026-11-22T01:05:00Z".into());
        assert!(!super::should_sync_github_issue_change(
            &sync,
            613,
            "2026-11-22T01:05:00Z",
            "2026-11-22T01:05:00Z",
        ));

        let body = format_synced_github_issue_body("Bug details", "mESmaC/velocity", 613);
        assert!(body.contains("https://github.com/mESmaC/velocity/issues/613"));
        assert!(body.contains(&github_sync_marker("mESmaC/velocity", 613)));
    }

    #[test]
    fn host_issue_identity_check_catches_number_collisions_and_accepts_matches() {
        use super::{
            ExpectedIssueIdentity, format_synced_github_issue_body, host_issue_matches_expected,
        };

        let expected = ExpectedIssueIdentity {
            title: Some(
                "#612 put already-moved projects back on GitHub issues and GitHub CI after restart; keep them on Collapse when their import finished",
            ),
            github_repository: Some("mESmaC/velocity"),
            github_number: Some(613),
        };
        assert!(host_issue_matches_expected(
            &expected,
            "#612 put already-moved projects back on GitHub issues and GitHub CI after restart; keep them on Collapse when their import finished",
            "Imported body"
        ));
        // Colliding Collapse issue with a different title and no GitHub #613 source must mismatch.
        assert!(!host_issue_matches_expected(
            &expected,
            "Velocity retries a missing Collapse issue every 30 s forever, and GitHub and Collapse issue numbers collide after an import",
            "Filed on Collapse; mentions https://github.com/mESmaC/velocity/issues/6130"
        ));
        // Synced issue carrying the GitHub source marker matches even after a title edit.
        let synced_body = format_synced_github_issue_body("Details", "mESmaC/velocity", 613);
        assert!(host_issue_matches_expected(
            &expected,
            "Edited title on Collapse",
            &synced_body
        ));
    }

    #[test]
    fn move_to_collapse_skips_finished_import_unless_forced() {
        use super::{
            ImportStepDecision, MoveToCollapseStepId, MoveToCollapseStepStatus,
            already_done_import_step, move_to_collapse_can_switch_mirror,
            plan_move_to_collapse_import,
        };

        // Re-running Move to Collapse when Collapse reports a finished import skips Step 1.
        assert_eq!(
            plan_move_to_collapse_import(Some(true), false, false),
            ImportStepDecision::AlreadyDone
        );
        // Local finished-import state protects against re-importing when remote GET is unavailable.
        assert_eq!(
            plan_move_to_collapse_import(None, true, false),
            ImportStepDecision::AlreadyDone
        );
        // Fresh or purged Collapse repo whose remote GET reports unfinished runs import.
        assert_eq!(
            plan_move_to_collapse_import(Some(false), true, false),
            ImportStepDecision::RunImport
        );
        assert_eq!(
            plan_move_to_collapse_import(None, false, false),
            ImportStepDecision::RunImport
        );
        // Explicit "Import again" always runs the import.
        assert_eq!(
            plan_move_to_collapse_import(Some(true), true, true),
            ImportStepDecision::RunImport
        );

        let step = already_done_import_step(Some("mESmaC/velocity"), Some("2026-11-22T10:00:00Z"));
        assert_eq!(step.step, MoveToCollapseStepId::ImportIssues);
        assert_eq!(step.status, MoveToCollapseStepStatus::Passed);
        assert!(move_to_collapse_can_switch_mirror(step.status));
        assert!(step.summary.contains("Already done"));
        assert!(step.summary.contains("skipped re-import"));
        assert!(
            step.detail
                .as_deref()
                .is_some_and(|detail| detail.contains("Import again"))
        );
    }
}
