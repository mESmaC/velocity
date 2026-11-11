//! Which PRs F6 follow-through must read again on a pass (#217). One list query per project
//! shows every open PR, and the most recently closed or merged ones, with the fields that
//! move when anything follow-through acts on changes. A thread whose PR shows the same
//! fields as at its last snapshot reuses that snapshot instead of reading GitHub again.
use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::follow_through::FollowThrough;
use crate::pull_request_queries::FIELDS;
use crate::pull_requests::Connection;
use crate::{
    PullRequest, PullRequestClient, PullRequestError, PullRequestSnapshot, PullRequestState,
};

/// Closed and merged PRs drop out of the open list. The most recently updated of them are
/// read in the same request, so a PR that closes between two passes is still seen.
const RECENTLY_CLOSED: u32 = 25;

/// A followed PR is read in full at least this often, even when its listed fields did not
/// move: a backstop for a change GitHub records without moving any of them.
pub const FULL_REFRESH: Duration = Duration::from_secs(30 * 60);

/// Every field below is checked against GitHub in #217's PR:
/// - `updatedAt` moves on pushes, new and edited comments, new and edited reviews, review
///   thread replies, resolving and unresolving a thread, ready/draft, close and reopen.
/// - It does not move when a CI check changes, so the check contexts are read here too.
/// - The counts catch a new comment, review or thread at once, while `updatedAt` can lag
///   behind by a few seconds.
const FINGERPRINT_FIELDS: &str = "comments { totalCount } reviews { totalCount }
  reviewThreads(first:100) { totalCount nodes { id isResolved comments { totalCount } } }
  commits(last:1) { nodes { commit { statusCheckRollup { state contexts(first:100) {
    totalCount nodes { __typename
      ... on CheckRun { name status conclusion detailsUrl }
      ... on StatusContext { context state targetUrl }
    } } } } } }";

/// One PR as the list shows it, and what its listed fields were.
#[derive(Clone, Debug)]
pub struct ListedPullRequest {
    pub pull_request: PullRequest,
    pub fingerprint: String,
}

impl PullRequestClient {
    /// Every open PR and the most recently closed or merged ones, in one request per 100
    /// open PRs.
    pub fn follow_through_list(&self) -> Result<Vec<ListedPullRequest>, PullRequestError> {
        let query = format!(
            "query($owner:String!,$name:String!,$cursor:String) {{
              repository(owner:$owner,name:$name) {{
                open: pullRequests(states:OPEN,first:100,after:$cursor,
                  orderBy:{{field:UPDATED_AT,direction:DESC}}) {{
                  nodes {{ {FIELDS} {FINGERPRINT_FIELDS} }} pageInfo {{ hasNextPage endCursor }}
                }}
                recent: pullRequests(states:[CLOSED,MERGED],first:{RECENTLY_CLOSED},
                  orderBy:{{field:UPDATED_AT,direction:DESC}}) {{
                  nodes {{ {FIELDS} {FINGERPRINT_FIELDS} }} pageInfo {{ hasNextPage endCursor }}
                }}
              }}
            }}"
        );
        let response: Value =
            self.graphql_at(&query, self.variables(0), "/data/repository")?;
        let page = |name: &str| -> Result<Connection<Value>, PullRequestError> {
            let page = response
                .get(name)
                .ok_or_else(|| PullRequestError::Missing(format!("{name} pull requests")))?;
            Ok(serde_json::from_value(page.clone())?)
        };
        let mut nodes = self.complete_connection(
            &query,
            self.variables(0),
            "/data/repository/open",
            page("open")?,
        )?;
        nodes.extend(page("recent")?.into_nodes());
        nodes.into_iter().map(listed).collect()
    }
}

fn listed(mut node: Value) -> Result<ListedPullRequest, PullRequestError> {
    let pull_request: PullRequest = serde_json::from_value(node.clone())?;
    // GitHub recomputes mergeability lazily after the base moves, so the exact status flickers
    // through UNKNOWN on every merge to the base. Follow-through acts only on conflicts.
    if let Some(status) = node.get_mut("mergeStateStatus") {
        *status = Value::Bool(pull_request.merge_state_status == "DIRTY");
    }
    Ok(ListedPullRequest {
        pull_request,
        fingerprint: node.to_string(),
    })
}

/// A thread's part in this pass, read from the store before any GitHub call.
pub struct PollThread<'a> {
    pub key: (u32, u32),
    pub branch: &'a str,
    pub saved_number: Option<u32>,
    pub watch: &'a FollowThrough,
}

/// What a pass does for one thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PollChoice {
    /// Nothing to follow: no PR yet, a draft claim, or a PR from an earlier use of the branch.
    Skip,
    /// The PR's listed fields match the last snapshot; observe that snapshot again.
    Reuse,
    /// Read the PR in full. `fingerprint` is its listed fields before the read.
    Refresh {
        number: u32,
        fingerprint: Option<String>,
    },
    /// The thread has no saved PR and has not been looked up by branch since Velocity
    /// started: the PR may have opened and closed while Velocity was not watching.
    Search,
}

struct CachedSnapshot {
    fingerprint: Option<String>,
    snapshot: PullRequestSnapshot,
    read_at: Instant,
}

/// The last snapshot each followed thread observed, kept in memory: after a restart each
/// followed thread takes one snapshot again.
#[derive(Default)]
pub struct SnapshotCache {
    snapshots: BTreeMap<(u32, u32), CachedSnapshot>,
    searched: BTreeSet<(u32, u32)>,
}

impl SnapshotCache {
    pub fn choose(
        &self,
        thread: &PollThread<'_>,
        listing: &[ListedPullRequest],
        owns: impl Fn(&PullRequest) -> bool,
    ) -> PollChoice {
        let number = match thread.saved_number {
            Some(number) => number,
            None if !self.searched.contains(&thread.key) => return PollChoice::Search,
            None => match newest_for_branch(listing, &owns) {
                Some(listed)
                    if thread.watch.should_follow(&listed.pull_request)
                        && thread.watch.belongs_to_thread(&listed.pull_request) =>
                {
                    listed.pull_request.number
                }
                _ => return PollChoice::Skip,
            },
        };
        let cached = self
            .snapshots
            .get(&thread.key)
            .filter(|cached| cached.snapshot.pull_request.number == number);
        let listed = listing
            .iter()
            .find(|listed| listed.pull_request.number == number);
        match (listed, cached) {
            (Some(listed), _) if !thread.watch.should_follow(&listed.pull_request) => {
                PollChoice::Skip
            }
            (Some(listed), Some(cached))
                if cached.fingerprint.as_ref() == Some(&listed.fingerprint)
                    && cached.read_at.elapsed() < FULL_REFRESH =>
            {
                PollChoice::Reuse
            }
            (Some(listed), _) => PollChoice::Refresh {
                number,
                fingerprint: Some(listed.fingerprint.clone()),
            },
            // Off both lists, and already read as closed or merged: nothing more to learn.
            (None, Some(cached))
                if cached.snapshot.pull_request.state != PullRequestState::Open =>
            {
                PollChoice::Reuse
            }
            (None, _) => PollChoice::Refresh {
                number,
                fingerprint: None,
            },
        }
    }

    pub fn searched(&mut self, key: (u32, u32)) {
        self.searched.insert(key);
    }

    pub fn snapshot(&self, key: (u32, u32)) -> Option<&PullRequestSnapshot> {
        self.snapshots.get(&key).map(|cached| &cached.snapshot)
    }

    pub fn store(
        &mut self,
        key: (u32, u32),
        fingerprint: Option<String>,
        snapshot: PullRequestSnapshot,
    ) {
        self.snapshots.insert(
            key,
            CachedSnapshot {
                fingerprint,
                snapshot,
                read_at: Instant::now(),
            },
        );
    }

    /// Forget threads that are no longer followed, so a reused thread identifier starts clean.
    pub fn retain(&mut self, followed: &BTreeSet<(u32, u32)>) {
        self.snapshots.retain(|key, _| followed.contains(key));
        self.searched.retain(|key| followed.contains(key));
    }
}

/// The branch's newest PR, as `PullRequestClient::for_branch` orders them.
fn newest_for_branch<'a>(
    listing: &'a [ListedPullRequest],
    owns: &impl Fn(&PullRequest) -> bool,
) -> Option<&'a ListedPullRequest> {
    listing
        .iter()
        .filter(|listed| owns(&listed.pull_request))
        .max_by(|left, right| {
            let order = |listed: &ListedPullRequest| {
                // GitHub's timestamps share one ISO 8601 form, so they order as text.
                (listed.pull_request.created_at.clone(), listed.pull_request.number)
            };
            order(left).cmp(&order(right))
        })
}
