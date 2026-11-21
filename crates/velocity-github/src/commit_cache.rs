//! Process-wide commit status and ancestry cache (#556).
//!
//! Records green `velocity/integration` commits, covered ancestors, mirrored commits,
//! ancestry results and settled non-green commits so orphaned landed holds and post-land
//! verification never re-poll old commits.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::IntegrationCoverage;

/// How long a repository's default-branch tip and recent commit statuses stay fresh before
/// re-reading (also refreshed automatically by every shared `open_issues` GraphQL read).
pub(crate) const BRANCH_SNAPSHOT_TTL: Duration = Duration::from_secs(2 * 60);

const MAX_ANCESTRY_ENTRIES: usize = 2048;
const MAX_SETTLED_ENTRIES: usize = 2048;

#[derive(Clone, Debug)]
pub(crate) struct CommitStatusEntry {
    pub sha: String,
    /// `Some(passed_at)` when this commit carries a green `velocity/integration` status.
    pub integration_green: Option<Option<u64>>,
}

#[derive(Clone, Debug)]
pub(crate) struct BranchTipSnapshot {
    pub fetched_at: Instant,
    pub tip: String,
    /// Newest commits on the default branch, newest first.
    pub history: Vec<CommitStatusEntry>,
}

impl BranchTipSnapshot {
    /// The newest commit in `history` with a green `velocity/integration` status.
    pub(crate) fn newest_green(&self) -> Option<IntegrationCoverage> {
        self.history.iter().find_map(|entry| {
            entry
                .integration_green
                .map(|passed_at| IntegrationCoverage {
                    commit: entry.sha.clone(),
                    passed_at,
                })
        })
    }

    /// Position of `commit` in `history` (0 is tip), if present.
    pub(crate) fn position(&self, commit: &str) -> Option<usize> {
        self.history
            .iter()
            .position(|entry| entry.sha.eq_ignore_ascii_case(commit))
    }

    /// Newest green commit at or above `index` (covering `history[index]`).
    pub(crate) fn green_covering_index(&self, index: usize) -> Option<IntegrationCoverage> {
        self.history[..=index].iter().find_map(|entry| {
            entry
                .integration_green
                .map(|passed_at| IntegrationCoverage {
                    commit: entry.sha.clone(),
                    passed_at,
                })
        })
    }
}

#[derive(Default)]
struct CommitCache {
    snapshots: HashMap<String, BranchTipSnapshot>,
    green: HashMap<(String, String), Option<u64>>,
    covered: HashMap<(String, String), IntegrationCoverage>,
    mirrored: HashSet<(String, String)>,
    ancestry: HashMap<(String, String, String), bool>,
    settled_non_green: HashSet<(String, String)>,
}

static CACHE: LazyLock<Mutex<CommitCache>> = LazyLock::new(Mutex::default);
static BRANCH_LOCKS: LazyLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> =
    LazyLock::new(Mutex::default);

pub(crate) fn branch_fetch_lock(repository: &str) -> Arc<Mutex<()>> {
    let mut locks = BRANCH_LOCKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    Arc::clone(locks.entry(repo_key(repository)).or_default())
}

pub(crate) fn fresh_branch_snapshot(
    repository: &str,
    max_age: Duration,
) -> Option<BranchTipSnapshot> {
    let cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .snapshots
        .get(&repo_key(repository))
        .filter(|snapshot| snapshot.fetched_at.elapsed() < max_age)
        .cloned()
}

pub(crate) fn record_branch_snapshot(
    repository: &str,
    tip: String,
    history: Vec<CommitStatusEntry>,
) -> BranchTipSnapshot {
    let key = repo_key(repository);
    let snapshot = BranchTipSnapshot {
        fetched_at: Instant::now(),
        tip,
        history,
    };
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut active_green: Option<IntegrationCoverage> = None;
    for entry in &snapshot.history {
        let sha_key = sha_key(&entry.sha);
        cache.mirrored.insert((key.clone(), sha_key.clone()));
        if let Some(passed_at) = entry.integration_green {
            cache
                .green
                .insert((key.clone(), sha_key.clone()), passed_at);
            cache
                .settled_non_green
                .remove(&(key.clone(), sha_key.clone()));
            let coverage = IntegrationCoverage {
                commit: entry.sha.clone(),
                passed_at,
            };
            if active_green.is_none() {
                active_green = Some(coverage);
            }
        }
        if let Some(coverage) = &active_green {
            cache
                .covered
                .entry((key.clone(), sha_key))
                .or_insert_with(|| coverage.clone());
        }
    }
    cache.snapshots.insert(key, snapshot.clone());
    snapshot
}

pub(crate) fn record_green(repository: &str, commit: &str, passed_at: Option<u64>) {
    let repo = repo_key(repository);
    let sha = sha_key(commit);
    let coverage = IntegrationCoverage {
        commit: commit.to_owned(),
        passed_at,
    };
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.green.insert((repo.clone(), sha.clone()), passed_at);
    cache.mirrored.insert((repo.clone(), sha.clone()));
    cache.settled_non_green.remove(&(repo.clone(), sha.clone()));
    cache.covered.insert((repo.clone(), sha), coverage.clone());
    if let Some(snapshot) = cache.snapshots.get_mut(&repo)
        && let Some(index) = snapshot.position(commit)
    {
        snapshot.history[index].integration_green = Some(passed_at);
        let covered_shas: Vec<String> = snapshot.history[index..]
            .iter()
            .map(|entry| sha_key(&entry.sha))
            .collect();
        for older in covered_shas {
            cache
                .covered
                .entry((repo.clone(), older))
                .or_insert_with(|| coverage.clone());
        }
    }
}

pub(crate) fn record_covered(repository: &str, commit: &str, coverage: IntegrationCoverage) {
    let repo = repo_key(repository);
    let sha = sha_key(commit);
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache.mirrored.insert((repo.clone(), sha.clone()));
    cache.covered.insert((repo, sha), coverage);
}

pub(crate) fn covered(repository: &str, commit: &str) -> Option<IntegrationCoverage> {
    let cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let key = (repo_key(repository), sha_key(commit));
    if let Some(coverage) = cache.covered.get(&key) {
        return Some(coverage.clone());
    }
    cache.green.get(&key).map(|passed_at| IntegrationCoverage {
        commit: commit.to_owned(),
        passed_at: *passed_at,
    })
}

pub(crate) fn is_known_green(repository: &str, commit: &str) -> bool {
    let cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .green
        .contains_key(&(repo_key(repository), sha_key(commit)))
}

pub(crate) fn is_mirrored(repository: &str, commit: &str) -> bool {
    let cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .mirrored
        .contains(&(repo_key(repository), sha_key(commit)))
}

pub(crate) fn record_mirrored(repository: &str, commit: &str) {
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .mirrored
        .insert((repo_key(repository), sha_key(commit)));
}

pub(crate) fn cached_ancestry(repository: &str, ancestor: &str, descendant: &str) -> Option<bool> {
    let cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .ancestry
        .get(&(repo_key(repository), sha_key(ancestor), sha_key(descendant)))
        .copied()
}

pub(crate) fn record_ancestry(
    repository: &str,
    ancestor: &str,
    descendant: &str,
    is_ancestor: bool,
) {
    let repo = repo_key(repository);
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if cache.ancestry.len() >= MAX_ANCESTRY_ENTRIES {
        cache.ancestry.clear();
    }
    if is_ancestor {
        cache.mirrored.insert((repo.clone(), sha_key(ancestor)));
    }
    cache
        .ancestry
        .insert((repo, sha_key(ancestor), sha_key(descendant)), is_ancestor);
}

pub(crate) fn is_settled_non_green(repository: &str, commit: &str) -> bool {
    let cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    cache
        .settled_non_green
        .contains(&(repo_key(repository), sha_key(commit)))
}

pub(crate) fn record_settled_non_green(repository: &str, commit: &str) {
    let mut cache = CACHE
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if cache.settled_non_green.len() >= MAX_SETTLED_ENTRIES {
        cache.settled_non_green.clear();
    }
    cache
        .settled_non_green
        .insert((repo_key(repository), sha_key(commit)));
}

fn repo_key(repository: &str) -> String {
    repository.to_ascii_lowercase()
}

fn sha_key(commit: &str) -> String {
    commit.to_ascii_lowercase()
}
