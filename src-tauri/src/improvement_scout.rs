//! Tauri edge for the machine-wide improvement scout (SPEC.md §1, F7 Development speed, #622).
//!
//! Polls machine-level dispatch slot occupancy and Rosen land history every 30 seconds and
//! delegates scheduling, digest collection, and bounded session execution to
//! `velocity_threads::ImprovementScouts`.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tauri::{AppHandle, Manager};
use velocity_core::improvement_scout::ToolSuiteRepository;
use velocity_github::IssueHost;
use velocity_store::ProjectItem;
use velocity_store::continuous_dispatch::busy_count;
use velocity_threads::{ImprovementFleetSnapshot, ImprovementScouts, TargetRepositoryConfig};

use crate::state::ManagedStateStore;
use crate::threads::lock;

const POLL_INTERVAL: Duration = Duration::from_secs(30);
const LOG_SOURCE: &str = "improvement scout";

/// Starts the machine-wide improvement scout monitor for the life of the app.
pub fn initialize(application: &AppHandle) -> anyhow::Result<()> {
    let executable = std::env::current_exe()?;
    let app_data = application.path().app_data_dir()?;
    let airc = crate::airc_access::launch(application).ok();
    let scouts = ImprovementScouts::new(executable, app_data, airc, |message| {
        crate::diagnostics::record(LOG_SOURCE, message);
    });
    application.manage(scouts.clone());
    let handle = application.clone();
    tauri::async_runtime::spawn(async move {
        loop {
            if !crate::rosen::paused(&handle)
                && let Some(snapshot) = build_fleet_snapshot(&handle)
            {
                scouts.tick(snapshot).await;
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    });
    Ok(())
}

fn build_fleet_snapshot(application: &AppHandle) -> Option<ImprovementFleetSnapshot> {
    let (settings, projects, host_project) = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )
        .ok()?;
        let settings = store.settings();
        let projects = store.state().projects.clone();
        let host_project = choose_host_project(&projects)
            .map(|project| (project.clone(), store.worktree_preferences(project)));
        (settings, projects, host_project)
    };
    if projects.is_empty() {
        return None;
    }
    let now = unix_seconds();
    let (slots_free_and_nothing_dispatchable, external_idle_since) =
        evaluate_machine_slots(application, &projects, now);
    let fallback_dir = host_project
        .as_ref()
        .map(|(project, _)| PathBuf::from(&project.repository_path))
        .unwrap_or_else(|| PathBuf::from(&projects[0].repository_path));
    let repositories = resolve_tool_suite_repositories(&projects, &fallback_dir);
    Some(ImprovementFleetSnapshot {
        settings,
        projects,
        host_project,
        repositories,
        slots_free_and_nothing_dispatchable,
        external_idle_since,
    })
}

fn evaluate_machine_slots(
    application: &AppHandle,
    projects: &[ProjectItem],
    now: u64,
) -> (bool, Option<u64>) {
    let mut free_slots: u32 = 0;
    let mut ready_total: usize = 0;
    let mut earliest_idle: Option<u64> = None;
    for project in projects {
        if !project.spec_locked
            || !project.rosen_in_use()
            || project.continuous_dispatch.paused
            || project.continuous_dispatch.slots == 0
        {
            continue;
        }
        let busy = busy_count(project);
        free_slots =
            free_slots.saturating_add(project.continuous_dispatch.slots.saturating_sub(busy));
        if let Some(health) =
            crate::continuous_dispatch::health_snapshot(application, project.id, now)
        {
            ready_total = ready_total.saturating_add(health.ready_count);
        }
        if let Some(since) = project.continuous_dispatch.idle_since_secs {
            earliest_idle = Some(earliest_idle.map_or(since, |existing| existing.min(since)));
        }
    }
    let idle = free_slots > 0 && ready_total == 0;
    (idle, idle.then_some(earliest_idle).flatten())
}

fn choose_host_project(projects: &[ProjectItem]) -> Option<&ProjectItem> {
    projects
        .iter()
        .find(|project| {
            project.rosen_in_use()
                && matches_repository(project, ToolSuiteRepository::Velocity.repository())
        })
        .or_else(|| projects.iter().find(|project| project.rosen_in_use()))
}

fn resolve_tool_suite_repositories(
    projects: &[ProjectItem],
    fallback_dir: &std::path::Path,
) -> Vec<TargetRepositoryConfig> {
    ToolSuiteRepository::ALL
        .into_iter()
        .map(|target| {
            let repo_name = target.repository();
            if let Some(project) = projects
                .iter()
                .find(|project| matches_repository(project, repo_name))
            {
                TargetRepositoryConfig {
                    repository: repo_name.to_owned(),
                    host: project.effective_issue_host(),
                    directory: PathBuf::from(&project.repository_path),
                }
            } else {
                TargetRepositoryConfig {
                    repository: repo_name.to_owned(),
                    host: IssueHost::GitHub,
                    directory: fallback_dir.to_path_buf(),
                }
            }
        })
        .collect()
}

fn matches_repository(project: &ProjectItem, repository: &str) -> bool {
    project
        .collapse_repository
        .as_deref()
        .is_some_and(|remote| remote.eq_ignore_ascii_case(repository))
        || project
            .github_remote
            .as_deref()
            .is_some_and(|remote| remote.eq_ignore_ascii_case(repository))
}

fn unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
