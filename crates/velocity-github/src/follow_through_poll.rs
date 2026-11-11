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

/// Open PRs per page. GitHub prices a query by the nodes it may return, so 50 keeps a page at
/// one rate-limit point; 100 costs two.
const OPEN_PAGE: u32 = 50;

/// Closed and merged PRs drop out of the open list. The most recently updated of them are
/// read in the same request, so a PR that closes between two passes is still seen.
const RECENTLY_CLOSED: u32 = 25;

/// A followed open PR is read in full at least this often, even when its listed fields did not
/// move. GitHub records an edit to an inline review comment without moving any listed field,
/// and an edited review comment must still reach the agent.
pub const FULL_REFRESH: Duration = Duration::from_secs(30 * 60);

/// One page of a PR's review threads, as the fingerprint reads them.
macro_rules! thread_page {
    () => {
        "nodes { id isResolved comments { totalCount } } pageInfo { hasNextPage endCursor }"
    };
}

/// One page of the head's check contexts, as the fingerprint reads them.
macro_rules! context_page {
    () => {
        "nodes { __typename
      ... on CheckRun { name status conclusion detailsUrl }
      ... on StatusContext { context state targetUrl }
    } pageInfo { hasNextPage endCursor }"
    };
}

/// The fields, besides the PR's own, that show whether a snapshot is out of date. Each was
/// checked against GitHub for #217:
/// - `updatedAt` (in the PR's fields) moves on a push, a new or edited conversation comment,
///   a new or edited review, a new inline thread and a reply in one, but can lag by seconds.
/// - It does not move when a CI check changes, or when a thread is resolved or unresolved,
///   so the head's check contexts and each thread's `isResolved` are read here.
/// - The counts change at once when a comment, review, thread or reply is added or deleted.
///
/// A PR with more than 100 threads or check contexts has the rest read page by page on every
/// pass (`complete_nested`), so resolving the 101st thread or a change to the 101st check is
/// seen like any other (#229). Such PRs are rare; each costs one request per further 100.
const FINGERPRINT_FIELDS: &str = concat!(
    "comments { totalCount } reviews { totalCount }
  reviewThreads(first:100) { totalCount ",
    thread_page!(),
    " }
  headRef { target { ... on Commit { oid statusCheckRollup { state contexts(first:100) {
    totalCount ",
    context_page!(),
    " } } } } }"
);

const MORE_THREADS: &str = concat!(
    "query($owner:String!,$name:String!,$number:Int!,$cursor:String) {
  repository(owner:$owner,name:$name) { pullRequest(number:$number) {
    reviewThreads(first:100,after:$cursor) { ",
    thread_page!(),
    " } } } }"
);

const MORE_CONTEXTS: &str = concat!(
    "query($owner:String!,$name:String!,$number:Int!,$cursor:String) {
  repository(owner:$owner,name:$name) { pullRequest(number:$number) {
    headRef { target { ... on Commit { statusCheckRollup {
      contexts(first:100,after:$cursor) { ",
    context_page!(),
    " } } } } } } } }"
);

/// One PR as the list shows it, and what its listed fields were.
#[derive(Clone, Debug)]
pub struct ListedPullRequest {
    pub pull_request: PullRequest,
    pub fingerprint: String,
}

impl PullRequestClient {
    /// Every open PR and the most recently closed or merged ones: one request, and one more
    /// for each further 50 open PRs.
    pub fn follow_through_list(&self) -> Result<Vec<ListedPullRequest>, PullRequestError> {
        // A closed or merged PR needs only its own fields: follow-through reads it in full
        // once when it leaves the open list, and nothing that happens to it later is sent to
        // the agent.
        let query = format!(
            "query($owner:String!,$name:String!,$cursor:String) {{
              repository(owner:$owner,name:$name) {{
                open: pullRequests(states:OPEN,first:{OPEN_PAGE},after:$cursor,
                  orderBy:{{field:UPDATED_AT,direction:DESC}}) {{
                  nodes {{ {FIELDS} {FINGERPRINT_FIELDS} }} pageInfo {{ hasNextPage endCursor }}
                }}
                recent: pullRequests(states:[CLOSED,MERGED],first:{RECENTLY_CLOSED},
                  orderBy:{{field:UPDATED_AT,direction:DESC}}) {{
                  nodes {{ {FIELDS} }} pageInfo {{ hasNextPage endCursor }}
                }}
              }}
            }}"
        );
        let response: Value = self.graphql_at(&query, self.variables(0), "/data/repository")?;
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
        nodes.into_iter().map(|node| self.listed(node)).collect()
    }

    fn listed(&self, mut node: Value) -> Result<ListedPullRequest, PullRequestError> {
        let pull_request: PullRequest = serde_json::from_value(node.clone())?;
        let number = pull_request.number;
        self.complete_nested(&mut node, number, "/reviewThreads", MORE_THREADS)?;
        self.complete_nested(
            &mut node,
            number,
            "/headRef/target/statusCheckRollup/contexts",
            MORE_CONTEXTS,
        )?;
        fingerprinted(node, pull_request)
    }

    /// Reads the rest of a nested connection the list cut off at 100 nodes, and drops its page
    /// cursor, so the fingerprint holds every node and nothing that moves without a change.
    fn complete_nested(
        &self,
        node: &mut Value,
        number: u32,
        pointer: &str,
        query: &str,
    ) -> Result<(), PullRequestError> {
        // A closed PR's node, a deleted head branch or a head with no checks has none.
        let Some(connection) = node.pointer_mut(pointer).filter(|value| value.is_object()) else {
            return Ok(());
        };
        let first_page: Connection<Value> = serde_json::from_value(connection.clone())?;
        let nodes = self.complete_connection(
            query,
            self.variables(number),
            &format!("/data/repository/pullRequest{pointer}"),
            first_page,
        )?;
        connection["nodes"] = Value::Array(nodes);
        if let Some(fields) = connection.as_object_mut() {
            fields.remove("pageInfo");
        }
        Ok(())
    }
}

fn fingerprinted(
    mut node: Value,
    pull_request: PullRequest,
) -> Result<ListedPullRequest, PullRequestError> {
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

/// A PR read in full whose head is not the thread's branch, and its listed fields at that read.
struct Mismatch {
    number: u32,
    fingerprint: Option<String>,
    read_at: Instant,
}

/// The last snapshot each followed thread observed, kept in memory: after a restart each
/// followed thread takes one snapshot again.
#[derive(Default)]
pub struct SnapshotCache {
    snapshots: BTreeMap<(u32, u32), CachedSnapshot>,
    searched: BTreeSet<(u32, u32)>,
    mismatched: BTreeMap<(u32, u32), Mismatch>,
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
        if self.still_mismatched(thread.key, number, listed) {
            return PollChoice::Skip;
        }
        match (listed, cached) {
            (Some(listed), _) if !thread.watch.should_follow(&listed.pull_request) => {
                PollChoice::Skip
            }
            (Some(listed), Some(cached))
                if cached.fingerprint.as_ref() == Some(&listed.fingerprint)
                    && (cached.read_at.elapsed() < FULL_REFRESH
                        || listed.pull_request.state != PullRequestState::Open) =>
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

    /// Whether the PR was last read with a head other than the thread's branch, and nothing
    /// listed about it has moved since. A changed head moves `headRefName` or the head
    /// repository in the listed fields, so the PR is read again as soon as it could match;
    /// the half-hourly full read covers anything the list does not show (#229).
    fn still_mismatched(
        &self,
        key: (u32, u32),
        number: u32,
        listed: Option<&ListedPullRequest>,
    ) -> bool {
        self.mismatched.get(&key).is_some_and(|mismatch| {
            mismatch.number == number
                && mismatch.fingerprint.as_ref() == listed.map(|listed| &listed.fingerprint)
                && mismatch.read_at.elapsed() < FULL_REFRESH
        })
    }

    /// Records that PR `number`, read with these listed fields, is not the thread's branch.
    pub fn mismatched(&mut self, key: (u32, u32), number: u32, fingerprint: Option<String>) {
        self.snapshots.remove(&key);
        self.mismatched.insert(
            key,
            Mismatch {
                number,
                fingerprint,
                read_at: Instant::now(),
            },
        );
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
        self.mismatched.remove(&key);
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
        self.mismatched.retain(|key, _| followed.contains(key));
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
                (
                    listed.pull_request.created_at.clone(),
                    listed.pull_request.number,
                )
            };
            order(left).cmp(&order(right))
        })
}
