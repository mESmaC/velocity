//! Shared per-repository open-issues cache and `updated_at`-keyed discussion cache (#556).
//!
//! Continuous dispatch, the issue panel, Work pool, project health and Rosen pin checks all
//! read from this single-flight cache so one repository's open issues are fetched at most
//! once per [`OPEN_ISSUES_REFRESH`] interval across the whole application.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use velocity_core::collapse::IssueHost;

use crate::{GitHubIssue, IssueDiscussion, IssueState};

/// How long a repository's open issues are reused across every feature before refreshing.
pub const OPEN_ISSUES_REFRESH: Duration = Duration::from_secs(60);

/// How long a closed issue's discussion (not present in the open-issues cache) is reused.
const CLOSED_DISCUSSION_TTL: Duration = Duration::from_secs(5 * 60);
const MAX_DISCUSSION_ENTRIES: usize = 256;

struct RepoIssues {
    issues: Vec<GitHubIssue>,
    fetched_at: Instant,
    version: u64,
}

struct CachedDiscussion {
    updated_at: Option<String>,
    fetched_at: Instant,
    discussion: IssueDiscussion,
}

type CacheKey = (IssueHost, String);
type DiscussionKey = (IssueHost, String, u32);

static ISSUES: LazyLock<Mutex<HashMap<CacheKey, RepoIssues>>> = LazyLock::new(Mutex::default);
static FETCH_LOCKS: LazyLock<Mutex<HashMap<CacheKey, Arc<Mutex<()>>>>> =
    LazyLock::new(Mutex::default);
static DISCUSSIONS: LazyLock<Mutex<HashMap<DiscussionKey, CachedDiscussion>>> =
    LazyLock::new(Mutex::default);
static NEXT_VERSION: AtomicU64 = AtomicU64::new(1);

pub(crate) fn fetch_lock(host: IssueHost, repository: &str) -> Arc<Mutex<()>> {
    let mut locks = FETCH_LOCKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(locks.entry(repo_key(host, repository)).or_default())
}

pub(crate) fn get_fresh(
    host: IssueHost,
    repository: &str,
    max_age: Duration,
) -> Option<Vec<GitHubIssue>> {
    let cache = ISSUES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .get(&repo_key(host, repository))
        .filter(|entry| entry.fetched_at.elapsed() < max_age)
        .map(|entry| entry.issues.clone())
}

pub(crate) fn store(host: IssueHost, repository: &str, issues: &[GitHubIssue]) -> u64 {
    let version = NEXT_VERSION.fetch_add(1, Ordering::Relaxed);
    let mut cache = ISSUES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.insert(
        repo_key(host, repository),
        RepoIssues {
            issues: issues.to_vec(),
            fetched_at: Instant::now(),
            version,
        },
    );
    version
}

pub(crate) fn invalidate(host: IssueHost, repository: &str) {
    let mut cache = ISSUES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.remove(&repo_key(host, repository));
}

pub(crate) fn version(host: IssueHost, repository: &str) -> Option<u64> {
    let cache = ISSUES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .get(&repo_key(host, repository))
        .map(|entry| entry.version)
}

pub(crate) fn cached_issue(host: IssueHost, repository: &str, number: u32) -> Option<GitHubIssue> {
    let cache = ISSUES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .get(&repo_key(host, repository))
        .and_then(|entry| entry.issues.iter().find(|issue| issue.number == number))
        .cloned()
}

pub(crate) fn upsert_issue(host: IssueHost, repository: &str, issue: &GitHubIssue) {
    let mut cache = ISSUES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(entry) = cache.get_mut(&repo_key(host, repository)) else {
        return;
    };
    if issue.state == IssueState::Closed {
        entry
            .issues
            .retain(|existing| existing.number != issue.number);
    } else if let Some(existing) = entry
        .issues
        .iter_mut()
        .find(|existing| existing.number == issue.number)
    {
        *existing = issue.clone();
    } else {
        entry.issues.push(issue.clone());
    }
    entry.version = NEXT_VERSION.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn get_discussion(
    host: IssueHost,
    repository: &str,
    number: u32,
) -> Option<IssueDiscussion> {
    let current_updated_at = cached_issue(host, repository, number).map(|issue| issue.updated_at);
    let cache = DISCUSSIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = cache.get(&(host, repository.to_ascii_lowercase(), number))?;
    match current_updated_at {
        Some(updated_at) if entry.updated_at.as_deref() == Some(updated_at.as_str()) => {
            Some(entry.discussion.clone())
        }
        None if entry.fetched_at.elapsed() < CLOSED_DISCUSSION_TTL => {
            Some(entry.discussion.clone())
        }
        _ => None,
    }
}

pub(crate) fn store_discussion(
    host: IssueHost,
    repository: &str,
    number: u32,
    updated_at: Option<String>,
    discussion: &IssueDiscussion,
) {
    let mut cache = DISCUSSIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if cache.len() >= MAX_DISCUSSION_ENTRIES {
        cache.clear();
    }
    cache.insert(
        (host, repository.to_ascii_lowercase(), number),
        CachedDiscussion {
            updated_at,
            fetched_at: Instant::now(),
            discussion: discussion.clone(),
        },
    );
}

pub(crate) fn invalidate_discussion(host: IssueHost, repository: &str, number: u32) {
    let mut cache = DISCUSSIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.remove(&(host, repository.to_ascii_lowercase(), number));
}

fn repo_key(host: IssueHost, repository: &str) -> CacheKey {
    (host, repository.to_ascii_lowercase())
}
