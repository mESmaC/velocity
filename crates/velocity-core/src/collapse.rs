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

/// The repository's page, for a value `repository_from_remote` returned.
pub fn repository_page(repository: &str) -> Option<String> {
    let (owner, name) = repository.split_once('/')?;
    (valid_segment(owner) && valid_segment(name)).then(|| format!("{PAGE_HOST}{owner}/{name}"))
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

/// The repository's Settings → Downstream remote section, where the owner points Collapse's
/// forwarder at GitHub (C4). Collapse accepts that change only from the owner's browser.
pub fn downstream_settings_page(repository: &str) -> Option<String> {
    Some(format!(
        "{}/settings#downstream",
        repository_page(repository)?
    ))
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
    /// Reads the dashboard's `forward.state` (Collapse C2); anything else is unavailable.
    pub fn from_collapse(state: &str) -> Self {
        match state {
            "unconfigured" => Self::Unconfigured,
            "synced" => Self::Synced,
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
    use super::{github_repository_from_remote, land_page};

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
}
