//! Native scheduling and IPC for the F9 issue projection and dispatch service.
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager};
use tauri_plugin_opener::OpenerExt;
use tauri_specta::Event;
use velocity_core::layout_view::{LayoutAction, LayoutIntent, LayoutSnapshot};
use velocity_core::work_pool::WorkKey;
use velocity_github::{
    GitHubCommandError, IssueClient, IssueDiscussion, IssueHost, IssuePanelCache,
    IssuePanelSnapshot, OPEN_ISSUES_REFRESH,
};
use velocity_store::issues::{ThreadIssue, issue_thread};
use velocity_store::rosen_work::IssueVersion;
use velocity_store::{ThreadState, VersionControl};

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::{lock, report};

pub type IssuePanel = Mutex<IssuePanelSnapshot>;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type, Event)]
#[tauri_specta(event_name = "issue://panel")]
pub struct IssuePanelChanged {
    pub panel: IssuePanelSnapshot,
}

#[derive(Clone, PartialEq, Eq)]
struct ProjectContext {
    identifier: u32,
    directory: PathBuf,
    host: IssueHost,
    remote: Option<String>,
}

impl ProjectContext {
    fn from_project(project: &velocity_store::ProjectItem) -> Self {
        let host = project.effective_issue_host();
        let remote = match host {
            IssueHost::Collapse => project.collapse_repository.clone(),
            IssueHost::GitHub if project.version_control == VersionControl::Rosen => {
                project.github_remote.clone()
            }
            IssueHost::GitHub => None,
        };
        Self {
            identifier: project.id,
            directory: project.repository_path.clone().into(),
            host,
            remote,
        }
    }

    fn issue_client(&self) -> Result<IssueClient, GitHubCommandError> {
        match &self.remote {
            Some(remote) => IssueClient::for_host(&self.directory, self.host, remote),
            None => IssueClient::discover(&self.directory),
        }
    }
}

fn selected_project(application: &AppHandle) -> Result<Option<ProjectContext>, String> {
    let store = application.state::<ManagedStateStore>();
    let store = lock(store.inner(), "persisted state")?;
    Ok(store.active_project().map(ProjectContext::from_project))
}

#[tauri::command]
#[specta::specta]
pub async fn get_issue_panel(application: AppHandle) -> Result<IssuePanelSnapshot, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let project = selected_project(&application)?;
        let panel = application.state::<IssuePanel>();
        {
            let snapshot = lock(panel.inner(), "issues")?;
            let matches_project =
                snapshot.project_identifier == project.as_ref().map(|project| project.identifier);
            if matches_project && !snapshot.loading && snapshot.error.is_none() {
                return Ok(snapshot.clone());
            }
        }
        refresh(&application, project, &mut IssueReader::default())?;
        Ok(lock(panel.inner(), "issues")?.clone())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
#[specta::specta]
pub async fn get_issue_discussion(
    application: AppHandle,
    project_identifier: u32,
    repository: String,
    number: u32,
) -> Result<IssueDiscussion, String> {
    let project = require_project(&application, project_identifier)?;
    tauri::async_runtime::spawn_blocking(move || {
        IssueClient::for_host(&project.directory, project.host, &repository)?.discussion(number)
    })
    .await
    .map_err(|error| error.to_string())?
    .map_err(|error| error.to_string())
}

#[tauri::command]
#[specta::specta]
pub fn open_issue_discussion(
    application: AppHandle,
    repository: String,
    number: u32,
) -> Result<(), String> {
    let parsed =
        velocity_github::RepositoryName::parse(&repository).map_err(|error| error.to_string())?;
    let full_name = parsed.full_name();
    let host = resolve_repository_issue_host(&application, &full_name)?;
    let url = match host {
        IssueHost::Collapse => velocity_core::collapse::issue_page(&full_name, number)
            .ok_or("Invalid Collapse issue URL")?,
        IssueHost::GitHub => format!("https://github.com/{full_name}/issues/{number}"),
    };
    application
        .opener()
        .open_url(url, None::<&str>)
        .map_err(|error| error.to_string())
}

fn resolve_repository_issue_host(
    application: &AppHandle,
    repository: &str,
) -> Result<IssueHost, String> {
    let store = application.state::<ManagedStateStore>();
    let store = lock(store.inner(), "persisted state")?;
    if let Some(active) = store.active_project()
        && let Some((host, remote)) = active.issue_host_repository()
        && remote.eq_ignore_ascii_case(repository)
    {
        return Ok(host);
    }
    if let Some(host) = store.state().projects.iter().find_map(|project| {
        let (host, remote) = project.issue_host_repository()?;
        remote.eq_ignore_ascii_case(repository).then_some(host)
    }) {
        return Ok(host);
    }
    Ok(IssueHost::GitHub)
}

#[tauri::command]
#[specta::specta]
pub async fn set_work_issue_expanded(
    application: AppHandle,
    project_identifier: u32,
    repository: String,
    number: u32,
    expanded: bool,
) -> Result<(), String> {
    crate::layout::run_concurrently("set_work_issue_expanded", move || {
        let key = WorkKey::issue(&repository, number);
        let refresh = {
            let store = application.state::<ManagedStateStore>();
            let mut store = lock(store.inner(), "persisted state")?;
            let project = store
                .active_project()
                .filter(|project| project.id == project_identifier)
                .ok_or("The selected project changed; refresh Work")?;
            if project.expanded_work_issues.contains(&key) == expanded {
                return Ok(());
            }
            store
                .update_project::<_, velocity_store::projects::ProjectError>(
                    project_identifier,
                    |project| {
                        if expanded {
                            project.expanded_work_issues.push(key);
                        } else {
                            project.expanded_work_issues.retain(|issue| issue != &key);
                        }
                        Ok(())
                    },
                )
                .map_err(|error| error.to_string())?;
            store.layout_refresh()
        };
        crate::rosen_events::publish_progress(&application, refresh)
    })
    .await
}

/// Expands or collapses one issue row in the rail (#209) and broadcasts the new snapshot.
#[tauri::command]
#[specta::specta]
pub async fn toggle_issue_expanded(
    application: AppHandle,
    number: u32,
) -> Result<IssuePanelSnapshot, String> {
    crate::layout::run_concurrently("toggle_issue_expanded", move || {
        let (changed, snapshot) = {
            let panel = application.state::<IssuePanel>();
            let mut panel = lock(panel.inner(), "issues")?;
            let changed = panel.toggle_expanded(number);
            (changed, panel.clone())
        };
        if changed {
            IssuePanelChanged {
                panel: snapshot.clone(),
            }
            .emit(&application)
            .map_err(|error| error.to_string())?;
        }
        Ok(snapshot)
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn dispatch_issue(
    application: AppHandle,
    project_identifier: u32,
    number: u32,
    accept_recommendation: bool,
) -> Result<LayoutSnapshot, String> {
    let project = require_project(&application, project_identifier)?;
    let repository = panel_repository(&application, project_identifier)?;
    dispatch_selected_issue(
        application,
        project,
        repository,
        number,
        accept_recommendation,
    )
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn dispatch_work_issue(
    application: AppHandle,
    project_identifier: u32,
    repository: String,
    number: u32,
    accept_recommendation: bool,
) -> Result<LayoutSnapshot, String> {
    let project = require_project(&application, project_identifier)?;
    dispatch_selected_issue(
        application,
        project,
        repository,
        number,
        accept_recommendation,
    )
    .await
}

async fn dispatch_selected_issue(
    application: AppHandle,
    project: ProjectContext,
    repository: String,
    number: u32,
    accept_recommendation: bool,
) -> Result<LayoutSnapshot, String> {
    let project_identifier = project.identifier;
    if let Some(snapshot) =
        open_assigned_thread(&application, project_identifier, &repository, number)?
    {
        return Ok(snapshot);
    }
    let (repository, issue) =
        tauri::async_runtime::spawn_blocking(move || read_issue(&project, &repository, number))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|error| error.to_string())?;
    velocity_rosen::memory::require_headroom()?;
    let rosen_project = {
        let store = application.state::<ManagedStateStore>();
        let store = lock(store.inner(), "persisted state")?;
        store
            .project(project_identifier)
            .filter(|project| project.version_control == VersionControl::Rosen)
            .cloned()
    };
    if let Some(project) = rosen_project {
        let handle = application.clone();
        let texts = vec![issue.title.clone(), issue.body.clone()];
        let _ = tauri::async_runtime::spawn_blocking(move || {
            crate::graph_access::warn_dispatch_signatures(&handle, &project, number, &texts);
        })
        .await;
    }
    let (plan, thread) = {
        let store = application.state::<ManagedStateStore>();
        let mut store = lock(store.inner(), "persisted state")?;
        let identity = ThreadIssue::new(repository.clone(), number);
        let version = IssueVersion::from_github(&issue);
        if store.project(project_identifier).is_some_and(|project| {
            project.version_control == VersionControl::Rosen
                && project.rosen_issue_completed_for_version(&identity, version)
        }) {
            return Err(format!("Issue #{number} has a landed change"));
        }
        let plan = velocity_threads::plan_issue_dispatch(
            &store,
            project_identifier,
            &repository,
            &issue,
            accept_recommendation,
        )
        .map_err(|error| error.to_string())?;
        let thread = plan.record(&mut store).map_err(|error| error.to_string())?;
        (plan.launch, thread)
    };
    let provider_name = plan.thread.provider.key();
    let model_name = plan.thread.model_name.clone();
    crate::fleet_ledger::record_dispatch(
        &application,
        project_identifier,
        &repository,
        &issue,
        thread,
        &provider_name,
        &model_name,
    );
    let snapshot = crate::threads::start_planned(&application, plan, thread)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    crate::project_health::notify_dispatched(&application, project_identifier, now);
    report(update_links(&application, false));
    Ok(snapshot)
}

#[tauri::command]
#[specta::specta]
pub async fn release_issue_hold(
    application: AppHandle,
    project_identifier: u32,
    repository: String,
    number: u32,
) -> Result<IssuePanelSnapshot, String> {
    let project = require_project(&application, project_identifier)?;
    tauri::async_runtime::spawn_blocking(move || {
        IssueClient::for_host(&project.directory, project.host, &repository)
            .and_then(|client| client.release_person_hold(number))
            .map_err(|error| error.to_string())?;
        refresh(&application, Some(project), &mut IssueReader::default())?;
        let panel = application.state::<IssuePanel>();
        Ok(lock(panel.inner(), "issues")?.clone())
    })
    .await
    .map_err(|error| error.to_string())?
}

#[tauri::command]
#[specta::specta]
pub async fn open_project_issue(
    application: AppHandle,
    project_identifier: u32,
    number: u32,
) -> Result<(), String> {
    let project = require_project(&application, project_identifier)?;
    let repository = panel_repository(&application, project_identifier)?;
    if open_assigned_thread(&application, project_identifier, &repository, number)?.is_some() {
        return Ok(());
    }
    if project.host == IssueHost::Collapse {
        let url = velocity_core::collapse::issue_page(&repository, number)
            .ok_or("Invalid Collapse issue URL")?;
        return application
            .opener()
            .open_url(url, None::<&str>)
            .map_err(|error| error.to_string());
    }
    tauri::async_runtime::spawn_blocking(move || project.issue_client()?.open_in_browser(number))
        .await
        .map_err(|error| error.to_string())?
        .map_err(|error| error.to_string())
}

fn require_project(application: &AppHandle, identifier: u32) -> Result<ProjectContext, String> {
    selected_project(application)?
        .filter(|project| project.identifier == identifier)
        .ok_or_else(|| "The selected project changed; refresh issues".into())
}

fn panel_repository(application: &AppHandle, project: u32) -> Result<String, String> {
    let panel = application.state::<IssuePanel>();
    let panel = lock(panel.inner(), "issues")?;
    if panel.project_identifier != Some(project) {
        return Err("The selected project changed; refresh issues".into());
    }
    panel
        .repository
        .clone()
        .filter(|repository| !repository.is_empty())
        .ok_or_else(|| "Issues have not loaded yet".into())
}

fn read_issue(
    project: &ProjectContext,
    repository: &str,
    number: u32,
) -> Result<(String, velocity_github::GitHubIssue), velocity_github::GitHubCommandError> {
    let client = project.issue_client()?;
    if !client.repository().eq_ignore_ascii_case(repository) {
        return Err(velocity_github::GitHubCommandError::Invalid(
            "The issue repository changed; refresh issues".into(),
        ));
    }
    Ok((client.repository().to_owned(), client.get(number)?))
}

fn open_assigned_thread(
    application: &AppHandle,
    project: u32,
    repository: &str,
    number: u32,
) -> Result<Option<LayoutSnapshot>, String> {
    let (outcome, fast_snapshot) = {
        let store = application.state::<ManagedStateStore>();
        let mut store = lock(store.inner(), "persisted state")?;
        let project = store
            .active_project()
            .filter(|item| item.id == project)
            .ok_or("The selected project changed")?;
        let identity = ThreadIssue::new(repository.into(), number);
        let projected_owner = if project.version_control == VersionControl::Rosen {
            project
                .rosen_work_item(&WorkKey::issue(repository, number))
                .filter(|item| !item.status.completed())
                .and_then(|item| item.thread_identifier)
                .filter(|identifier| {
                    project.threads.iter().any(|thread| {
                        thread.id == *identifier
                            && !thread.supervision.released
                            && !matches!(thread.state, ThreadState::Merged | ThreadState::Done)
                    })
                })
        } else {
            None
        };
        let Some(identifier) =
            projected_owner.or_else(|| issue_thread(project, &identity).map(|thread| thread.id))
        else {
            return Ok(None);
        };
        let layout = application.state::<MainLayout>();
        let mut layout = lock(layout.inner(), "main-window state")?;
        let outcome = store
            .dispatch_project_layout(&mut layout, LayoutAction::OpenThread(identifier as usize))
            .map_err(|error| error.to_string())?;
        drop(store);
        let fast_snapshot = (outcome.project_switch.is_none()
            && outcome.pending_hydration.is_none())
        .then(|| layout.snapshot());
        (outcome, fast_snapshot)
    };
    let snapshot = match fast_snapshot {
        Some(snapshot) => snapshot,
        None => {
            crate::layout::finish_dispatch(
                application,
                outcome.project_switch,
                outcome.pending_hydration,
            )?
            .0
        }
    };
    crate::layout::publish(application, &snapshot, None)?;
    Ok(Some(snapshot))
}

pub fn handle_intent(application: &AppHandle, intent: &LayoutIntent) -> bool {
    let (number, dispatch) = match intent {
        LayoutIntent::DispatchIssue(number) => (*number, true),
        LayoutIntent::OpenIssue(number) => (*number, false),
        _ => return false,
    };
    let Some(project) = selected_project(application).ok().flatten() else {
        return true;
    };
    let Ok(number) = u32::try_from(number) else {
        return true;
    };
    let application = application.clone();
    tauri::async_runtime::spawn(async move {
        let result = if dispatch {
            dispatch_issue(application.clone(), project.identifier, number, false)
                .await
                .map(|_| ())
        } else {
            open_project_issue(application.clone(), project.identifier, number).await
        };
        if let Err(error) = result {
            show_error(&application, error);
        }
    });
    true
}

fn show_error(application: &AppHandle, error: String) {
    let snapshot = {
        let layout = application.state::<MainLayout>();
        lock(layout.inner(), "main-window state")
            .ok()
            .map(|mut layout| {
                layout.show_toast(error, None);
                layout.snapshot()
            })
    };
    if let Some(snapshot) = snapshot {
        report(crate::layout::publish(application, &snapshot, None));
    }
}

pub fn initialize(application: &AppHandle) {
    application.manage(IssuePanel::default());
    let application = application.clone();
    std::thread::spawn(move || monitor(application));
}

/// The selected project's GitHub client and its last full read. Kept between refreshes:
/// reading the repository's identity is a GitHub request (#228).
#[derive(Default)]
struct IssueReader {
    client: Option<IssueClient>,
    cache: IssuePanelCache,
}

fn monitor(application: AppHandle) {
    // Wait briefly on startup for Collapse issue-host backfill so the issue panel does not
    // flash GitHub issues for an already-imported Collapse project (#613).
    crate::collapse_mirror::wait_for_issue_host_backfill(&application, Duration::from_secs(5));
    let mut previous = None;
    let mut refreshed = None::<Instant>;
    let mut cache_version = None::<u64>;
    let mut revision = None;
    let mut reader = IssueReader::default();
    loop {
        if let Ok(project) = selected_project(&application) {
            let shared_version = reader.client.as_ref().and_then(|client| {
                IssueClient::open_issues_version_for(client.host(), client.repository())
            });
            let cache_updated = shared_version.is_some() && shared_version != cache_version;
            if previous != project
                || cache_updated
                || refreshed.is_none_or(|time| time.elapsed() >= OPEN_ISSUES_REFRESH)
            {
                if previous != project {
                    reader = IssueReader::default();
                }
                previous = project.clone();
                report(refresh(&application, project, &mut reader));
                cache_version = reader.client.as_ref().and_then(|client| {
                    IssueClient::open_issues_version_for(client.host(), client.repository())
                });
                refreshed = Some(Instant::now());
            }
            let next = application
                .state::<ManagedStateStore>()
                .lock()
                .ok()
                .map(|store| store.state().revision);
            if revision != next {
                report(update_links(&application, false));
                revision = next;
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn refresh(
    application: &AppHandle,
    project: Option<ProjectContext>,
    reader: &mut IssueReader,
) -> Result<(), String> {
    if selected_project(application)? != project {
        return Ok(());
    }
    let selected_panel = {
        let panel = application.state::<IssuePanel>();
        let mut panel = lock(panel.inner(), "issues")?;
        panel.select(
            project.as_ref().map(|project| project.identifier),
            project
                .as_ref()
                .map(|project| project.host)
                .unwrap_or(IssueHost::GitHub),
        );
        panel.clone()
    };
    IssuePanelChanged {
        panel: selected_panel,
    }
    .emit(application)
    .map_err(|error| error.to_string())?;
    let Some(project) = project else {
        return Ok(());
    };
    let result = fetch_issues(&project, reader);
    if selected_project(application)?.as_ref() != Some(&project) {
        return Ok(());
    }
    {
        let panel = application.state::<IssuePanel>();
        let mut panel = lock(panel.inner(), "issues")?;
        if panel.project_identifier != Some(project.identifier) {
            return Ok(());
        }
        panel.update(result);
    }
    update_links(application, true)
}

fn fetch_issues(
    project: &ProjectContext,
    reader: &mut IssueReader,
) -> Result<(String, Vec<velocity_github::IssuePanelRow>), String> {
    if project.remote.is_none()
        && (project.host == IssueHost::Collapse
            || !project.directory.join(".git").exists()
            || !IssueClient::has_origin(&project.directory).map_err(|error| error.to_string())?)
    {
        *reader = IssueReader::default();
        return Ok((String::new(), Vec::new()));
    }
    let client = match reader.client.take() {
        Some(client) if project.remote.is_some() || client.is_current(&project.directory) => client,
        _ => project.issue_client().map_err(|error| error.to_string())?,
    };
    let panel = client
        .cached_panel(&mut reader.cache)
        .map_err(|error| error.to_string())?;
    // A failed read drops the client, so a changed remote or sign-in is discovered afresh.
    reader.client = Some(client);
    Ok(panel)
}

pub(crate) fn refresh_links(application: &AppHandle) {
    report(update_links(application, false));
}

fn update_links(application: &AppHandle, force_publish: bool) -> Result<(), String> {
    let (project_id, repository, numbers) = {
        let panel = application.state::<IssuePanel>();
        let panel = lock(panel.inner(), "issues")?;
        let Some(project_id) = panel.project_identifier else {
            return Ok(());
        };
        let numbers: Vec<u32> = panel.rows.iter().map(|row| row.number).collect();
        (project_id, panel.repository.clone(), numbers)
    };
    let (links, reopened_holds) = {
        let store = application.state::<ManagedStateStore>();
        let store = lock(store.inner(), "persisted state")?;
        let Some(project) = store
            .active_project()
            .filter(|project| project.id == project_id)
        else {
            return Ok(());
        };
        let links = repository.as_deref().map(|repository| {
            velocity_threads::dispatch::collect_issue_row_links(project, repository, &numbers)
        });
        let holds = repository
            .as_deref()
            .map(|repository| {
                project
                    .continuous_dispatch
                    .reopened_landed_holds
                    .iter()
                    .filter(|(_, hold)| hold.issue.repository.eq_ignore_ascii_case(repository))
                    .map(|(&number, hold)| (number, hold.clone()))
                    .collect::<std::collections::BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        (links, holds)
    };
    let unblocks = match repository.as_deref() {
        Some(repository) => crate::continuous_dispatch::unblock_counts(application, repository)?,
        None => std::collections::BTreeMap::new(),
    };
    let restart_waits =
        crate::continuous_dispatch::restart_wait::waits_for_project(application, project_id);
    let publish_bundle = {
        let panel = application.state::<IssuePanel>();
        let mut panel = lock(panel.inner(), "issues")?;
        if panel.project_identifier != Some(project_id) || panel.repository != repository {
            return Ok(());
        }
        let mut changed = false;
        if let Some(mut links) = links {
            for link in &mut links {
                if link.thread_identifier.is_none()
                    && let Some(hold) = reopened_holds.get(&link.number)
                {
                    link.thread_label = Some(
                        velocity_core::reopened_land::reopened_already_landed_reason(&hold.commit),
                    );
                    link.hold = Some(crate::continuous_dispatch::reopened_land::issue_hold(hold));
                } else if link.thread_identifier.is_none()
                    && let Some(wait) = restart_waits.get(&link.number)
                {
                    link.thread_label = Some(wait.label());
                    link.hold = Some(wait.hold());
                }
            }
            changed |= velocity_threads::dispatch::apply_issue_row_links(&mut panel.rows, &links);
            for row in &mut panel.rows {
                let next_unblocks = unblocks.get(&row.number).copied().unwrap_or(0);
                if row.unblocks != next_unblocks {
                    row.unblocks = next_unblocks;
                    changed = true;
                }
            }
        }
        if !changed && !force_publish {
            None
        } else {
            panel.revision = panel.revision.wrapping_add(1);
            let palette_rows = velocity_threads::dispatch::issue_palette_rows(&panel.rows);
            Some((panel.clone(), palette_rows))
        }
    };
    let Some((panel_snapshot, palette_rows)) = publish_bundle else {
        return Ok(());
    };
    let layout_snapshot = {
        let layout = application.state::<MainLayout>();
        let mut layout = lock(layout.inner(), "main-window state")?;
        layout
            .set_project_issues(project_id, palette_rows)
            .then(|| layout.snapshot())
    };
    if let Some(layout_snapshot) = layout_snapshot {
        crate::layout::publish(application, &layout_snapshot, None)?;
    }
    IssuePanelChanged {
        panel: panel_snapshot,
    }
    .emit(application)
    .map_err(|error| error.to_string())
}
