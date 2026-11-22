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

/// Default duration (14 days) that Velocity keeps mirroring newly opened GitHub issues into
/// Collapse after switching a project's issue host to Collapse (#612).
pub const GITHUB_ISSUE_SYNC_DEFAULT_WINDOW_SECS: u64 = 14 * 24 * 60 * 60;

/// Persisted one-way sync state for mirroring GitHub issues opened after switching a
/// project's issue host to Collapse (#612).
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GitHubIssueSync {
    /// Whether Velocity periodically mirrors newly opened GitHub issues to Collapse.
    #[serde(default)]
    pub enabled: bool,
    /// GitHub `owner/repository` watched for newly opened issues.
    #[serde(default)]
    pub github_repository: String,
    /// Highest GitHub issue number covered by the initial import or already mirrored.
    #[serde(default)]
    pub high_water_number: u32,
    /// Maps mirrored GitHub issue numbers to their created Collapse issue numbers.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub synced_numbers: std::collections::BTreeMap<u32, u32>,
    /// Optional Unix seconds when this transitional sync automatically turns off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at_secs: Option<u64>,
    /// Unix milliseconds of the last successful poll, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at_ms: Option<u64>,
    /// Last sync error from the most recent poll, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// Serializable status of post-switch one-way GitHub issue sync for Collapse settings (#612).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[cfg_attr(feature = "bindings", derive(specta::Type))]
pub struct GitHubIssueSyncView {
    pub enabled: bool,
    pub github_repository: String,
    pub high_water_number: u32,
    pub synced_count: u32,
    /// Decimal Unix milliseconds when transitional sync expires, if a window is set.
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
            synced_count: u32::try_from(self.synced_numbers.len()).unwrap_or(u32::MAX),
            expires_at_ms: self
                .expires_at_secs
                .map(|secs| secs.saturating_mul(1000).to_string()),
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

/// Hidden idempotency marker appended to GitHub issues mirrored one-way into Collapse (#612).
pub fn github_sync_marker(github_repository: &str, number: u32) -> String {
    format!("<!-- velocity-github-sync: {github_repository}#{number} -->")
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

/// Whether a project's transitional GitHub-to-Collapse issue sync window has expired (#612).
pub fn github_sync_expired(sync: &GitHubIssueSync, now_secs: u64) -> bool {
    sync.enabled
        && sync
            .expires_at_secs
            .is_some_and(|expires_at| now_secs >= expires_at)
}

/// Whether GitHub issue `#number` should be mirrored one-way into Collapse (#612).
pub fn should_sync_github_issue(sync: &GitHubIssueSync, number: u32, now_secs: u64) -> bool {
    sync.enabled
        && !sync.github_repository.trim().is_empty()
        && !github_sync_expired(sync, now_secs)
        && number > sync.high_water_number
        && !sync.synced_numbers.contains_key(&number)
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
            expires_at_secs: Some(1_000),
            ..GitHubIssueSync::default()
        };
        assert!(!should_sync_github_issue(&sync, 612, 500));
        assert!(should_sync_github_issue(&sync, 613, 500));
        assert!(!should_sync_github_issue(&sync, 613, 1_000));
        assert!(github_sync_expired(&sync, 1_000));
        sync.synced_numbers.insert(613, 614);
        assert!(!should_sync_github_issue(&sync, 613, 500));

        let body = format_synced_github_issue_body("Bug details", "mESmaC/velocity", 613);
        assert!(body.contains("https://github.com/mESmaC/velocity/issues/613"));
        assert!(body.contains(&github_sync_marker("mESmaC/velocity", 613)));
    }
}
