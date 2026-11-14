//! A Rosen project's open PRs, read from GitHub off its pass (SPEC.md F7 "How a review
//! runs"). A Rosen project's work is its Rosen changes; its PRs are what is left on GitHub
//! from before, or what a bot opens there. Listing them is a network request that can take
//! seconds, or hang until its deadline, so the pass never waits for it: it works with the
//! PRs as last listed, and a listing that finds something new wakes the runner.

use std::time::{Duration, Instant};

use velocity_core::pr_review::ReviewCandidate;
use velocity_store::duty::PrDutyRoster;

use super::PrDutyReviews;

/// How old a listing may be before a pass has it read again. A pass runs at every Rosen
/// event, far more often than a PR changes, and each listing is a GitHub request.
const FRESH: Duration = Duration::from_secs(20);
/// A listing older than this is not acted on: its PRs may have merged or moved since.
const STALE: Duration = Duration::from_secs(120);

/// One project's open PRs as GitHub last listed them.
#[derive(Default)]
pub(super) struct Listing {
    candidates: Vec<ReviewCandidate>,
    /// When the listing was read; `None` before the first.
    read: Option<Instant>,
    /// A read is under way.
    reading: bool,
}

impl PrDutyReviews {
    /// The project's open PRs as last listed, and a new listing started when that one is
    /// no longer fresh. Empty before the first listing, when the last one is stale, and
    /// when GitHub can't be read.
    pub(super) fn listed_pull_requests(&self, roster: &PrDutyRoster) -> Vec<ReviewCandidate> {
        let project = roster.project.id;
        let (candidates, read_again) = {
            let mut state = self.lock();
            let listing = state.listings.entry(project).or_default();
            let age = listing.read.map(|read| read.elapsed());
            let read_again = !listing.reading && age.is_none_or(|age| age > FRESH);
            listing.reading |= read_again;
            let usable = age.is_some_and(|age| age <= STALE);
            let candidates = if usable {
                listing.candidates.clone()
            } else {
                Vec::new()
            };
            (candidates, read_again)
        };
        if read_again {
            let (runner, roster) = (self.clone(), roster.clone());
            tokio::spawn(async move { runner.read_listing(&roster).await });
        }
        candidates
    }

    async fn read_listing(&self, roster: &PrDutyRoster) {
        // A Rosen project needs no GitHub remote: one that can't be listed has no PRs.
        let listed = self
            .pull_request_candidates(roster)
            .await
            .unwrap_or_default();
        let changed = {
            let mut state = self.lock();
            let listing = state.listings.entry(roster.project.id).or_default();
            let changed = listing.candidates != listed;
            listing.candidates = listed;
            listing.read = Some(Instant::now());
            listing.reading = false;
            changed
        };
        if changed {
            // The pass that started this read went on without it.
            self.wake();
        }
    }
}
