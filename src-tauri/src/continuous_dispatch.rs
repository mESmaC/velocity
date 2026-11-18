//! Native background adapter for F8 continuous dispatch. It never uses the selected
//! project's issue rail, so changing projects cannot redirect a background launch.
use crate::state::ManagedStateStore;
use crate::threads::{ManagedThreads, lock, report};
use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use velocity_github::{GitHubIssue, IssueClient};
use velocity_store::continuous_dispatch::has_capacity as project_has_capacity;
use velocity_store::continuous_dispatch::{busy_count, can_finish_empty_dispatch};
use velocity_store::{ProjectItem, TaskAttribution, VersionControl};
use velocity_threads::continuous_dispatch::{
    DispatchView, DispatchWork, dispatchable_task, ordered_issues, ordered_work, plan_task,
};

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
        .cloned();
    let issues_known = backlog
        .as_ref()
        .is_some_and(|backlog| backlog.error.is_none());
    let backlog = backlog.unwrap_or_default();
    let claimed = claimed_pieces(application, identifier)?;
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let project = store.project(identifier).ok_or("The project was removed")?;
    let work = ordered_work(
        project,
        &backlog.repository,
        &backlog.issues,
        &claimed,
        issues_known,
    );
    Ok(DispatchView::new(project, &work, backlog.error))
}

pub(crate) fn integration_idle(application: &AppHandle, identifier: u32) -> Result<bool, String> {
    let known = lock(application.state::<Backlogs>().inner(), "dispatch backlog")?
        .get(&identifier)
        .filter(|backlog| backlog.error.is_none())
        .cloned();
    let Some(backlog) = known else {
        return Ok(false);
    };
    let claimed = claimed_pieces(application, identifier)?;
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let Some(project) = store.project(identifier) else {
        return Ok(false);
    };
    Ok(busy_count(project) == 0
        && ordered_work(
            project,
            &backlog.repository,
            &backlog.issues,
            &claimed,
            true,
        )
        .is_empty())
}

pub fn initialize(application: &AppHandle) {
    application.manage(Backlogs::default());
    let application = application.clone();
    std::thread::spawn(move || monitor(application));
}

struct Reader {
    directory: String,
    remote: Option<String>,
    client: Option<IssueClient>,
    refreshed: Option<Instant>,
    report_retry: BTreeMap<u32, Instant>,
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
                || !project.continuous_dispatch.no_change_reports.is_empty()
                || (project.version_control == VersionControl::Rosen
                    && (project.continuous_dispatch.slots > 0
                        || project
                            .threads
                            .iter()
                            .any(|thread| thread.rosen_change.is_some())
                        || project.threads.iter().any(|thread| {
                            thread.started_by_dispatch
                                && thread.state == velocity_store::ThreadState::Working
                        })
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
    let reader = reader(project, readers);
    report_no_change_issues(application, project, reader)?;
    cleanup_empty_threads(application, project)?;
    finish_empty_threads(application, project)?;
    if project_has_capacity(project) {
        // Local work takes precedence even while GitHub is slow or unavailable.
        start_ready(application, project.id, &Backlog::default(), None, false)?;
    }
    refresh_backlog(application, project.id, reader)?;
    start_ready(
        application,
        project.id,
        &reader.backlog,
        reader.client.as_ref(),
        reader.backlog.error.is_none(),
    )
}

fn start_ready(
    application: &AppHandle,
    identifier: u32,
    backlog: &Backlog,
    client: Option<&IssueClient>,
    issues_known: bool,
) -> Result<(), String> {
    if !has_capacity(application, identifier)? {
        return Ok(());
    }
    let claimed = claimed_pieces(application, identifier)?;
    let work = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let project = store.project(identifier).ok_or("The project was removed")?;
        ordered_work(
            project,
            &backlog.repository,
            &backlog.issues,
            &claimed,
            issues_known,
        )
    };
    for item in work {
        if !has_capacity(application, identifier)? {
            break;
        }
        match item {
            DispatchWork::Task(task) => start_task(application, identifier, task.id)?,
            DispatchWork::Issue(issue) => {
                let client = client.ok_or("The dispatch issue source was removed")?;
                // Re-read before claiming: a label, decision, ownership or close may have changed.
                let fresh = client
                    .get(issue.number)
                    .map_err(|error| error.to_string())?;
                start_issue(application, identifier, client.repository(), &fresh)?;
            }
        }
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
        match read_issues(reader) {
            Ok(backlog) => reader.backlog = backlog,
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

fn reader<'a>(project: &ProjectItem, readers: &'a mut BTreeMap<u32, Reader>) -> &'a mut Reader {
    let setup = || Reader {
        directory: project.repository_path.clone(),
        remote: project.github_remote.clone(),
        client: None,
        refreshed: None,
        report_retry: BTreeMap::new(),
        backlog: Backlog::default(),
    };
    let reader = readers.entry(project.id).or_insert_with(setup);
    if reader.directory != project.repository_path || reader.remote != project.github_remote {
        *reader = setup();
    }
    reader
}

fn read_issues(reader: &mut Reader) -> Result<Backlog, velocity_github::PullRequestError> {
    let directory = Path::new(&reader.directory);
    if reader.remote.is_none()
        && reader
            .client
            .as_ref()
            .is_some_and(|client| !client.is_current(directory))
    {
        reader.client = None;
    }
    if reader.client.is_none() {
        reader.client = match reader.remote.as_deref() {
            Some(remote) => Some(IssueClient::for_repository(directory, remote)?),
            None if directory.join(".git").exists() && IssueClient::has_origin(directory)? => {
                Some(IssueClient::discover(directory)?)
            }
            None => None,
        };
    }
    let Some(client) = &reader.client else {
        return Ok(Backlog::default());
    };
    Ok(Backlog {
        repository: client.repository().into(),
        issues: client.open_issues()?,
        error: None,
    })
}

fn has_capacity(application: &AppHandle, identifier: u32) -> Result<bool, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    Ok(store.project(identifier).is_some_and(project_has_capacity))
}

fn start_issue(
    application: &AppHandle,
    identifier: u32,
    repository: &str,
    issue: &GitHubIssue,
) -> Result<(), String> {
    let claimed = claimed_pieces(application, identifier)?;
    let (plan, thread) = {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let project = store.project(identifier).ok_or("The project was removed")?;
        if !project_has_capacity(project) || has_user_task(project, &claimed) {
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

fn start_task(application: &AppHandle, identifier: u32, task: u32) -> Result<(), String> {
    let claimed = claimed_pieces(application, identifier)?;
    let (plan, thread) = {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let project = store.project(identifier).ok_or("The project was removed")?;
        let Some(task) = project
            .tasks
            .iter()
            .find(|entry| entry.id == task && dispatchable_task(project, entry, &claimed))
            .filter(|_| project_has_capacity(project))
        else {
            return Ok(());
        };
        if task.added_by != TaskAttribution::User && has_user_task(project, &claimed) {
            return Ok(());
        }
        let task_identifier = task.id;
        let plan = plan_task(&store, project, task).map_err(|error| error.to_string())?;
        let thread = store
            .record_continuous_task(identifier, plan.thread.clone(), task_identifier)
            .map_err(|error| error.to_string())?;
        (plan, thread)
    };
    crate::threads::start_planned_with_selection(application, plan, thread, false)?;
    Ok(())
}

fn has_user_task(project: &ProjectItem, claimed: &HashSet<String>) -> bool {
    project.tasks.iter().any(|task| {
        task.added_by == TaskAttribution::User && dispatchable_task(project, task, claimed)
    })
}

fn claimed_pieces(application: &AppHandle, identifier: u32) -> Result<HashSet<String>, String> {
    Ok(dispatch_claims(application, identifier)?
        .iter()
        .map(|claim| claim.key().to_owned())
        .collect())
}

fn dispatch_claims(
    application: &AppHandle,
    identifier: u32,
) -> Result<Vec<velocity_airc::Claim>, String> {
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    let channel = velocity_airc::Channel::open(&data, &identifier.to_string())
        .map_err(|error| error.to_string())?;
    channel.claims().map_err(|error| error.to_string())
}

fn save_error(application: &AppHandle, identifier: u32, error: String) -> Result<(), String> {
    let mut backlogs = lock(application.state::<Backlogs>().inner(), "dispatch backlog")?;
    backlogs.entry(identifier).or_default().error = Some(error);
    Ok(())
}

fn finish_empty_threads(application: &AppHandle, project: &ProjectItem) -> Result<(), String> {
    for thread in project.threads.iter().filter(|thread| {
        can_finish_empty_dispatch(thread)
            && (project.continuous_dispatch.slots > 0 || thread.started_by_dispatch)
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
        let nick = if thread.started_by_dispatch && thread.issue.is_some() {
            let member = format!("thread-{}", thread.id);
            dispatch_claims(application, project)?
                .into_iter()
                .find(|claim| claim.member == member)
                .map(|claim| claim.nick)
        } else {
            None
        };
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .finish_empty_dispatch_with_nick(project, thread.id, nick)
        .map_err(|error| error.to_string())?;
        crate::threads::refresh(application, key, false)?;
    }
    Ok(())
}

fn report_no_change_issues(
    application: &AppHandle,
    project: &ProjectItem,
    reader: &mut Reader,
) -> Result<(), String> {
    for (&thread, pending) in &project.continuous_dispatch.no_change_reports {
        if reader
            .report_retry
            .get(&thread)
            .is_some_and(|time| time.elapsed() < Duration::from_secs(30))
        {
            continue;
        }
        let byline = velocity_threads::issue_activity::issue_byline(
            project.id,
            Some(thread),
            pending.nick.as_deref(),
        );
        let result =
            IssueClient::for_repository(Path::new(&reader.directory), &pending.issue.repository)
                .and_then(|client| {
                    client.post_no_change(
                        pending.issue.number,
                        &pending.change,
                        &byline,
                        &pending.reason,
                    )
                });
        if let Err(error) = result {
            reader.report_retry.insert(thread, Instant::now());
            report::<()>(Err(format!(
                "Could not report no change on {}#{}: {error}",
                pending.issue.repository, pending.issue.number
            )));
            continue;
        }
        if let Some(sweeps) = application.try_state::<velocity_threads::ScoutSweeps>() {
            sweeps.request_no_change_review(project.id);
        }
        lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?
        .finish_no_change_report(project.id, thread)
        .map_err(|error| error.to_string())?;
        reader.report_retry.remove(&thread);
        reader.refreshed = None;
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
