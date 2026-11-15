//! Native background adapter for F8 continuous dispatch. It never uses the selected
//! project's issue rail, so changing projects cannot redirect a background launch.
use crate::state::ManagedStateStore;
use crate::threads::{ManagedThreads, lock, report};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use velocity_github::{GitHubIssue, IssueClient};
use velocity_store::continuous_dispatch::can_finish_empty_dispatch;
use velocity_store::continuous_dispatch::has_capacity as project_has_capacity;
use velocity_store::{ProjectItem, VersionControl};
use velocity_threads::continuous_dispatch::{DispatchView, ordered_issues};

#[derive(Clone, Default)]
struct Backlog {
    repository: String,
    issues: Vec<GitHubIssue>,
    error: Option<String>,
}

type Backlogs = Mutex<BTreeMap<u32, Backlog>>;

#[tauri::command]
#[specta::specta]
pub async fn get_continuous_dispatch(
    application: AppHandle,
    project_identifier: u32,
) -> Result<DispatchView, String> {
    crate::layout::off_ui_thread(move || view(&application, project_identifier)).await
}

#[tauri::command]
#[specta::specta]
pub async fn set_continuous_dispatch(
    application: AppHandle,
    project_identifier: u32,
    slots: u32,
    paused: bool,
) -> Result<DispatchView, String> {
    crate::layout::off_ui_thread(move || {
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .set_continuous_dispatch(project_identifier, slots, paused)
        .map_err(|error| error.to_string())?;
        view(&application, project_identifier)
    })
    .await
}

fn view(application: &AppHandle, identifier: u32) -> Result<DispatchView, String> {
    let backlog = lock(application.state::<Backlogs>().inner(), "dispatch backlog")?
        .get(&identifier)
        .cloned()
        .unwrap_or_default();
    let claimed = claimed_issues(application, identifier)?;
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let project = store.project(identifier).ok_or("The project was removed")?;
    let issues = ordered_issues(project, &backlog.repository, &backlog.issues, &claimed);
    Ok(DispatchView::new(project, &issues, backlog.error))
}

pub fn initialize(application: &AppHandle) {
    application.manage(Backlogs::default());
    let application = application.clone();
    std::thread::spawn(move || monitor(application));
}

struct Reader {
    directory: String,
    remote: Option<String>,
    client: IssueClient,
    refreshed: Option<Instant>,
    backlog: Backlog,
}

fn monitor(application: AppHandle) {
    let mut readers = BTreeMap::new();
    let mut failed = BTreeMap::<u32, Instant>::new();
    loop {
        if !crate::rosen::paused(&application) {
            match projects(&application) {
                Ok(projects) => {
                    for project in projects {
                        if failed
                            .get(&project.id)
                            .is_some_and(|time| time.elapsed() < Duration::from_secs(30))
                        {
                            continue;
                        }
                        let result = pass(&application, &project, &mut readers);
                        if let Err(error) = result {
                            failed.insert(project.id, Instant::now());
                            report(save_error(&application, project.id, error));
                        } else {
                            failed.remove(&project.id);
                        }
                    }
                }
                Err(error) => report::<()>(Err(error)),
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn projects(application: &AppHandle) -> Result<Vec<ProjectItem>, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    Ok(store
        .state()
        .projects
        .iter()
        .filter(|project| {
            !project.continuous_dispatch.cleanup.is_empty()
                || (project.version_control == VersionControl::Rosen
                    && (project.continuous_dispatch.slots > 0
                        || store
                            .active_project()
                            .is_some_and(|active| active.id == project.id)))
        })
        .cloned()
        .collect())
}

fn pass(
    application: &AppHandle,
    project: &ProjectItem,
    readers: &mut BTreeMap<u32, Reader>,
) -> Result<(), String> {
    cleanup_empty_threads(application, project)?;
    let reader = reader(project, readers)?;
    refresh_backlog(application, project.id, reader)?;
    if project.continuous_dispatch.slots == 0 {
        return Ok(());
    }
    finish_empty_threads(application, project)?;
    if project.continuous_dispatch.paused || !project.spec_locked || reader.backlog.error.is_some()
    {
        return Ok(());
    }
    let claimed = claimed_issues(application, project.id)?;
    let issues = ordered_issues(
        project,
        reader.client.repository(),
        &reader.backlog.issues,
        &claimed,
    );
    for issue in issues {
        if !has_capacity(application, project.id)? {
            break;
        }
        // Re-read before claiming: a label, decision, ownership or close may have changed.
        let fresh = reader
            .client
            .get(issue.number)
            .map_err(|error| error.to_string())?;
        start(application, project.id, reader.client.repository(), &fresh)?;
    }
    Ok(())
}

fn refresh_backlog(
    application: &AppHandle,
    project: u32,
    reader: &mut Reader,
) -> Result<(), String> {
    if reader
        .refreshed
        .is_none_or(|time| time.elapsed() >= Duration::from_secs(30))
    {
        // Failed reads also back off: an unavailable API must not be hammered every tick.
        reader.refreshed = Some(Instant::now());
        match reader.client.open_issues() {
            Ok(issues) => {
                reader.backlog = Backlog {
                    repository: reader.client.repository().into(),
                    issues,
                    error: None,
                }
            }
            Err(error) => {
                reader.backlog.issues.clear();
                reader.backlog.error = Some(error.to_string());
            }
        }
        lock(application.state::<Backlogs>().inner(), "dispatch backlog")?
            .insert(project, reader.backlog.clone());
    }
    Ok(())
}

fn reader<'a>(
    project: &ProjectItem,
    readers: &'a mut BTreeMap<u32, Reader>,
) -> Result<&'a mut Reader, String> {
    let changed = readers.get(&project.id).is_none_or(|reader| {
        reader.directory != project.repository_path
            || reader.remote != project.github_remote
            || (reader.remote.is_none()
                && !reader
                    .client
                    .is_current(Path::new(&project.repository_path)))
    });
    if changed {
        let directory = Path::new(&project.repository_path);
        let client = match &project.github_remote {
            Some(remote) => IssueClient::for_repository(directory, remote),
            None => IssueClient::discover(directory),
        }
        .map_err(|error| error.to_string())?;
        readers.insert(
            project.id,
            Reader {
                directory: project.repository_path.clone(),
                remote: project.github_remote.clone(),
                client,
                refreshed: None,
                backlog: Backlog::default(),
            },
        );
    }
    readers
        .get_mut(&project.id)
        .ok_or_else(|| "Dispatch reader was removed".into())
}

fn has_capacity(application: &AppHandle, identifier: u32) -> Result<bool, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    Ok(store.project(identifier).is_some_and(project_has_capacity))
}

fn start(
    application: &AppHandle,
    identifier: u32,
    repository: &str,
    issue: &GitHubIssue,
) -> Result<(), String> {
    let claimed = claimed_issues(application, identifier)?;
    let (plan, thread) = {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let project = store.project(identifier).ok_or("The project was removed")?;
        if !project_has_capacity(project) {
            return Ok(());
        }
        if ordered_issues(project, repository, std::slice::from_ref(issue), &claimed).is_empty() {
            return Ok(());
        }
        let plan = velocity_threads::dispatch::plan_project_issue_dispatch(
            &store, identifier, repository, issue, false,
        )
        .map_err(|error| error.to_string())?;
        let thread = store
            .record_continuous_dispatch(
                identifier,
                plan.launch.thread.clone(),
                plan.issue,
                issue.updated_at.clone(),
            )
            .map_err(|error| error.to_string())?;
        (plan.launch, thread)
    };
    crate::threads::start_planned_with_selection(application, plan, thread, false)?;
    Ok(())
}

fn claimed_issues(application: &AppHandle, identifier: u32) -> Result<Vec<u32>, String> {
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let channel = velocity_airc::Channel::open(&data, &identifier.to_string())
        .map_err(|error| error.to_string())?;
    Ok(channel
        .claims()
        .map_err(|error| error.to_string())?
        .iter()
        .filter_map(|claim| claim.key().strip_prefix("issue:")?.parse().ok())
        .collect())
}

fn save_error(application: &AppHandle, identifier: u32, error: String) -> Result<(), String> {
    let mut backlogs = lock(application.state::<Backlogs>().inner(), "dispatch backlog")?;
    backlogs.entry(identifier).or_default().error = Some(error);
    Ok(())
}

fn finish_empty_threads(application: &AppHandle, project: &ProjectItem) -> Result<(), String> {
    for thread in project.threads.iter().filter(|thread| {
        can_finish_empty_dispatch(thread)
            && thread.supervision.turns > 0
            && thread.rosen_change.is_some()
    }) {
        finish_empty_thread(application, project.id, thread)?;
    }
    Ok(())
}

fn finish_empty_thread(
    application: &AppHandle,
    project: u32,
    thread: &velocity_store::ThreadItem,
) -> Result<(), String> {
    let key = (project, thread.id);
    let lease = crate::threads::action_lock(application, key)?;
    let Ok(_guard) = lease.try_lock() else {
        return Ok(());
    };
    // The project snapshot preceded the lease; recovery or a send may have changed it.
    let Some(thread) = current_empty_thread(application, key)? else {
        return Ok(());
    };
    if !lock(
        application.state::<ManagedThreads>().inner(),
        "thread sessions",
    )?
    .is_at_step_boundary(key)
    {
        return Ok(());
    }
    let Some(change) = thread.rosen_change.as_deref() else {
        return Ok(());
    };
    let Some(root) = thread.worktree_path.as_deref() else {
        return Ok(());
    };
    let empty = tauri::async_runtime::block_on(async {
        let server = crate::rosen::server(application, project).await?;
        velocity_rosen::dispatch::ended_without_changes(&server, change, Path::new(root))
            .await
            .map_err(|error| error.to_string())
    })?;
    if empty && crate::threads::stop_idle_thread(application, key)? {
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .finish_empty_dispatch(project, thread.id)
        .map_err(|error| error.to_string())?;
        crate::threads::refresh(application, key, false)?;
    }
    Ok(())
}

fn current_empty_thread(
    application: &AppHandle,
    key: velocity_threads::ThreadKey,
) -> Result<Option<velocity_store::ThreadItem>, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    Ok(store
        .project(key.0)
        .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
        .filter(|thread| can_finish_empty_dispatch(thread))
        .cloned())
}

fn cleanup_empty_threads(application: &AppHandle, project: &ProjectItem) -> Result<(), String> {
    for identifier in &project.continuous_dispatch.cleanup {
        let Some(thread) = project
            .threads
            .iter()
            .find(|thread| thread.id == *identifier)
        else {
            lock(
                application.state::<ManagedStateStore>().inner(),
                "persisted state",
            )?
            .finish_dispatch_cleanup(project.id, *identifier)
            .map_err(|error| error.to_string())?;
            continue;
        };
        let Some(change) = thread.rosen_change.as_deref() else {
            continue;
        };
        tauri::async_runtime::block_on(async {
            if let Some(root) = thread.worktree_path.as_deref() {
                crate::rosen::stop_watch(application, Path::new(root)).await;
            }
            let server = crate::rosen::server(application, project.id).await?;
            velocity_rosen::dispatch::abandon_finished(&server, change)
                .await
                .map_err(|error| error.to_string())
        })?;
        crate::airc_claims::release(application, (project.id, *identifier))?;
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .finish_dispatch_cleanup(project.id, *identifier)
        .map_err(|error| error.to_string())?;
    }
    Ok(())
}
