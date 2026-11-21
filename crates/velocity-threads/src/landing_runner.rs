//! Lands named Rosen changes and executes the post-land jobs offered by Rosen.

mod bindings_gate;
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

use bindings_gate::{Gate, GateNote};
pub use build_repairs::BuildRepairs;
use post_land::VerifyNote;

const TICK: Duration = Duration::from_secs(30);
const LANDER: &str = "Rosen";
const NAME_AGAIN: &str = "Name the repaired change with `rosen commit -m \"<what changed>\"` so Velocity can retry the land.";

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
    /// Called when a post-land or integration check was stopped for running too long (#486, #509).
    fn post_land_stopped(
        &self,
        _project: u32,
        _commit: &str,
        _command: &str,
        _stopped: &rosen_checker::StoppedCheck,
    ) {
    }
    /// Checks the project's mirror tip and catches up before a land (#520).
    fn catch_up_before_land(
        &self,
        _project: u32,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send + '_>> {
        Box::pin(async {})
    }
    /// Called when orphaned landed commits on `project` have a green integration build (#519).
    fn orphaned_verified(&self, _project: u32) {}
}

#[derive(Default)]
struct RunnerState {
    landing_projects: HashSet<u32>,
    refusal_retries: HashMap<(u32, u32), u64>,
    /// Named changes whose bindings are being or were checked, by project and thread.
    bindings: HashMap<(u32, u32), GateNote>,
    /// Projects whose bindings check folder is in use.
    checking_bindings: HashSet<u32>,
    verifying_projects: HashSet<(u32, &'static str)>,
    verifying: HashMap<(u32, &'static str), VerifyNote>,
    reporting_projects: HashSet<u32>,
    reporting_notes: HashMap<u32, String>,
    repairing_projects: HashMap<u32, bool>,
    repair_deadlines: HashMap<u32, u64>,
    /// Projects with an urgent integration build requested by the release flow (#521).
    forced_integrations: HashMap<u32, Option<String>>,
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

    /// Whether a post-land or integration verification pass is currently running for `project`.
    pub fn is_verifying(&self, project: u32) -> bool {
        self.lock()
            .verifying_projects
            .iter()
            .any(|&(id, _)| id == project)
    }

    /// Requests an immediate integration build for `project` (bypassing the 10-land / 2-hour
    /// batch wait), optionally targeting `commit` so the release gate can pass (#521).
    pub fn request_integration(&self, project: u32, commit: Option<String>) {
        {
            let mut state = self.lock();
            state
                .forced_integrations
                .entry(project)
                .and_modify(|existing| {
                    if commit.is_some() {
                        *existing = commit.clone();
                    }
                })
                .or_insert(commit);
        }
        self.wake();
    }

    pub(super) fn has_forced_integration(&self, project: u32) -> bool {
        self.lock().forced_integrations.contains_key(&project)
    }

    pub(super) fn take_forced_integration(&self, project: u32) -> Option<Option<String>> {
        self.lock().forced_integrations.remove(&project)
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
            state.bindings.retain(|(project, thread), note| {
                *project != id || named_generations.get(thread) == Some(&note.generation)
            });
            if named_generations.is_empty() {
                return;
            }
            state.landing_projects.insert(id);
        }
        let (runner, host) = (self.clone(), host.clone());
        tokio::spawn(async move {
            let server = ProjectServer::new(&runner.app_data, id);
            if let Ok(base_after_land) = velocity_rosen::post_land::after_land(&server).await {
                for thread in project
                    .project
                    .threads
                    .iter()
                    .filter(|thread| named(thread))
                {
                    let Some(change) = &thread.rosen_change else {
                        continue;
                    };
                    // On a before-land base, only a rosen.toml-only switch to after-land
                    // gets a land request; unrelated changes keep Rosen's before-land gate (#524).
                    let land_allowed = base_after_land
                        || matches!(
                            velocity_rosen::post_land::switches_to_after_land(&server, change)
                                .await,
                            Ok(true)
                        );
                    if land_allowed {
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
        host.catch_up_before_land(id).await;
        let gate_started = Instant::now();
        match self
            .bindings_gate(host, server, project, thread, change)
            .await
        {
            Gate::Clear => {}
            Gate::Waiting => return,
            Gate::Stale => {
                let (reason, repair) = rosen_checker::stale_bindings_refusal();
                let instructions = format!("{repair}{NAME_AGAIN}");
                return self.send_refusal(host, project, thread, change, (reason, instructions));
            }
        }
        if gate_started.elapsed() >= Duration::from_secs(5) {
            host.catch_up_before_land(id).await;
        }
        self.warn_signature_references(host, server, id, change)
            .await;
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

    /// Warns in the landing feed when `change` alters a public signature, IPC command, or
    /// IPC event type referenced by another open or recently landed change (#542).
    async fn warn_signature_references<Host: LandingHost>(
        &self,
        host: &Host,
        server: &ProjectServer,
        project: u32,
        change: &str,
    ) {
        let Ok(executable) = std::env::current_exe() else {
            return;
        };
        let app_data = self.app_data.clone();
        let project_id = project.to_string();
        let endpoint = server.endpoint().to_string();
        let change_owned = change.to_owned();
        let lookup = tokio::task::spawn_blocking(move || {
            velocity_graph::change_signature_warnings(
                &executable,
                &app_data,
                &project_id,
                &endpoint,
                &change_owned,
            )
        });
        let Ok(Ok(warnings)) = tokio::time::timeout(Duration::from_secs(3), lookup).await else {
            return;
        };
        if warnings.is_empty() {
            return;
        }
        let short = change.get(..8).unwrap_or(change);
        let summary = velocity_graph::signature_impact::format_warning_summary(&warnings);
        host.activity(
            project,
            LANDER,
            DutyEventKind::Wait,
            format!("Change {short} · {summary}"),
        );
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
        if matches!(&refusal, LandRefusal::Other { .. })
            && !is_before_land_gate(&refusal)
            && self.retry_later(key, generation)
        {
            return;
        }
        let branch = crate::launch::base_branch(&project.project);
        let refusal = refusal_instructions(change, &branch, refusal);
        self.send_refusal(host, project, thread, change, refusal);
    }

    /// Whether a refusal Rosen gave no repair for is tried again before its author hears
    /// of it. A parent may land later in this pass, so every such refusal gets one more
    /// pass. A parent may also wait on a bindings check (#503): while one runs in the
    /// project the refusal is not counted, so that one more pass comes after the check.
    fn retry_later(&self, key: (u32, u32), generation: u64) -> bool {
        let checking = self.checking_bindings(key.0);
        let mut state = self.lock();
        if checking {
            state.refusal_retries.remove(&key);
            return true;
        }
        state.refusal_retries.insert(key, generation) != Some(generation)
    }

    /// Gives the change back to its author with the reason and the repair.
    fn send_refusal<Host: LandingHost>(
        &self,
        host: &Host,
        project: &LandingProject,
        thread: &velocity_store::ThreadItem,
        change: &str,
        (reason, instructions): (String, String),
    ) {
        let project_id = project.project.id;
        let generation = thread.rosen_progress_generation;
        let key = (project_id, thread.id);
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

pub fn refusal_instructions(
    change: &str,
    base_branch: &str,
    refusal: LandRefusal,
) -> (String, String) {
    let name_again = NAME_AGAIN;
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
        LandRefusal::Other { code, message } => match code.as_str() {
            "unapproved" | "no_receipt" | "receipt_stale" => (
                code,
                format!(
                    "Rosen's before-land policy refused the land.\n\n{message}\n\nTo complete the switch to `after-land`, use Switch to after-land in the Rosen status pane, or approve this head (`rosen review approve {change} --head <head>`), submit the pre-land receipt (`rosen receipt submit {change} --head <head> --base <base>`) and run `rosen land {change}`. Do not name the change again, because a new snapshot voids approvals and receipts."
                ),
            ),
            _ => (
                code,
                format!(
                    "Rosen refused the retry.\n\n{message}\n\nRun `rosen status` and address the refusal. {name_again}"
                ),
            ),
        },
    }
}

fn is_before_land_gate(refusal: &LandRefusal) -> bool {
    matches!(
        refusal,
        LandRefusal::Other { code, .. }
            if matches!(code.as_str(), "unapproved" | "no_receipt" | "receipt_stale")
    )
}
