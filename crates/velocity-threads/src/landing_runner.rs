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
use velocity_core::{duty::DutyEventKind, rosen_checker};
use velocity_rosen::landing::{self, LandOutcome, LandRefusal};
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
    fn land_refused(
        &self,
        project: u32,
        change: &str,
        generation: u64,
        message: &str,
    ) -> Result<bool, String>;
}

#[derive(Default)]
struct RunnerState {
    landing_projects: HashSet<u32>,
    refusal_retries: HashMap<(u32, u32), u64>,
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
        let named_generations: HashMap<_, _> = project
            .project
            .threads
            .iter()
            .filter(|thread| named(thread) && thread.rosen_change.is_some())
            .map(|thread| (thread.id, thread.rosen_progress_generation))
            .collect();
        {
            let mut state = self.lock();
            if state.landing_projects.contains(&id) {
                return;
            }
            state
                .refusal_retries
                .retain(|(project, thread), generation| {
                    *project != id || named_generations.get(thread) == Some(generation)
                });
            if named_generations.is_empty() {
                return;
            }
            state.landing_projects.insert(id);
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
                self.lock().refusal_retries.remove(&(id, thread.id));
                if let Err(error) = host.change_landed(id, change, thread.rosen_progress_generation)
                {
                    eprintln!("Rosen land checkpoint for change {change}: {error}");
                }
                let message = rosen_checker::landed_line(change, &commit, started.elapsed());
                host.activity(id, LANDER, DutyEventKind::Merge, message);
                self.wake();
            }
            Ok(LandOutcome::Refused(refusal)) => {
                self.report_refusal(host, project, thread, change, refusal);
            }
            Err(_) => {}
        }
    }

    fn report_refusal<Host: LandingHost>(
        &self,
        host: &Host,
        project: &LandingProject,
        thread: &velocity_store::ThreadItem,
        change: &str,
        refusal: LandRefusal,
    ) {
        let project_id = project.project.id;
        let generation = thread.rosen_progress_generation;
        let key = (project_id, thread.id);
        // A parent may land later in this pass, so give other refusals one more pass.
        if matches!(&refusal, LandRefusal::Other { .. })
            && self.lock().refusal_retries.insert(key, generation) != Some(generation)
        {
            return;
        }
        let branch = crate::launch::base_branch(&project.project);
        let (reason, instructions) = refusal_instructions(change, &branch, refusal);
        let message = format!("Land refused · {reason}\n\n{instructions}");
        match host.land_refused(project_id, change, generation, &message) {
            Ok(recorded) => {
                self.lock().refusal_retries.remove(&key);
                if recorded {
                    let activity = rosen_checker::refused_line(change, &reason);
                    host.activity(project_id, LANDER, DutyEventKind::Changes, activity);
                }
            }
            Err(error) => eprintln!("Rosen land refusal for change {change}: {error}"),
        }
    }
}

fn named(thread: &velocity_store::ThreadItem) -> bool {
    thread.state == ThreadState::Review && thread.review_stage == Some(ReviewStage::Review)
}

fn refusal_instructions(change: &str, base_branch: &str, refusal: LandRefusal) -> (String, String) {
    let name_again = "Name the repaired change with `rosen commit -m \"<what changed>\"` so Velocity can retry the land.";
    match refusal {
        LandRefusal::Regenerate { paths } => (
            format!("regeneration required for {}", paths.join(", ")),
            format!(
                "Run `rosen rebase` in your workspace after any ancestors in the stack land. Regenerate each listed file from the merged inputs. Use the project's generator for any other listed path.\n\n{}\nCheck `rosen status` and clear any remaining stored conflicts with `rosen resolve <path>` after repairing them. {name_again}",
                rosen_checker::regeneration_instructions(&paths),
            ),
        ),
        LandRefusal::Conflict { paths, message } => (
            "merge conflict".into(),
            if paths.is_empty() {
                format!("{message}\n\nRun `rosen rebase` and resolve the conflicts. {name_again}")
            } else {
                rosen_checker::conflict_message(change, base_branch, &paths)
            },
        ),
        LandRefusal::Other { code, message } => (
            code,
            format!(
                "Rosen refused the retry.\n\n{message}\n\nRun `rosen status` and address the refusal. {name_again}"
            ),
        ),
    }
}
