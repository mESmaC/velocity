//! Lands named Rosen changes and executes the post-land jobs offered by Rosen.

mod build_repairs;
mod check_feed;
mod post_land;
mod post_land_issues;

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::sync::Notify;
use velocity_core::duty::DutyEventKind;
use velocity_rosen::landing::{self, LandOutcome};
use velocity_rosen::{ProjectServer, checker};
use velocity_store::duty::LandingProject;
use velocity_store::{ReviewStage, ThreadState};

pub use build_repairs::BuildRepairs;
use post_land::VerifyNote;

const TICK: Duration = Duration::from_secs(30);
const LANDER: &str = "Rosen";

pub trait LandingHost: Clone + Send + Sync + 'static {
    fn projects(&self) -> Vec<LandingProject>;
    fn integration_idle(&self, project: u32) -> bool;
    fn repair_builds(&self, project: u32, observation: BuildRepairs) -> Result<(), String>;
    fn change_landed(&self, project: u32, change: &str, generation: u64) -> Result<(), String>;
    fn activity(&self, project: u32, actor: &str, kind: DutyEventKind, message: String);
    fn merge_conflict(&self, project: u32, thread: u32, change: &str, message: &str);
}

#[derive(Default)]
struct RunnerState {
    landing_projects: HashSet<u32>,
    verifying_projects: HashSet<(u32, &'static str)>,
    verifying: HashMap<(u32, &'static str), VerifyNote>,
    reporting_projects: HashSet<u32>,
    reporting_notes: HashMap<u32, String>,
    repairing_projects: HashMap<u32, bool>,
    repair_deadlines: HashMap<u32, u64>,
}

#[derive(Clone)]
pub struct RosenLanding {
    app_data: PathBuf,
    state: Arc<Mutex<RunnerState>>,
    wake: Arc<Notify>,
}

impl RosenLanding {
    pub fn new(app_data: PathBuf) -> Self {
        Self {
            app_data,
            state: Arc::default(),
            wake: Arc::default(),
        }
    }

    pub fn wake(&self) {
        self.wake.notify_one();
    }

    pub async fn run<Host: LandingHost>(self, host: Host) {
        loop {
            for project in host.projects() {
                self.repair_builds(&host, project.project.id);
                self.verify_landed(&host, &project);
                self.start_landing(&host, project);
            }
            let _ = tokio::time::timeout(TICK, self.wake.notified()).await;
        }
    }

    fn lock(&self) -> MutexGuard<'_, RunnerState> {
        self.state.lock().unwrap_or_else(|error| error.into_inner())
    }

    fn start_landing<Host: LandingHost>(&self, host: &Host, project: LandingProject) {
        let id = project.project.id;
        let has_named_work = project
            .project
            .threads
            .iter()
            .any(|thread| thread.rosen_change.is_some() && named(thread));
        if !has_named_work || !self.lock().landing_projects.insert(id) {
            return;
        }
        let (runner, host) = (self.clone(), host.clone());
        tokio::spawn(async move {
            let server = ProjectServer::new(&runner.app_data, id);
            if matches!(
                velocity_rosen::post_land::after_land(&server).await,
                Ok(true)
            ) {
                for thread in project
                    .project
                    .threads
                    .iter()
                    .filter(|thread| named(thread))
                {
                    if let Some(change) = &thread.rosen_change {
                        runner.land(&host, &server, &project, thread, change).await;
                    }
                }
            }
            runner.lock().landing_projects.remove(&id);
        });
    }

    async fn land<Host: LandingHost>(
        &self,
        host: &Host,
        server: &ProjectServer,
        project: &LandingProject,
        thread: &velocity_store::ThreadItem,
        change: &str,
    ) {
        let id = project.project.id;
        let started = Instant::now();
        match landing::land(server, checker::CHECKER, change).await {
            Ok(LandOutcome::Landed { commit }) => {
                if let Err(error) = host.change_landed(id, change, thread.rosen_progress_generation)
                {
                    eprintln!("Rosen land checkpoint for change {change}: {error}");
                }
                let message =
                    velocity_core::rosen_checker::landed_line(change, &commit, started.elapsed());
                host.activity(id, LANDER, DutyEventKind::Merge, message);
                self.wake();
            }
            Ok(LandOutcome::Refused { code, .. }) if code == "merge_conflict" => {
                report_conflict(host, server, project, thread.id, change).await;
            }
            // A pending parent can become landable after a later pass. Rosen records the
            // refusal and the change stays named so the next pass tries it again.
            Ok(LandOutcome::Refused { .. }) | Err(_) => {}
        }
    }
}

fn named(thread: &velocity_store::ThreadItem) -> bool {
    thread.state == ThreadState::Review && thread.review_stage == Some(ReviewStage::Review)
}

async fn report_conflict<Host: LandingHost>(
    host: &Host,
    server: &ProjectServer,
    project: &LandingProject,
    thread: u32,
    change: &str,
) {
    let location = crate::launch::Location::new(&project.project, &project.worktrees);
    let Ok(root) = location.project_root() else {
        return;
    };
    let Ok(merge) =
        checker::stage_merge_result(server, change, &root.join("landing-conflicts")).await
    else {
        return;
    };
    if merge.conflicts.is_empty() {
        return;
    }
    let branch = crate::launch::base_branch(&project.project);
    let message = velocity_core::rosen_checker::conflict_message(change, &branch, &merge.conflicts);
    host.merge_conflict(project.project.id, thread, change, &message);
}
