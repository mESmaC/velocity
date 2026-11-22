//! Runs Rosen R12 `[deploy.<step>]` steps after green lands using secrets from
//! Windows Credential Manager (`velocity/deploy/<project>/<NAME>`) or known local
//! CLI logins (`flyctl` for `FLY_API_TOKEN`, `wrangler` for `CLOUDFLARE_API_TOKEN`)
//! (#594, #604).
//!
//! Secret values are loaded only for the step's named `secrets`, injected solely
//! into that deploy process, never passed to agents, and redacted from all output
//! before posting `deploy_result`, recording activity/health, or starting a
//! fix-forward repair thread.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};
use velocity_core::deploy::{
    CliScopeFailure, DeployRunState, DeploySecretSource, DeploySecretStatus, DeployStepSummary,
    ProjectDeploySettings, deploy_checkout_folder_name, deploy_step_position,
    detect_cli_scope_error, extract_secret_sources_line, format_deploy_duration,
    format_deploy_failure_tail, format_deploy_result_output, format_secret_sources,
    missing_secrets_output_with_notes, secret_target, summarize_deploy_error, summarize_output,
    validate_secret_name,
};
use velocity_core::duty::DutyEventKind;
use velocity_core::project_health::short_commit;
use velocity_rosen::deploy::{
    DeployCommandOutcome, DeployDue, MAX_REPORTED_BYTES, RosenDeployStatus, cleanup_deploy_staging,
    deploy_run_state, deploy_steps, parse_deploy_due, parse_deploy_result, platform_matches_host,
    run_deploy_command, running_deploys, submit_deploy_result,
};
use velocity_rosen::deploy_cli::{
    CliSecretLookup, load_cli_fallback_secret, probe_cli_fallback_source,
};
use velocity_rosen::{ProjectEvent, ProjectServer, checker, post_land};
use velocity_store::duty::LandingProject;
use velocity_threads::deploy_repairs::{
    DeployFailure, pending_deploy_repairs, prepare_deploy_repair,
};
use velocity_threads::project_workspace_root;

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::{ManagedThreads, lock, report};

const DEPLOY_ACTOR: &str = "deploy";
const SOURCE: &str = "deploy";
const FAILURE_LOG_LINES: usize = 3;
const FAILURE_LOG_CHARS: usize = 400;

/// Setup failure waiting for its single next-pass retry on the same green commit (#606).
#[derive(Clone, Debug)]
pub(crate) struct PendingSetupRetry {
    pub(crate) due: DeployDue,
    pub(crate) error: String,
    pub(crate) failed_at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SetupRetryDecision {
    RetryOnNextPass,
    Exhausted,
}

#[derive(Default)]
struct ProjectDeployQueue {
    running: Option<i64>,
    worker_active: bool,
    pending: Vec<DeployDue>,
    pending_retries: BTreeMap<(String, String), PendingSetupRetry>,
    retried_once: HashSet<(String, String)>,
}

#[derive(Default)]
pub struct ManagedDeployRunner {
    projects: Mutex<BTreeMap<u32, ProjectDeployQueue>>,
}

impl ManagedDeployRunner {
    fn lock_projects(&self) -> std::sync::MutexGuard<'_, BTreeMap<u32, ProjectDeployQueue>> {
        self.projects
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn enqueue_many(&self, project: u32, dues: Vec<DeployDue>) -> bool {
        if dues.is_empty() {
            return false;
        }
        let mut projects = self.lock_projects();
        let queue = projects.entry(project).or_default();
        let mut added = false;
        for due in dues {
            let duplicate = queue.running == Some(due.id)
                || queue.pending.iter().any(|item| item.id == due.id)
                || queue
                    .pending_retries
                    .values()
                    .any(|item| item.due.id == due.id);
            if !duplicate {
                queue.pending.push(due);
                added = true;
            }
        }
        if !added {
            return false;
        }
        queue.pending.sort_by_key(|item| item.id);
        if queue.worker_active {
            false
        } else {
            queue.worker_active = true;
            true
        }
    }

    fn pending_count(&self, project: u32) -> usize {
        self.lock_projects()
            .get(&project)
            .map_or(0, |queue| queue.pending.len())
    }

    fn pop_next(&self, project: u32, step_order: &[String]) -> Option<DeployDue> {
        let mut projects = self.lock_projects();
        let queue = projects.get_mut(&project)?;
        if queue.pending.is_empty() {
            queue.running = None;
            queue.worker_active = false;
            return None;
        }
        let next_index = next_pending_index(&queue.pending, step_order);
        let due = queue.pending.remove(next_index);
        queue.running = Some(due.id);
        Some(due)
    }

    fn clear_running(&self, project: u32) {
        if let Some(queue) = self.lock_projects().get_mut(&project) {
            queue.running = None;
        }
    }

    fn record_setup_failure(
        &self,
        project: u32,
        due: &DeployDue,
        error: &str,
    ) -> SetupRetryDecision {
        let mut projects = self.lock_projects();
        let queue = projects.entry(project).or_default();
        prune_older_step_retries(queue, &due.step, &due.commit);
        let key = (due.step.clone(), due.commit.clone());
        if queue.retried_once.insert(key.clone()) {
            queue.pending_retries.insert(
                key,
                PendingSetupRetry {
                    due: due.clone(),
                    error: error.to_owned(),
                    failed_at_ms: unix_millis(),
                },
            );
            SetupRetryDecision::RetryOnNextPass
        } else {
            queue.pending_retries.remove(&key);
            SetupRetryDecision::Exhausted
        }
    }

    fn clear_step_retries(&self, project: u32, step: &str, commit: &str) {
        if let Some(queue) = self.lock_projects().get_mut(&project) {
            prune_older_step_retries(queue, step, commit);
            queue
                .pending_retries
                .remove(&(step.to_owned(), commit.to_owned()));
        }
    }

    fn schedule_pending_retries(&self, project: u32) -> (Vec<(String, String)>, bool) {
        let mut projects = self.lock_projects();
        let Some(queue) = projects.get_mut(&project) else {
            return (Vec::new(), false);
        };
        if queue.pending_retries.is_empty() {
            return (Vec::new(), false);
        }
        let retries: Vec<PendingSetupRetry> = std::mem::take(&mut queue.pending_retries)
            .into_values()
            .collect();
        let mut scheduled = Vec::new();
        for entry in retries {
            let duplicate = queue.running == Some(entry.due.id)
                || queue.pending.iter().any(|item| item.id == entry.due.id);
            if !duplicate {
                scheduled.push((entry.due.step.clone(), entry.due.commit.clone()));
                queue.pending.push(entry.due);
            }
        }
        if scheduled.is_empty() {
            return (scheduled, false);
        }
        queue.pending.sort_by_key(|item| item.id);
        let spawn_worker = if queue.worker_active {
            false
        } else {
            queue.worker_active = true;
            true
        };
        (scheduled, spawn_worker)
    }

    fn pending_retry_for(
        &self,
        project: u32,
        step: &str,
        commit: &str,
    ) -> Option<PendingSetupRetry> {
        self.lock_projects()
            .get(&project)?
            .pending_retries
            .get(&(step.to_owned(), commit.to_owned()))
            .cloned()
    }

    fn has_active_work(&self) -> bool {
        self.lock_projects().values().any(|queue| {
            queue.worker_active || queue.running.is_some() || !queue.pending.is_empty()
        })
    }
}

fn prune_older_step_retries(queue: &mut ProjectDeployQueue, step: &str, commit: &str) {
    queue
        .pending_retries
        .retain(|(entry_step, entry_commit), _| entry_step != step || entry_commit == commit);
    queue
        .retried_once
        .retain(|(entry_step, entry_commit)| entry_step != step || entry_commit == commit);
}

fn next_pending_index(pending: &[DeployDue], step_order: &[String]) -> usize {
    if pending.len() <= 1 || step_order.is_empty() {
        return 0;
    }
    let mut commit_min_id: HashMap<&str, i64> = HashMap::new();
    for item in pending {
        commit_min_id
            .entry(item.commit.as_str())
            .and_modify(|min_id| *min_id = (*min_id).min(item.id))
            .or_insert(item.id);
    }
    pending
        .iter()
        .enumerate()
        .min_by_key(|(_, item)| {
            let group_id = commit_min_id
                .get(item.commit.as_str())
                .copied()
                .unwrap_or(item.id);
            let step_index = deploy_step_position(step_order, &item.step);
            (group_id, step_index, item.id)
        })
        .map_or(0, |(index, _)| index)
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

pub fn initialize(application: &AppHandle) {
    application.manage(ManagedDeployRunner::default());
}

/// Whether any project currently has a deploy step running or queued (#609).
pub(crate) fn is_busy(application: &AppHandle) -> bool {
    application
        .try_state::<ManagedDeployRunner>()
        .is_some_and(|runner| runner.has_active_work())
}

/// Handles a live `deploy_due` event from Rosen's event stream (#594, #606).
pub(crate) fn on_deploy_due(application: &AppHandle, project: u32, event: &ProjectEvent) {
    let Some(due) = parse_deploy_due(event) else {
        return;
    };
    enqueue_deploys(application, project, vec![due]);
}

/// Handles a `deploy_result` event from Rosen's event stream: closes open deploy
/// repairs when a step passes and keeps Project Health synchronized (#594).
pub(crate) fn on_deploy_result(application: &AppHandle, project: u32, event: &ProjectEvent) {
    let Some(result) = parse_deploy_result(event) else {
        return;
    };
    if let Some(runner) = application.try_state::<ManagedDeployRunner>() {
        runner.clear_step_retries(project, &result.step, &result.commit);
    }
    if result.status == RosenDeployStatus::Passed {
        close_repairs(application, project, &result.step);
        let sources = extract_secret_sources_line(&result.output);
        crate::project_health::record_deploy_passed(
            application,
            project,
            &result.step,
            &result.commit,
            sources,
        );
    } else if result.status == RosenDeployStatus::Skipped {
        crate::project_health::record_deploy_skipped(
            application,
            project,
            &result.step,
            &result.commit,
            &result.output,
        );
    }
}

/// Resumes any `running` deploy steps left in Rosen's `deploy_runs` table across
/// restarts and launches any saved deploy repair threads that never finished starting.
pub(crate) fn recover_running(application: &AppHandle, project: u32) {
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        report(resume_pending_repairs(&handle, project));
        let Ok(server) = crate::rosen::server(&handle, project).await else {
            return;
        };
        let reading = server.clone();
        let running = tauri::async_runtime::spawn_blocking(move || running_deploys(&reading))
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or_default();
        enqueue_deploys(&handle, project, running);
    });
}

/// Re-enqueues any deploy step that hit a setup error for its single retry on the next pass (#606).
pub(crate) fn retry_setup_failures(application: &AppHandle, project: u32) {
    let Some(runner) = application.try_state::<ManagedDeployRunner>() else {
        return;
    };
    let (scheduled, spawn_worker) = runner.schedule_pending_retries(project);
    for (step, commit) in scheduled {
        crate::diagnostics::record(
            SOURCE,
            &format!(
                "Project {project} retrying deploy `{step}` at {} after setup error",
                short_commit(&commit)
            ),
        );
    }
    if spawn_worker {
        spawn_project_worker(application, project);
    }
}

/// Returns the pending setup-error retry for `(project, step, commit)` when waiting for the next pass (#606).
pub(crate) fn pending_setup_retry(
    application: &AppHandle,
    project: u32,
    step: &str,
    commit: &str,
) -> Option<PendingSetupRetry> {
    application
        .try_state::<ManagedDeployRunner>()?
        .pending_retry_for(project, step, commit)
}

fn enqueue_deploys(application: &AppHandle, project: u32, dues: Vec<DeployDue>) {
    let Some(runner) = application.try_state::<ManagedDeployRunner>() else {
        return;
    };
    if runner.enqueue_many(project, dues) {
        spawn_project_worker(application, project);
    }
}

fn spawn_project_worker(application: &AppHandle, project: u32) {
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        drain_project_deploys(&handle, project).await;
    });
}

async fn drain_project_deploys(application: &AppHandle, project: u32) {
    loop {
        let needs_order = application
            .try_state::<ManagedDeployRunner>()
            .is_some_and(|runner| runner.pending_count(project) > 1);
        let step_order = if needs_order {
            load_step_order(application, project).await
        } else {
            Vec::new()
        };
        let next = application
            .try_state::<ManagedDeployRunner>()
            .and_then(|runner| runner.pop_next(project, &step_order));
        let Some(due) = next else {
            return;
        };
        execute_due(application, project, due).await;
        if let Some(runner) = application.try_state::<ManagedDeployRunner>() {
            runner.clear_running(project);
        }
    }
}

async fn load_step_order(application: &AppHandle, project: u32) -> Vec<String> {
    let Ok(server) = crate::rosen::server(application, project).await else {
        return Vec::new();
    };
    deploy_steps(&server)
        .await
        .map(|steps| steps.into_iter().map(|step| step.name).collect())
        .unwrap_or_default()
}

async fn is_run_still_active(server: &ProjectServer, run_id: i64) -> bool {
    let reading = server.clone();
    let state = tauri::async_runtime::spawn_blocking(move || deploy_run_state(&reading, run_id))
        .await
        .ok()
        .and_then(Result::ok)
        .flatten();
    !matches!(
        state,
        Some(DeployRunState::Passed | DeployRunState::Failed | DeployRunState::Skipped)
    )
}

async fn execute_due(application: &AppHandle, project: u32, due: DeployDue) {
    if !platform_matches_host(&due.platforms) {
        crate::diagnostics::record(
            SOURCE,
            &format!(
                "Project {project} deploy `{}` (run {}) targets {:?}; leaving for matching checker",
                due.step, due.id, due.platforms
            ),
        );
        return;
    }
    let Some(landing) = load_landing_project(application, project) else {
        return;
    };
    let Ok(server) = crate::rosen::server(application, project).await else {
        return;
    };
    if !is_run_still_active(&server, due.id).await {
        return;
    }
    let loaded = match load_step_secrets(&landing.project.name, project, &due.secrets).await {
        Ok(SecretLoad::Ready(loaded)) => loaded,
        Ok(SecretLoad::Missing(missing)) => {
            handle_missing_secrets(application, &server, project, &due, &missing).await;
            return;
        }
        Err(error) => {
            handle_setup_failure(application, &server, project, &due, &error).await;
            return;
        }
    };
    let folder = match stage_deploy_folder(&landing, &server, &due.step, &due.commit).await {
        Ok(folder) => folder,
        Err(error) => {
            handle_setup_failure(application, &server, project, &due, &error).await;
            return;
        }
    };
    let LoadedSecrets { pairs, sources } = loaded;
    announce_running(application, project, &due, &sources);
    let started = Instant::now();
    let step_due = due.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        run_deploy_command(&folder, &step_due, &pairs)
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|inner| inner.map_err(|error| error.to_string()));
    finish_due(
        application,
        &server,
        project,
        &due,
        &sources,
        started.elapsed(),
        outcome,
    )
    .await;
}

async fn finish_due(
    application: &AppHandle,
    server: &ProjectServer,
    project: u32,
    due: &DeployDue,
    sources: &[(String, DeploySecretSource)],
    elapsed: Duration,
    outcome: Result<DeployCommandOutcome, String>,
) {
    match outcome {
        Ok(DeployCommandOutcome::Passed { output }) => {
            handle_passed(application, server, project, due, sources, elapsed, &output).await;
        }
        Ok(DeployCommandOutcome::Failed {
            exit_code,
            stopped_reason,
            output,
        }) => {
            handle_failed(
                application,
                server,
                project,
                due,
                sources,
                elapsed,
                exit_code,
                stopped_reason,
                &output,
            )
            .await;
        }
        Err(error) => {
            handle_setup_failure(application, server, project, due, &error).await;
        }
    }
}

fn announce_running(
    application: &AppHandle,
    project: u32,
    due: &DeployDue,
    sources: &[(String, DeploySecretSource)],
) {
    let short = short_commit(&due.commit);
    let source_summary = format_secret_sources(sources);
    let log_line = match &source_summary {
        Some(summary) => format!(
            "Project {project} deploy `{}` at {short} using {summary}",
            due.step
        ),
        None => format!("Project {project} deploy `{}` at {short}", due.step),
    };
    crate::diagnostics::record(SOURCE, &log_line);
    let message = match &source_summary {
        Some(summary) => format!("Deploying {} at {short} ({summary})…", due.step),
        None => format!("Deploying {} at {short}…", due.step),
    };
    crate::rosen_landing::publish_project_activity(
        application,
        project,
        DEPLOY_ACTOR,
        DutyEventKind::Wait,
        message,
    );
    crate::project_health::record_deploy_running(application, project, &due.step, &due.commit);
}

async fn submit_and_log_result(
    server: &ProjectServer,
    project: u32,
    due: &DeployDue,
    status: RosenDeployStatus,
    output: &str,
) {
    let label = status.as_str();
    match submit_deploy_result(server, due.id, &due.step, &due.commit, status, output).await {
        Ok(Some(_)) => {}
        Ok(None) => crate::diagnostics::record(
            SOURCE,
            &format!(
                "Project {project} deploy `{}` (run {}) {label} result was refused by Rosen (already finished or superseded)",
                due.step, due.id
            ),
        ),
        Err(error) => crate::diagnostics::record(
            SOURCE,
            &format!(
                "Project {project} could not report {label} deploy `{}` (run {}): {error}",
                due.step, due.id
            ),
        ),
    }
}

async fn handle_missing_secrets(
    application: &AppHandle,
    server: &ProjectServer,
    project: u32,
    due: &DeployDue,
    missing: &[(String, String, Option<String>)],
) {
    clear_step_retries(application, project, due);
    let message = missing_secrets_output_with_notes(missing);
    let short = short_commit(&due.commit);
    crate::diagnostics::record(
        SOURCE,
        &format!(
            "Project {project} deploy `{}` skipped at {short}: {message}",
            due.step
        ),
    );
    submit_and_log_result(server, project, due, RosenDeployStatus::Skipped, &message).await;
    crate::rosen_landing::publish_project_activity(
        application,
        project,
        DEPLOY_ACTOR,
        DutyEventKind::Changes,
        format!("Deploy {} skipped at {short} · {message}", due.step),
    );
    crate::project_health::record_deploy_skipped(
        application,
        project,
        &due.step,
        &due.commit,
        &message,
    );
}

async fn handle_passed(
    application: &AppHandle,
    server: &ProjectServer,
    project: u32,
    due: &DeployDue,
    sources: &[(String, DeploySecretSource)],
    elapsed: Duration,
    redacted_output: &str,
) {
    clear_step_retries(application, project, due);
    let short = short_commit(&due.commit);
    let took = format_deploy_duration(elapsed);
    let source_summary = format_secret_sources(sources);
    let reported_output =
        format_deploy_result_output(sources, redacted_output, None, MAX_REPORTED_BYTES);
    let log_line = match &source_summary {
        Some(summary) => format!(
            "Project {project} deploy `{}` deployed {short} in {took} ({summary})",
            due.step
        ),
        None => format!(
            "Project {project} deploy `{}` deployed {short} in {took}",
            due.step
        ),
    };
    crate::diagnostics::record(SOURCE, &log_line);
    submit_and_log_result(
        server,
        project,
        due,
        RosenDeployStatus::Passed,
        &reported_output,
    )
    .await;
    close_repairs(application, project, &due.step);
    let activity = match &source_summary {
        Some(summary) => format!("Deployed {} at {short} in {took} · {summary}", due.step),
        None => format!("Deployed {} at {short} in {took}", due.step),
    };
    crate::rosen_landing::publish_project_activity(
        application,
        project,
        DEPLOY_ACTOR,
        DutyEventKind::Approve,
        activity,
    );
    crate::project_health::record_deploy_passed(
        application,
        project,
        &due.step,
        &due.commit,
        source_summary.as_deref(),
    );
}

#[allow(clippy::too_many_arguments)]
async fn handle_failed(
    application: &AppHandle,
    server: &ProjectServer,
    project: u32,
    due: &DeployDue,
    sources: &[(String, DeploySecretSource)],
    elapsed: Duration,
    exit_code: Option<i32>,
    stopped_reason: Option<String>,
    redacted_output: &str,
) {
    clear_step_retries(application, project, due);
    let scope_failure = detect_cli_scope_error(redacted_output, sources);
    let reported_output = format_deploy_result_output(
        sources,
        redacted_output,
        scope_failure.as_ref().map(|item| item.output_note.as_str()),
        MAX_REPORTED_BYTES,
    );
    submit_and_log_result(
        server,
        project,
        due,
        RosenDeployStatus::Failed,
        &reported_output,
    )
    .await;
    if let Some(scope) = scope_failure {
        handle_cli_scope_failure(application, project, due, elapsed, &scope);
        return;
    }
    handle_repairable_failure(
        application,
        project,
        due,
        elapsed,
        exit_code,
        stopped_reason,
        &reported_output,
        redacted_output,
    )
    .await;
}

fn handle_cli_scope_failure(
    application: &AppHandle,
    project: u32,
    due: &DeployDue,
    elapsed: Duration,
    scope: &CliScopeFailure,
) {
    let short = short_commit(&due.commit);
    let took = format_deploy_duration(elapsed);
    let action = format!("CLI login may lack a required scope; {}", scope.suggestion);
    crate::diagnostics::record(
        SOURCE,
        &format!(
            "Project {project} deploy `{}` failed at {short} in {took}: {} ({action})",
            due.step, scope.api_error
        ),
    );
    crate::rosen_landing::publish_project_activity(
        application,
        project,
        DEPLOY_ACTOR,
        DutyEventKind::Changes,
        format!(
            "Deploy {} failed at {short} · {} · {}",
            due.step, scope.api_error, scope.suggestion
        ),
    );
    crate::project_health::record_deploy_failed(
        application,
        project,
        &due.step,
        &due.commit,
        &action,
        Some(scope.api_error.clone()),
        None,
    );
    show_failure_toast(
        application,
        &due.step,
        short,
        &format!("{} — {}", scope.api_error, scope.suggestion),
    );
}

#[allow(clippy::too_many_arguments)]
async fn handle_repairable_failure(
    application: &AppHandle,
    project: u32,
    due: &DeployDue,
    elapsed: Duration,
    exit_code: Option<i32>,
    stopped_reason: Option<String>,
    reported_output: &str,
    redacted_output: &str,
) {
    let short = short_commit(&due.commit);
    let took = format_deploy_duration(elapsed);
    let tail = format_failure_log_tail(redacted_output, exit_code, stopped_reason.as_deref());
    let (action, thread) = open_or_track_repair(
        application,
        project,
        DeployFailure {
            run_id: due.id,
            step: due.step.clone(),
            commit: due.commit.clone(),
            command: due.command.clone(),
            exit_code,
            stopped_reason,
            redacted_output: reported_output.to_owned(),
        },
    )
    .await;
    crate::diagnostics::record(
        SOURCE,
        &format!(
            "Project {project} deploy `{}` failed at {short} in {took}: {tail} ({action})",
            due.step
        ),
    );
    crate::rosen_landing::publish_project_activity(
        application,
        project,
        DEPLOY_ACTOR,
        DutyEventKind::Changes,
        format!("Deploy {} failed at {short} · {action}", due.step),
    );
    crate::project_health::record_deploy_failed(
        application,
        project,
        &due.step,
        &due.commit,
        &action,
        summarize_deploy_error(redacted_output, 180),
        thread,
    );
    show_failure_toast(application, &due.step, short, &action);
}

fn format_failure_log_tail(
    redacted_output: &str,
    exit_code: Option<i32>,
    stopped_reason: Option<&str>,
) -> String {
    format_deploy_failure_tail(redacted_output, FAILURE_LOG_LINES, FAILURE_LOG_CHARS)
        .unwrap_or_else(|| {
            if let Some(reason) = stopped_reason {
                reason.to_owned()
            } else if let Some(code) = exit_code {
                format!("exited with status {code}")
            } else {
                "command failed with no output".to_owned()
            }
        })
}

async fn open_or_track_repair(
    application: &AppHandle,
    project: u32,
    failure: DeployFailure,
) -> (String, Option<u32>) {
    let step = failure.step.clone();
    let handle = application.clone();
    let repair_result = tauri::async_runtime::spawn_blocking(move || {
        start_deploy_repair(&handle, project, &failure)
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|inner| inner);
    match repair_result {
        Ok(Some(thread_id)) => (
            "Started fix-forward thread for deploy failure".to_owned(),
            Some(thread_id),
        ),
        Ok(None) => (
            "Fix-forward thread already open for this deploy failure".to_owned(),
            open_repair_thread(application, project, &step),
        ),
        Err(reason) => (
            format!("Could not start fix-forward thread: {reason}"),
            None,
        ),
    }
}

async fn handle_setup_failure(
    application: &AppHandle,
    server: &ProjectServer,
    project: u32,
    due: &DeployDue,
    error: &str,
) {
    let decision = application
        .try_state::<ManagedDeployRunner>()
        .map_or(SetupRetryDecision::Exhausted, |runner| {
            runner.record_setup_failure(project, due, error)
        });
    if decision == SetupRetryDecision::RetryOnNextPass {
        record_setup_retry(application, project, due, error);
        return;
    }
    let short = short_commit(&due.commit);
    crate::diagnostics::record(
        SOURCE,
        &format!(
            "Project {project} deploy `{}` failed at {short}: setup error after retry: {error}",
            due.step
        ),
    );
    submit_and_log_result(server, project, due, RosenDeployStatus::Failed, error).await;
    crate::rosen_landing::publish_project_activity(
        application,
        project,
        DEPLOY_ACTOR,
        DutyEventKind::Changes,
        format!("Deploy {} failed at {short} · {error}", due.step),
    );
    crate::project_health::record_deploy_failed(
        application,
        project,
        &due.step,
        &due.commit,
        "Deploy step failed before command finished after retry",
        summarize_output(error, 180),
        None,
    );
}

fn record_setup_retry(application: &AppHandle, project: u32, due: &DeployDue, error: &str) {
    let short = short_commit(&due.commit);
    crate::diagnostics::record(
        SOURCE,
        &format!(
            "Project {project} deploy `{}` setup error at {short}: {error} (retrying once on next pass)",
            due.step
        ),
    );
    crate::rosen_landing::publish_project_activity(
        application,
        project,
        DEPLOY_ACTOR,
        DutyEventKind::Wait,
        format!(
            "Deploy {} setup error at {short} · retrying once on next pass",
            due.step
        ),
    );
    crate::project_health::record_deploy_retrying(
        application,
        project,
        &due.step,
        &due.commit,
        error,
    );
}

fn clear_step_retries(application: &AppHandle, project: u32, due: &DeployDue) {
    if let Some(runner) = application.try_state::<ManagedDeployRunner>() {
        runner.clear_step_retries(project, &due.step, &due.commit);
    }
}

struct LoadedSecrets {
    pairs: Vec<(String, String)>,
    sources: Vec<(String, DeploySecretSource)>,
}

enum SecretLoad {
    Ready(LoadedSecrets),
    Missing(Vec<(String, String, Option<String>)>),
}

async fn load_step_secrets(
    project_name: &str,
    project_id: u32,
    names: &[String],
) -> Result<SecretLoad, String> {
    if names.is_empty() {
        return Ok(SecretLoad::Ready(LoadedSecrets {
            pairs: Vec::new(),
            sources: Vec::new(),
        }));
    }
    let project_name = project_name.to_owned();
    let names = names.to_vec();
    tauri::async_runtime::spawn_blocking(move || {
        let mut pairs = Vec::with_capacity(names.len());
        let mut sources = Vec::with_capacity(names.len());
        let mut missing = Vec::new();
        for name in &names {
            let target = secret_target(&project_name, project_id, name)?;
            match velocity_collapse::load_deploy_secret(&target, name) {
                Ok(Some(secret)) => {
                    pairs.push((name.clone(), secret.expose().to_owned()));
                    sources.push((name.clone(), DeploySecretSource::CredentialManager));
                }
                Ok(None) => match load_cli_fallback_secret(name) {
                    CliSecretLookup::Found { source, value } => {
                        pairs.push((name.clone(), value.expose().to_owned()));
                        sources.push((name.clone(), source));
                    }
                    CliSecretLookup::Unavailable { reason, .. } => {
                        missing.push((name.clone(), target, Some(reason)));
                    }
                    CliSecretLookup::Unsupported => missing.push((name.clone(), target, None)),
                },
                Err(error) => {
                    return Err(format!("Could not read secret {name} ({target}): {error}"));
                }
            }
        }
        if missing.is_empty() {
            Ok(SecretLoad::Ready(LoadedSecrets { pairs, sources }))
        } else {
            Ok(SecretLoad::Missing(missing))
        }
    })
    .await
    .map_err(|error| error.to_string())?
}

async fn stage_deploy_folder(
    landing: &LandingProject,
    server: &ProjectServer,
    step: &str,
    commit: &str,
) -> Result<PathBuf, String> {
    let folder = project_workspace_root(&landing.project, &landing.worktrees)
        .ok_or_else(|| "Could not resolve the project's workspace folder for deploy".to_owned())?
        .join(deploy_checkout_folder_name(step));
    let clean_before = folder.clone();
    tauri::async_runtime::spawn_blocking(move || cleanup_deploy_staging(&clean_before))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
    if let Err(error) = post_land::stage_commit(server, commit, &folder).await {
        let clean_failed = folder.clone();
        let _ = tauri::async_runtime::spawn_blocking(move || cleanup_deploy_staging(&clean_failed))
            .await;
        return Err(error.to_string());
    }
    let apply_folder = folder.clone();
    let applied =
        tauri::async_runtime::spawn_blocking(move || checker::apply_staged(&apply_folder))
            .await
            .map_err(|error| error.to_string())
            .and_then(|inner| inner.map_err(|error| error.to_string()));
    if let Err(error) = applied {
        let clean_failed = folder.clone();
        let _ = tauri::async_runtime::spawn_blocking(move || cleanup_deploy_staging(&clean_failed))
            .await;
        return Err(error);
    }
    Ok(folder)
}

fn load_landing_project(application: &AppHandle, project: u32) -> Option<LandingProject> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()?
    .landing_projects()
    .into_iter()
    .find(|item| item.project.id == project)
}

fn close_repairs(application: &AppHandle, project: u32, step: &str) {
    if let Ok(mut store) = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    ) {
        let _ = store.close_deploy_repairs(project, step);
    }
}

fn open_repair_thread(application: &AppHandle, project: u32, step: &str) -> Option<u32> {
    lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()?
    .project(project)?
    .deploy_repairs
    .iter()
    .rev()
    .find(|repair| !repair.closed && repair.step == step)
    .map(|repair| repair.thread)
}

fn start_deploy_repair(
    application: &AppHandle,
    project: u32,
    failure: &DeployFailure,
) -> Result<Option<u32>, String> {
    let planned = {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let existing = store.project(project).and_then(|item| {
            item.deploy_repairs
                .iter()
                .find(|repair| {
                    repair.run_id == failure.run_id
                        || (!repair.closed
                            && repair.step == failure.step
                            && repair.commit == failure.commit)
                })
                .map(|repair| (project, repair.thread))
        });
        if let Some(key) = existing
            && lock(
                application.state::<ManagedThreads>().inner(),
                "thread sessions",
            )?
            .is_live(key)
        {
            return Ok(None);
        }
        let planned = prepare_deploy_repair(&mut store, project, failure)?;
        if let Some((_, thread)) = &planned {
            store
                .update_thread(project, *thread, |thread| {
                    thread.state = velocity_store::ThreadState::Working;
                    thread.activity = velocity_store::AgentActivity::Executing;
                })
                .map_err(|error| error.to_string())?;
        }
        planned
    };
    let Some((plan, thread)) = planned else {
        return Ok(None);
    };
    crate::state::save_now(application)?;
    crate::threads::start_planned_with_selection(application, plan, thread, false)?;
    Ok(Some(thread))
}

fn resume_pending_repairs(application: &AppHandle, project: u32) -> Result<(), String> {
    let pending = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        pending_deploy_repairs(&store, project)
    };
    for (plan, thread) in pending {
        let live = lock(
            application.state::<ManagedThreads>().inner(),
            "thread sessions",
        )?
        .is_live((project, thread));
        if !live {
            report(crate::threads::start_planned_with_selection(
                application,
                plan,
                thread,
                false,
            ));
        }
    }
    Ok(())
}

fn show_failure_toast(application: &AppHandle, step: &str, short_commit: &str, action: &str) {
    if let Ok(mut layout) = lock(application.state::<MainLayout>().inner(), "layout") {
        layout.show_toast(
            format!("Deploy `{step}` failed at {short_commit}: {action}"),
            None,
        );
        report(crate::layout::publish(
            application,
            &layout.snapshot(),
            None,
        ));
    }
}

/// `get_project_deploy_settings` (§3.1): reads `[deploy]` steps from `rosen.toml`
/// and reports which named secrets exist in Windows Credential Manager without
/// ever returning secret values (#594).
#[tauri::command]
#[specta::specta]
pub async fn get_project_deploy_settings(
    application: AppHandle,
    project_identifier: u32,
) -> Result<Option<ProjectDeploySettings>, String> {
    load_project_deploy_settings(&application, project_identifier).await
}

/// `set_project_deploy_secret` (§3.1): saves or replaces one deploy secret under
/// `velocity/deploy/<project>/<NAME>` in Windows Credential Manager (#594).
#[tauri::command]
#[specta::specta]
pub async fn set_project_deploy_secret(
    application: AppHandle,
    project_identifier: u32,
    name: String,
    value: String,
) -> Result<ProjectDeploySettings, String> {
    let valid_name = validate_secret_name(&name)?;
    let project_name = project_name_for(&application, project_identifier)?;
    let target = secret_target(&project_name, project_identifier, &valid_name)?;
    let save_name = valid_name.clone();
    tauri::async_runtime::spawn_blocking(move || {
        velocity_collapse::save_deploy_secret(&target, &save_name, &value)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())??;
    {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        store
            .record_deploy_secret_name(project_identifier, &valid_name)
            .map_err(|error| error.to_string())?;
    }
    load_project_deploy_settings(&application, project_identifier)
        .await?
        .ok_or_else(|| "Deploy settings are available only for Rosen projects".into())
}

/// `clear_project_deploy_secret` (§3.1): deletes one deploy secret from Windows
/// Credential Manager under `velocity/deploy/<project>/<NAME>` (#594).
#[tauri::command]
#[specta::specta]
pub async fn clear_project_deploy_secret(
    application: AppHandle,
    project_identifier: u32,
    name: String,
) -> Result<ProjectDeploySettings, String> {
    let valid_name = validate_secret_name(&name)?;
    let project_name = project_name_for(&application, project_identifier)?;
    let target = secret_target(&project_name, project_identifier, &valid_name)?;
    let clear_name = valid_name.clone();
    tauri::async_runtime::spawn_blocking(move || {
        velocity_collapse::forget_deploy_secret(&target, &clear_name)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())??;
    {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        store
            .remove_deploy_secret_name(project_identifier, &valid_name)
            .map_err(|error| error.to_string())?;
    }
    load_project_deploy_settings(&application, project_identifier)
        .await?
        .ok_or_else(|| "Deploy settings are available only for Rosen projects".into())
}

fn project_name_for(application: &AppHandle, project_identifier: u32) -> Result<String, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let project = store
        .project(project_identifier)
        .ok_or_else(|| format!("Project {project_identifier} was not found"))?;
    Ok(project.name.clone())
}

async fn load_project_deploy_settings(
    application: &AppHandle,
    project_identifier: u32,
) -> Result<Option<ProjectDeploySettings>, String> {
    let (rosen, project_name, remembered) = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let Some(project) = store.project(project_identifier) else {
            return Ok(None);
        };
        (
            project.rosen_in_use(),
            project.name.clone(),
            project.deploy_secret_names.clone(),
        )
    };
    if !rosen {
        return Ok(None);
    }
    let (steps, config_error) = match crate::rosen::server(application, project_identifier).await {
        Ok(server) => match deploy_steps(&server).await {
            Ok(steps) => (steps, None),
            Err(error) => (Vec::new(), Some(error.to_string())),
        },
        Err(error) => (Vec::new(), Some(error)),
    };
    let secret_rows = collect_secret_statuses(
        project_name.clone(),
        project_identifier,
        &steps,
        &remembered,
    )
    .await?;
    Ok(Some(ProjectDeploySettings {
        project_identifier,
        project_name,
        supported: velocity_collapse::deploy_credentials_supported(),
        steps,
        secrets: secret_rows,
        config_error,
    }))
}

async fn collect_secret_statuses(
    project_name: String,
    project_identifier: u32,
    steps: &[DeployStepSummary],
    remembered: &[String],
) -> Result<Vec<DeploySecretStatus>, String> {
    let mut usage: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for step in steps {
        for secret in &step.secrets {
            usage
                .entry(secret.clone())
                .or_default()
                .push(step.name.clone());
        }
    }
    for name in remembered {
        usage.entry(name.clone()).or_default();
    }
    tauri::async_runtime::spawn_blocking(move || {
        let mut statuses = Vec::with_capacity(usage.len());
        for (name, used_by) in usage {
            let target = secret_target(&project_name, project_identifier, &name)?;
            let stored = velocity_collapse::has_deploy_secret(&target, &name)
                .map_err(|error| error.to_string())?;
            let fallback = if stored {
                None
            } else {
                probe_cli_fallback_source(&name).map(|source| source.label().to_owned())
            };
            statuses.push(DeploySecretStatus {
                name,
                target,
                stored,
                used_by,
                fallback,
            });
        }
        Ok(statuses)
    })
    .await
    .map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_due(id: i64, step: &str, commit: &str) -> DeployDue {
        DeployDue {
            id,
            step: step.to_owned(),
            commit: commit.to_owned(),
            from: None,
            command: format!("deploy-{step}"),
            paths: Vec::new(),
            secrets: Vec::new(),
            platforms: Vec::new(),
        }
    }

    // Risk (#606): concurrent deploy steps raced on one checkout and setup errors were never
    // retried. Queued steps for a project must serialize in `rosen.toml` declaration order, and
    // a setup error must retry exactly once on the next pass for the same green commit.
    #[test]
    fn runner_serializes_in_declaration_order_and_retries_setup_once() {
        let runner = ManagedDeployRunner::default();
        let commit = "1a2acaffbb2b0000000000000000000000000000";
        let edge = sample_due(11, "edge", commit);
        let server = sample_due(12, "server", commit);

        assert!(runner.enqueue_many(3, vec![edge.clone()]));
        assert!(!runner.enqueue_many(3, vec![server.clone()]));

        let order = vec!["server".to_owned(), "edge".to_owned()];
        let first = runner.pop_next(3, &order);
        assert_eq!(
            first.as_ref().map(|item| item.step.as_str()),
            Some("server")
        );

        assert_eq!(
            runner.record_setup_failure(3, &server, "leftover staging"),
            SetupRetryDecision::RetryOnNextPass
        );
        runner.clear_running(3);

        let second = runner.pop_next(3, &order);
        assert_eq!(second.as_ref().map(|item| item.step.as_str()), Some("edge"));
        runner.clear_running(3);
        assert!(runner.pop_next(3, &order).is_none());

        let (scheduled, spawn_worker) = runner.schedule_pending_retries(3);
        assert_eq!(scheduled, vec![("server".to_owned(), commit.to_owned())]);
        assert!(spawn_worker);

        let retried = runner.pop_next(3, &order);
        assert_eq!(
            retried.as_ref().map(|item| item.step.as_str()),
            Some("server")
        );
        assert_eq!(
            runner.record_setup_failure(3, &server, "still failing"),
            SetupRetryDecision::Exhausted
        );
        runner.clear_running(3);
        assert!(runner.schedule_pending_retries(3).0.is_empty());
    }
}
