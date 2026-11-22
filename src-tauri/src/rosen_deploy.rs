//! Runs Rosen R12 `[deploy.<step>]` steps after green lands using secrets from
//! Windows Credential Manager (`velocity/deploy/<project>/<NAME>`) (#594).
//!
//! Secret values are loaded only for the step's named `secrets`, injected solely
//! into that deploy process, never passed to agents, and redacted from all output
//! before posting `deploy_result`, recording activity/health, or starting a
//! fix-forward repair thread.

use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::Mutex;

use tauri::{AppHandle, Manager};
use velocity_core::deploy::{
    DeploySecretStatus, DeployStepSummary, ProjectDeploySettings, missing_secrets_output,
    secret_target, summarize_output, validate_secret_name,
};
use velocity_core::duty::DutyEventKind;
use velocity_core::project_health::short_commit;
use velocity_rosen::deploy::{
    DeployCommandOutcome, DeployDue, RosenDeployStatus, deploy_steps, parse_deploy_due,
    parse_deploy_result, platform_matches_host, run_deploy_command, running_deploys,
    submit_deploy_result,
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
const DEPLOY_FOLDER: &str = "deploy-checkout";
const SOURCE: &str = "deploy";

#[derive(Default)]
pub struct ManagedDeployRunner {
    active_runs: Mutex<HashSet<(u32, i64)>>,
}

impl ManagedDeployRunner {
    fn begin(&self, project: u32, run_id: i64) -> bool {
        self.active_runs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert((project, run_id))
    }

    fn finish(&self, project: u32, run_id: i64) {
        self.active_runs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&(project, run_id));
    }
}

pub fn initialize(application: &AppHandle) {
    application.manage(ManagedDeployRunner::default());
}

/// Handles a live or replayed `deploy_due` event from Rosen's event stream (#594).
pub(crate) fn on_deploy_due(application: &AppHandle, project: u32, event: &ProjectEvent) {
    let Some(due) = parse_deploy_due(event) else {
        return;
    };
    spawn_deploy(application, project, due);
}

/// Handles a `deploy_result` event from Rosen's event stream: closes open deploy
/// repairs when a step passes and keeps Project Health synchronized (#594).
pub(crate) fn on_deploy_result(application: &AppHandle, project: u32, event: &ProjectEvent) {
    let Some(result) = parse_deploy_result(event) else {
        return;
    };
    if result.status == RosenDeployStatus::Passed {
        close_repairs(application, project, &result.step);
        crate::project_health::record_deploy_passed(
            application,
            project,
            &result.step,
            &result.commit,
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
        for due in running {
            spawn_deploy(&handle, project, due);
        }
    });
}

fn spawn_deploy(application: &AppHandle, project: u32, due: DeployDue) {
    let Some(runner) = application.try_state::<ManagedDeployRunner>() else {
        return;
    };
    if !runner.begin(project, due.id) {
        return;
    }
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        let run_id = due.id;
        execute_due(&handle, project, due).await;
        if let Some(runner) = handle.try_state::<ManagedDeployRunner>() {
            runner.finish(project, run_id);
        }
    });
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
    let secrets = match load_step_secrets(&landing.project.name, project, &due.secrets).await {
        Ok(SecretLoad::Ready(pairs)) => pairs,
        Ok(SecretLoad::Missing(missing)) => {
            handle_missing_secrets(application, &server, project, &due, &missing).await;
            return;
        }
        Err(error) => {
            handle_setup_failure(application, &server, project, &due, &error).await;
            return;
        }
    };
    let folder = match stage_deploy_folder(&landing, &server, &due.commit).await {
        Ok(folder) => folder,
        Err(error) => {
            handle_setup_failure(application, &server, project, &due, &error).await;
            return;
        }
    };
    announce_running(application, project, &due);
    let step_due = due.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        run_deploy_command(&folder, &step_due, &secrets)
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|inner| inner.map_err(|error| error.to_string()));
    finish_due(application, &server, project, &due, outcome).await;
}

async fn finish_due(
    application: &AppHandle,
    server: &ProjectServer,
    project: u32,
    due: &DeployDue,
    outcome: Result<DeployCommandOutcome, String>,
) {
    match outcome {
        Ok(DeployCommandOutcome::Passed { output }) => {
            handle_passed(application, server, project, due, &output).await;
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

fn announce_running(application: &AppHandle, project: u32, due: &DeployDue) {
    let short = short_commit(&due.commit);
    crate::rosen_landing::publish_project_activity(
        application,
        project,
        DEPLOY_ACTOR,
        DutyEventKind::Wait,
        format!("Deploying {} at {short}…", due.step),
    );
    crate::project_health::record_deploy_running(application, project, &due.step, &due.commit);
}

async fn handle_missing_secrets(
    application: &AppHandle,
    server: &ProjectServer,
    project: u32,
    due: &DeployDue,
    missing: &[(String, String)],
) {
    let message = missing_secrets_output(missing);
    let short = short_commit(&due.commit);
    crate::diagnostics::record(
        SOURCE,
        &format!("Project {project} {}: {message}", due.step),
    );
    if let Err(error) = submit_deploy_result(
        server,
        due.id,
        &due.step,
        &due.commit,
        RosenDeployStatus::Skipped,
        &message,
    )
    .await
    {
        crate::diagnostics::record(
            SOURCE,
            &format!(
                "Project {project} could not report skipped deploy `{}`: {error}",
                due.step
            ),
        );
    }
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
    redacted_output: &str,
) {
    let short = short_commit(&due.commit);
    if let Err(error) = submit_deploy_result(
        server,
        due.id,
        &due.step,
        &due.commit,
        RosenDeployStatus::Passed,
        redacted_output,
    )
    .await
    {
        crate::diagnostics::record(
            SOURCE,
            &format!(
                "Project {project} could not report passed deploy `{}`: {error}",
                due.step
            ),
        );
    }
    close_repairs(application, project, &due.step);
    crate::rosen_landing::publish_project_activity(
        application,
        project,
        DEPLOY_ACTOR,
        DutyEventKind::Approve,
        format!("Deployed {} at {short}", due.step),
    );
    crate::project_health::record_deploy_passed(application, project, &due.step, &due.commit);
}

async fn handle_failed(
    application: &AppHandle,
    server: &ProjectServer,
    project: u32,
    due: &DeployDue,
    exit_code: Option<i32>,
    stopped_reason: Option<String>,
    redacted_output: &str,
) {
    let short = short_commit(&due.commit);
    if let Err(error) = submit_deploy_result(
        server,
        due.id,
        &due.step,
        &due.commit,
        RosenDeployStatus::Failed,
        redacted_output,
    )
    .await
    {
        crate::diagnostics::record(
            SOURCE,
            &format!(
                "Project {project} could not report failed deploy `{}`: {error}",
                due.step
            ),
        );
    }
    let failure = DeployFailure {
        run_id: due.id,
        step: due.step.clone(),
        commit: due.commit.clone(),
        command: due.command.clone(),
        exit_code,
        stopped_reason,
        redacted_output: redacted_output.to_owned(),
    };
    let handle = application.clone();
    let repair_result = tauri::async_runtime::spawn_blocking(move || {
        start_deploy_repair(&handle, project, &failure)
    })
    .await
    .map_err(|error| error.to_string())
    .and_then(|inner| inner);
    let (action, thread) = match repair_result {
        Ok(Some(thread_id)) => (
            "Started fix-forward thread for deploy failure".to_owned(),
            Some(thread_id),
        ),
        Ok(None) => (
            "Fix-forward thread already open for this deploy failure".to_owned(),
            open_repair_thread(application, project, &due.step),
        ),
        Err(reason) => (
            format!("Could not start fix-forward thread: {reason}"),
            None,
        ),
    };
    crate::diagnostics::record(
        SOURCE,
        &format!(
            "Project {project} deploy `{}` failed at {short}: {action}",
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
        summarize_output(redacted_output, 180),
        thread,
    );
    show_failure_toast(application, &due.step, short, &action);
}

async fn handle_setup_failure(
    application: &AppHandle,
    server: &ProjectServer,
    project: u32,
    due: &DeployDue,
    error: &str,
) {
    let short = short_commit(&due.commit);
    let _ = submit_deploy_result(
        server,
        due.id,
        &due.step,
        &due.commit,
        RosenDeployStatus::Failed,
        error,
    )
    .await;
    crate::diagnostics::record(
        SOURCE,
        &format!(
            "Project {project} deploy `{}` setup error at {short}: {error}",
            due.step
        ),
    );
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
        "Deploy step failed before command finished",
        summarize_output(error, 180),
        None,
    );
}

enum SecretLoad {
    Ready(Vec<(String, String)>),
    Missing(Vec<(String, String)>),
}

async fn load_step_secrets(
    project_name: &str,
    project_id: u32,
    names: &[String],
) -> Result<SecretLoad, String> {
    if names.is_empty() {
        return Ok(SecretLoad::Ready(Vec::new()));
    }
    let project_name = project_name.to_owned();
    let names = names.to_vec();
    tauri::async_runtime::spawn_blocking(move || {
        let mut loaded = Vec::with_capacity(names.len());
        let mut missing = Vec::new();
        for name in &names {
            let target = secret_target(&project_name, project_id, name)?;
            match velocity_collapse::load_deploy_secret(&target, name) {
                Ok(Some(secret)) => loaded.push((name.clone(), secret.expose().to_owned())),
                Ok(None) => missing.push((name.clone(), target)),
                Err(error) => {
                    return Err(format!("Could not read secret {name} ({target}): {error}"));
                }
            }
        }
        if missing.is_empty() {
            Ok(SecretLoad::Ready(loaded))
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
    commit: &str,
) -> Result<PathBuf, String> {
    let folder = project_workspace_root(&landing.project, &landing.worktrees)
        .ok_or_else(|| "Could not resolve the project's workspace folder for deploy".to_owned())?
        .join(DEPLOY_FOLDER);
    post_land::stage_commit(server, commit, &folder)
        .await
        .map_err(|error| error.to_string())?;
    let apply_folder = folder.clone();
    tauri::async_runtime::spawn_blocking(move || checker::apply_staged(&apply_folder))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())?;
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
            statuses.push(DeploySecretStatus {
                name,
                target,
                stored,
                used_by,
            });
        }
        Ok(statuses)
    })
    .await
    .map_err(|error| error.to_string())?
}
