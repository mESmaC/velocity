//! Shared per-repository open-issues cache and `updated_at`-keyed discussion cache (#556).
//!
//! Continuous dispatch, the issue panel, Work pool, project health and Rosen pin checks all
//! read from this single-flight cache so one repository's open issues are fetched at most
//! once per [`OPEN_ISSUES_REFRESH`] interval across the whole application.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

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

static ISSUES: LazyLock<Mutex<HashMap<String, RepoIssues>>> = LazyLock::new(Mutex::default);
static FETCH_LOCKS: LazyLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> =
    LazyLock::new(Mutex::default);
static DISCUSSIONS: LazyLock<Mutex<HashMap<(String, u32), CachedDiscussion>>> =
    LazyLock::new(Mutex::default);
static NEXT_VERSION: AtomicU64 = AtomicU64::new(1);

pub(crate) fn fetch_lock(repository: &str) -> Arc<Mutex<()>> {
    let mut locks = FETCH_LOCKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(locks.entry(repo_key(repository)).or_default())
}

pub(crate) fn get_fresh(repository: &str, max_age: Duration) -> Option<Vec<GitHubIssue>> {
    let cache = ISSUES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .get(&repo_key(repository))
        .filter(|entry| entry.fetched_at.elapsed() < max_age)
        .map(|entry| entry.issues.clone())
}

pub(crate) fn store(repository: &str, issues: &[GitHubIssue]) -> u64 {
    let version = NEXT_VERSION.fetch_add(1, Ordering::Relaxed);
    let mut cache = ISSUES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.insert(
        repo_key(repository),
        RepoIssues {
            issues: issues.to_vec(),
            fetched_at: Instant::now(),
            version,
        },
    );
    version
}

pub(crate) fn invalidate(repository: &str) {
    let mut cache = ISSUES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.remove(&repo_key(repository));
}

pub(crate) fn version(repository: &str) -> Option<u64> {
    let cache = ISSUES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.get(&repo_key(repository)).map(|entry| entry.version)
}

pub(crate) fn cached_issue(repository: &str, number: u32) -> Option<GitHubIssue> {
    let cache = ISSUES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .get(&repo_key(repository))
        .and_then(|entry| entry.issues.iter().find(|issue| issue.number == number))
        .cloned()
}

pub(crate) fn upsert_issue(repository: &str, issue: &GitHubIssue) {
    let mut cache = ISSUES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(entry) = cache.get_mut(&repo_key(repository)) else {
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

pub(crate) fn get_discussion(repository: &str, number: u32) -> Option<IssueDiscussion> {
    let current_updated_at = cached_issue(repository, number).map(|issue| issue.updated_at);
    let cache = DISCUSSIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let entry = cache.get(&(repo_key(repository), number))?;
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
        (repo_key(repository), number),
        CachedDiscussion {
            updated_at,
            fetched_at: Instant::now(),
            discussion: discussion.clone(),
        },
    );
}

pub(crate) fn invalidate_discussion(repository: &str, number: u32) {
    let mut cache = DISCUSSIONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.remove(&(repo_key(repository), number));
}

fn repo_key(repository: &str) -> String {
    repository.to_ascii_lowercase()
}
