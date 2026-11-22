//! Running Rosen review-swarm and scout workers (SPEC.md F7).
//!
//! Git worktree sweeps have been removed (#418); all duty scouts and review workers run in
//! Rosen workspaces owned by `review_swarm`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Notify;
use velocity_acp::client::AircLaunch;
use velocity_core::duty::{DutyFeedEvent, DutyRoleEntry};
use velocity_store::duty::ScoutRoster;

/// How often the runner looks for due work when nothing wakes it sooner.
const TICK: Duration = Duration::from_secs(20);

/// A change to a project's Scouts: given the feed's timestamp, it returns the events to post.
pub type ScoutChange = Box<dyn FnOnce(&mut DutyRoleEntry, &str) -> Vec<DutyFeedEvent> + Send>;

/// Where the runner saves and shows what sweeps do: the store, the window and its events.
pub trait SweepHost: Clone + Send + Sync + 'static {
    /// Every project's Scouts role.
    fn rosters(&self) -> Vec<ScoutRoster>;
    /// Applies `change` to the project's Scouts, appends and publishes the events it returns,
    /// saves, and refreshes the window. `false` when the project or the role is gone.
    fn update(&self, project: u32, change: ScoutChange) -> bool;
}

type ScoutKey = (u32, String);

/// The duty scout and review-swarm runner. Cheap to clone; every clone drives the same workers.
#[derive(Clone)]
pub struct ScoutSweeps {
    wake: Arc<Notify>,
    review_swarms: crate::review_swarm::ReviewSwarms,
}

impl ScoutSweeps {
    pub fn review_transcript(
        &self,
        project: u32,
        nickname: &str,
    ) -> Option<Vec<velocity_core::thread_view_model::TranscriptBlock>> {
        self.review_swarms.transcript(project, nickname)
    }

    pub fn new(scout_server: PathBuf, reports: PathBuf) -> Self {
        Self {
            review_swarms: crate::review_swarm::ReviewSwarms::new(scout_server, &reports),
            wake: Arc::new(Notify::new()),
        }
    }

    /// Gives every worker's agent the project's AIRC channel under its scout nick (F14).
    pub fn with_airc(mut self, airc: AircLaunch) -> Self {
        self.review_swarms.airc = Some(airc);
        self
    }

    /// Looks for due work now, e.g. after scouts were started or resumed.
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    /// Delivers your instruction from the Duty view's composer (F7: it instructs every agent
    /// in the role).
    pub fn instruct(&self, project: u32, instruction: &str) {
        self.review_swarms.instruct(project, instruction);
        self.wake();
    }

    /// Dismisses `scout` (F7): stops its running session, which kills the agent's process tree.
    pub fn dismiss(&self, project: u32, scout: &str) {
        self.review_swarms.dismiss(project, scout);
        self.wake();
    }

    /// Scouts whose workers are running.
    pub fn running(&self) -> BTreeSet<ScoutKey> {
        self.review_swarms.running()
    }

    /// Runs until the app exits.
    pub async fn run<Host: SweepHost>(self, host: Host) {
        loop {
            self.tick(&host).await;
            let _ = tokio::time::timeout(TICK, self.wake.notified()).await;
        }
    }

    /// Schedules every due Rosen review-swarm/scout worker.
    pub async fn tick<Host: SweepHost>(&self, host: &Host) {
        let rosters = host.rosters();
        self.review_swarms.retain(&rosters);
        for roster in rosters {
            if !roster.project.rosen_in_use() {
                continue;
            }
            let server =
                velocity_rosen::ProjectServer::new(&self.review_swarms.app_data, roster.project.id);
            if matches!(
                velocity_rosen::post_land::after_land(&server).await,
                Ok(true)
            ) {
                self.review_swarms.schedule(host, &roster);
            }
        }
    }
}
