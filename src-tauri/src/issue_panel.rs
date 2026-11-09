//! Native scheduling and IPC for the F9 issue projection and dispatch service.
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Manager, State};
use tauri_specta::Event;
use velocity_core::layout_view::{LayoutAction, LayoutIntent, LayoutSnapshot};
use velocity_github::{IssueClient, IssuePanelSnapshot};
use velocity_store::issues::{ThreadIssue, issue_thread};

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
}

fn selected_project(application: &AppHandle) -> Result<Option<ProjectContext>, String> {
    let store = application.state::<ManagedStateStore>();
    let store = lock(store.inner(), "persisted state")?;
    Ok(store.active_project().map(|project| ProjectContext {
        identifier: project.id,
        directory: project.repository_path.clone().into(),
    }))
}

#[tauri::command]
#[specta::specta]
pub fn get_issue_panel(
    application: AppHandle,
    panel: State<'_, IssuePanel>,
) -> Result<IssuePanelSnapshot, String> {
    let project = selected_project(&application)?;
    let mut panel = lock(panel.inner(), "issues")?;
    panel.select(project.map(|project| project.identifier));
    Ok(panel.clone())
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
    let (plan, thread) = {
        let store = application.state::<ManagedStateStore>();
        let mut store = lock(store.inner(), "persisted state")?;
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
    let snapshot = crate::threads::start_planned(&application, plan, thread)?;
    report(update_links(&application));
    Ok(snapshot)
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
    tauri::async_runtime::spawn_blocking(move || {
        IssueClient::discover(&project.directory)?.open_in_browser(number)
    })
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
) -> Result<(String, velocity_github::GitHubIssue), velocity_github::PullRequestError> {
    let client = IssueClient::discover(&project.directory)?;
    if !client.repository().eq_ignore_ascii_case(repository) {
        return Err(velocity_github::PullRequestError::Invalid(
            "The GitHub remote changed; refresh issues".into(),
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
    let store = application.state::<ManagedStateStore>();
    let mut store = lock(store.inner(), "persisted state")?;
    let project = store
        .active_project()
        .filter(|item| item.id == project)
        .ok_or("The selected project changed")?;
    let Some(identifier) =
        issue_thread(project, &ThreadIssue::new(repository.into(), number)).map(|thread| thread.id)
    else {
        return Ok(None);
    };
    let layout = application.state::<MainLayout>();
    let mut layout = lock(layout.inner(), "main-window state")?;
    let outcome = store
        .dispatch_project_layout(&mut layout, LayoutAction::OpenThread(identifier as usize))
        .map_err(|error| error.to_string())?;
    crate::layout::publish(application, &outcome.snapshot, None)?;
    Ok(Some(outcome.snapshot))
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
    let layout = application.state::<MainLayout>();
    if let Ok(mut layout) = lock(layout.inner(), "main-window state") {
        layout.show_toast(error, None);
        report(crate::layout::publish(
            application,
            &layout.snapshot(),
            None,
        ));
    }
}

pub fn initialize(application: &AppHandle) {
    application.manage(IssuePanel::default());
    let application = application.clone();
    std::thread::spawn(move || monitor(application));
}

fn monitor(application: AppHandle) {
    let mut previous = None;
    let mut refreshed = None::<Instant>;
    let mut revision = None;
    loop {
        if let Ok(project) = selected_project(&application) {
            if previous != project
                || refreshed.is_none_or(|time| time.elapsed() >= Duration::from_secs(30))
            {
                previous = project.clone();
                report(refresh(&application, project));
                refreshed = Some(Instant::now());
            }
            let next = application
                .state::<ManagedStateStore>()
                .lock()
                .ok()
                .map(|store| store.state().revision);
            if revision != next {
                report(update_links(&application));
                revision = next;
            }
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn refresh(application: &AppHandle, project: Option<ProjectContext>) -> Result<(), String> {
    {
        let panel = application.state::<IssuePanel>();
        let mut panel = lock(panel.inner(), "issues")?;
        panel.select(project.as_ref().map(|project| project.identifier));
        IssuePanelChanged {
            panel: panel.clone(),
        }
        .emit(application)
        .map_err(|error| error.to_string())?;
    }
    let Some(project) = project else {
        return Ok(());
    };
    let result = fetch_issues(&project.directory);
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
    update_links(application)
}

fn fetch_issues(
    directory: &std::path::Path,
) -> Result<(String, Vec<velocity_github::IssuePanelRow>), String> {
    if !IssueClient::has_origin(directory).map_err(|error| error.to_string())? {
        return Ok((String::new(), Vec::new()));
    }
    IssueClient::discover(directory)
        .and_then(|client| client.panel())
        .map_err(|error| error.to_string())
}

fn update_links(application: &AppHandle) -> Result<(), String> {
    let store = application.state::<ManagedStateStore>();
    let store = lock(store.inner(), "persisted state")?;
    let panel = application.state::<IssuePanel>();
    let mut panel = lock(panel.inner(), "issues")?;
    let Some(project) = store
        .active_project()
        .filter(|project| Some(project.id) == panel.project_identifier)
    else {
        return Ok(());
    };
    if let Some(repository) = panel.repository.clone() {
        velocity_threads::dispatch::associate_issue_threads(project, &repository, &mut panel.rows);
    }
    panel.revision = panel.revision.wrapping_add(1);
    let layout = application.state::<MainLayout>();
    let mut layout = lock(layout.inner(), "main-window state")?;
    layout.set_project_issues(
        project.id,
        velocity_threads::dispatch::issue_palette_rows(&panel.rows),
    );
    crate::layout::publish(application, &layout.snapshot(), None)?;
    IssuePanelChanged {
        panel: panel.clone(),
    }
    .emit(application)
    .map_err(|error| error.to_string())
}
