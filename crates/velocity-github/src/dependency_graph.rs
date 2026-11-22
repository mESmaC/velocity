//! Which open issues wait on which, joined across the repositories the owner's projects
//! use (#510). Continuous dispatch starts the issue that unblocks the most open work first.
//! Pure and synchronous: it holds only what the issue readers last saw.
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::GitHubIssue;

/// A lowercased `owner/name` and an issue number.
type IssueKey = (String, u32);
type BorrowedKey<'graph> = (&'graph str, u32);

#[derive(Clone, Debug, Default)]
pub struct DependencyGraph {
    /// Per reader (one per project): each open blocker with an open issue that waits on it.
    /// Two projects on one repository repeat its edges, which the count ignores.
    sources: BTreeMap<u32, Vec<(IssueKey, IssueKey)>>,
}

impl DependencyGraph {
    /// Replaces what `source` last read with `repository`'s current open issues.
    pub fn replace_source(&mut self, source: u32, repository: &str, issues: &[GitHubIssue]) {
        self.replace_source_with_blockers(source, repository, issues, &BTreeMap::new());
    }

    /// Replaces what `source` last read with `repository`'s current open issues plus any
    /// additional open blockers (such as explicit text blockers from body or owner comments, #623).
    pub fn replace_source_with_blockers(
        &mut self,
        source: u32,
        repository: &str,
        issues: &[GitHubIssue],
        extra_open_blockers: &BTreeMap<u32, Vec<String>>,
    ) {
        let repository = repository.to_lowercase();
        let edges = issues
            .iter()
            .flat_map(|issue| {
                let dependent = (repository.clone(), issue.number);
                let extra = extra_open_blockers
                    .get(&issue.number)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]);
                issue
                    .open_blockers
                    .iter()
                    .chain(extra.iter())
                    .filter_map(|blocker| blocker_key(&repository, blocker))
                    .map(move |blocker| (blocker, dependent.clone()))
            })
            .collect();
        self.sources.insert(source, edges);
    }

    /// Forgets readers that no longer exist, such as a removed project's.
    pub fn retain_sources(&mut self, mut keep: impl FnMut(u32) -> bool) {
        self.sources.retain(|source, _| keep(*source));
    }

    /// For each issue of `repository` that blocks open work: how many open issues wait on
    /// it, directly or through other open issues, in any repository that was read.
    /// Issues that block nothing are absent.
    pub fn unblock_counts(&self, repository: &str) -> BTreeMap<u32, u32> {
        let repository = repository.to_lowercase();
        let mut dependents: HashMap<BorrowedKey, Vec<BorrowedKey>> = HashMap::new();
        for (blocker, dependent) in self.sources.values().flatten() {
            dependents
                .entry((blocker.0.as_str(), blocker.1))
                .or_default()
                .push((dependent.0.as_str(), dependent.1));
        }
        dependents
            .keys()
            .filter(|(name, _)| *name == repository)
            .map(|blocker| (blocker.1, waiting_on(&dependents, *blocker)))
            .collect()
    }
}

/// Only open issues are read and only open blockers are listed, so every edge joins two
/// open issues and a closed issue in the middle of a chain ends it.
fn waiting_on<'graph>(
    dependents: &HashMap<BorrowedKey<'graph>, Vec<BorrowedKey<'graph>>>,
    blocker: BorrowedKey<'graph>,
) -> u32 {
    let mut seen = HashSet::from([blocker]);
    let mut pending = vec![blocker];
    while let Some(issue) = pending.pop() {
        for dependent in dependents.get(&issue).into_iter().flatten() {
            if seen.insert(*dependent) {
                pending.push(*dependent);
            }
        }
    }
    // `seen` holds the blocker itself, so a dependency cycle back to it adds nothing.
    u32::try_from(seen.len().saturating_sub(1)).unwrap_or(u32::MAX)
}

/// `GitHubIssue::open_blockers` lists `#n` in its own repository and `owner/name#n` elsewhere.
fn blocker_key(repository: &str, blocker: &str) -> Option<IssueKey> {
    let (name, number) = blocker.rsplit_once('#')?;
    let name = if name.is_empty() {
        repository.to_owned()
    } else {
        name.to_lowercase()
    };
    Some((name, number.parse().ok()?))
}
