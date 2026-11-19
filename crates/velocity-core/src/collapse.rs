//! Collapse pages for a project whose Rosen mirror is a Collapse repository (#429).
//!
//! A land's commit page is immutable on Collapse, so it is the link to share. Only a
//! recognised Collapse remote with a plain `owner/repository` path produces a page, so
//! an arbitrary or credential-bearing mirror URL never reaches the browser.

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

/// A landed commit's page, which Collapse caches as immutable.
pub fn land_page(repository: &str, commit: &str) -> Option<String> {
    let is_commit =
        (7..=64).contains(&commit.len()) && commit.bytes().all(|byte| byte.is_ascii_hexdigit());
    if !is_commit {
        return None;
    }
    Some(format!(
        "{}/commit/{}",
        repository_page(repository)?,
        commit.to_ascii_lowercase()
    ))
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
