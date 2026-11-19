//! Tauri commands and events for swarms (SPEC.md §3.1, F10). Planning, records and read
//! models live in the crates; this file wires them to the window and the thread runtime.

use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use velocity_core::layout_view::LayoutSnapshot;
use velocity_core::swarm::{self, SwarmEventKind, SwarmFeedEvent};
use velocity_store::ThreadState;
use velocity_store::duty::clock_timestamp;
use velocity_store::swarm_dialog::{
    OpenIssues, SwarmDialog, SwarmDialogQuery, provider_model_name,
};
use velocity_store::swarms::{LaunchSwarmRequest, SwarmItem, live_members};
use velocity_threads::swarm::{OpenIssue, plan_swarm_member, require_swarm_launch};

use crate::issue_panel::IssuePanel;
use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::{lock, report};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, specta::Type, Event)]
#[tauri_specta(event_name = "swarm://event")]
pub struct SwarmEventPublished {
    pub project_identifier: u32,
    pub swarm_identifier: u32,
    pub event: SwarmFeedEvent,
}

/// The Swarm dialog for the user's current choices; asked again whenever one changes.
#[tauri::command]
#[specta::specta]
pub async fn get_swarm_dialog(
    application: AppHandle,
    query: SwarmDialogQuery,
) -> Result<SwarmDialog, String> {
    crate::layout::off_ui_thread(move || get_swarm_dialog_now(application, query)).await
}

/// [`get_swarm_dialog`] on the calling thread, for callers already off the UI thread.
pub(crate) fn get_swarm_dialog_now(
    application: AppHandle,
    query: SwarmDialogQuery,
) -> Result<SwarmDialog, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let panel = lock(application.state::<IssuePanel>().inner(), "issues")?;
    let active = store.active_project().map(|project| project.id);
    let current = panel.project_identifier == active && panel.repository.is_some();
    let rows = if current { &panel.rows[..] } else { &[] };
    let issues = OpenIssues {
        rows,
        loaded: current && !panel.loading,
    };
    store
        .swarm_dialog(&query, issues)
        .ok_or_else(|| "Select or create a project first".into())
}

/// Starts one agent per selected item, all at once, each in its own worktree (F10).
#[tauri::command]
#[specta::specta]
pub async fn launch_swarm(
    application: AppHandle,
    request: LaunchSwarmRequest,
) -> Result<LayoutSnapshot, String> {
    crate::layout::off_ui_thread(move || launch(&application, request)).await
}

/// + Items: the same launch, adding members to an existing swarm.
#[tauri::command]
#[specta::specta]
pub async fn add_to_swarm(
    application: AppHandle,
    request: LaunchSwarmRequest,
) -> Result<LayoutSnapshot, String> {
    crate::layout::off_ui_thread(move || {
        if request.into.is_none() {
            return Err("Choose the swarm to add items to".into());
        }
        launch(&application, request)
    })
    .await
}

fn launch(application: &AppHandle, request: LaunchSwarmRequest) -> Result<LayoutSnapshot, String> {
    velocity_rosen::memory::require_headroom()?;
    let (repository, issues) = open_issues(application, &request)?;
    {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        require_swarm_launch(&store, &request).map_err(|error| error.to_string())?;
    }
    let swarm = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .open_swarm(&request, clock_timestamp())
    .map_err(|error| error.to_string())?;
    let assignments = swarm_assignments(application, &request, &issues)?;
    // Members take ready worktrees one by one instead of all checking out at once (F5).
    crate::ready_pool::reserve(application, request.project_identifier, request.items.len());
    let mut started = 0;
    let mut refused = Vec::new();
    for assignment in assignments {
        if let Err(error) = velocity_rosen::memory::require_headroom() {
            refused.push(error);
            break;
        }
        match start_member(
            application,
            &request,
            swarm,
            &repository,
            &issues,
            assignment,
        ) {
            Ok(()) => started += 1,
            Err(error) => refused.push(error),
        }
    }
    record_launch(application, &request, swarm, started);
    finish_launch(application, &request, swarm, started, &refused)
}

type SwarmAssignment = (Option<String>, (SwarmItem, usize));

fn swarm_assignments(
    application: &AppHandle,
    request: &LaunchSwarmRequest,
    issues: &[OpenIssue],
) -> Result<Vec<SwarmAssignment>, String> {
    let project = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .project(request.project_identifier)
    .cloned()
    .ok_or("The selected project no longer exists")?;
    let graph = (request.items.len() > 1)
        .then(|| crate::code_graph::subsystem_map(application, &project))
        .flatten();
    let providers = swarm::round_robin(request.items.len(), request.providers.len());
    let tagged = request
        .items
        .iter()
        .copied()
        .zip(providers)
        .map(|(item, provider)| {
            let title = match item {
                SwarmItem::Task(id) => project
                    .tasks
                    .iter()
                    .find(|task| task.id == id)
                    .map(|task| task.text.as_str()),
                SwarmItem::Issue(number) => issues
                    .iter()
                    .find(|issue| issue.number == number)
                    .map(|issue| issue.title.as_str()),
            };
            let subsystem = title.and_then(|title| graph.as_ref()?.classify(title));
            (subsystem, (item, provider))
        })
        .collect();
    Ok(velocity_graph::coordination::spread(tagged))
}

/// The Issues panel's last read of open issues, which the dialog was drawn from (F9).
fn open_issues(
    application: &AppHandle,
    request: &LaunchSwarmRequest,
) -> Result<(String, Vec<OpenIssue>), String> {
    let panel = lock(application.state::<IssuePanel>().inner(), "issues")?;
    let wants_issues = request
        .items
        .iter()
        .any(|item| matches!(item, SwarmItem::Issue(_)));
    if !wants_issues {
        return Ok((panel.repository.clone().unwrap_or_default(), Vec::new()));
    }
    if panel.project_identifier != Some(request.project_identifier) {
        return Err("The selected project changed; refresh issues".into());
    }
    let repository = panel
        .repository
        .clone()
        .filter(|repository| !repository.is_empty())
        .ok_or("Issues have not loaded yet")?;
    let issues = panel
        .rows
        .iter()
        .filter(|row| row.question.is_none())
        .map(|row| OpenIssue {
            number: row.number,
            title: row.title.clone(),
            assigned: row.thread_identifier.is_some(),
        })
        .collect();
    Ok((repository, issues))
}

fn start_member(
    application: &AppHandle,
    request: &LaunchSwarmRequest,
    swarm: u32,
    repository: &str,
    issues: &[OpenIssue],
    assignment: SwarmAssignment,
) -> Result<(), String> {
    let (subsystem, (item, provider_index)) = assignment;
    let provider = request.providers[provider_index];
    let (mut plan, thread) = {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let plan = plan_swarm_member(&store, request, repository, issues, item, provider)
            .map_err(|error| error.to_string())?;
        let thread = store
            .add_swarm_member(
                request.project_identifier,
                swarm,
                plan.launch.thread.clone(),
                plan.item,
                plan.issue.clone(),
            )
            .map_err(|error| error.to_string())?;
        (plan.launch, thread)
    };
    if let Some(subsystem) = subsystem {
        plan.prompt.push_str(&format!("\n\nThe code graph suggests `{subsystem}` as a starting point for this item. Verify against the issue or task and source."));
    }
    crate::threads::start_planned(application, plan, thread).map(|_| ())
}

fn record_launch(
    application: &AppHandle,
    request: &LaunchSwarmRequest,
    swarm: u32,
    started: usize,
) {
    let models = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .map(|store| {
        let settings = store.settings();
        request
            .providers
            .iter()
            .map(|provider| provider_model_name(&settings, *provider))
            .collect::<Vec<_>>()
            .join(", ")
    })
    .unwrap_or_default();
    let plural = if started == 1 { "" } else { "s" };
    let message = match request.into {
        Some(_) => format!("Added {started} agent{plural} · {models}"),
        None => format!(
            "Launched {started} agent{plural} on {} · {models}",
            request.source.label()
        ),
    };
    let event = SwarmFeedEvent::new(&clock_timestamp(), "you", SwarmEventKind::You, message);
    report(post_event(
        application,
        request.project_identifier,
        swarm,
        event,
    ));
}

fn is_active(store: &velocity_store::StateStore, project: u32) -> bool {
    store.active_project().map(|active| active.id) == Some(project)
}

fn finish_launch(
    application: &AppHandle,
    request: &LaunchSwarmRequest,
    swarm: u32,
    started: usize,
    refused: &[String],
) -> Result<LayoutSnapshot, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let mut layout = lock(
        application.state::<MainLayout>().inner(),
        "main-window state",
    )?;
    store.refresh_follow_through_layout(&mut layout);
    // After a project switch the window shows another project, where this swarm's number
    // means nothing; the launch still reports in the toast.
    if is_active(&store, request.project_identifier) {
        layout.close_swarm_dialog_and_select(swarm as usize);
    }
    let mut message = format!(
        "{started} agent{} launched · {started} workspaces · all running",
        if started == 1 { "" } else { "s" }
    );
    if let Some(first) = refused.first() {
        message = format!("{message} · {} not started: {first}", refused.len());
    }
    layout.show_toast(message, None);
    let snapshot = layout.snapshot();
    crate::layout::publish(application, &snapshot, None)?;
    Ok(snapshot)
}

/// Saves a feed line and publishes it on `swarm://event` and `layout://state`.
pub(crate) fn post_event(
    application: &AppHandle,
    project: u32,
    swarm: u32,
    event: SwarmFeedEvent,
) -> Result<(), String> {
    let mut store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    if !store
        .record_swarm_event(project, swarm, event.clone())
        .map_err(|error| error.to_string())?
    {
        return Ok(());
    }
    let mut layout = lock(
        application.state::<MainLayout>().inner(),
        "main-window state",
    )?;
    store.refresh_follow_through_layout(&mut layout);
    crate::layout::publish(application, &layout.snapshot(), None)?;
    SwarmEventPublished {
        project_identifier: project,
        swarm_identifier: swarm,
        event,
    }
    .emit(application)
    .map_err(|error| error.to_string())
}

/// The swarm composer: a message goes to every agent still working, each picking it up at
/// its next step (Rule 1). `None` when `identifier` is not a swarm.
pub fn broadcast(
    application: &AppHandle,
    project: u32,
    identifier: usize,
    prompt: &str,
) -> Option<Result<LayoutSnapshot, String>> {
    let (project, swarm, members) = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )
        .ok()?;
        let project = store.project(project)?;
        let swarm = project
            .swarms
            .iter()
            .find(|swarm| swarm.id as usize == identifier)?;
        let members: Vec<u32> = live_members(project, swarm)
            .filter(|(_, thread)| {
                matches!(thread.state, ThreadState::Working | ThreadState::Review)
            })
            .map(|(_, thread)| thread.id)
            .collect();
        (project.id, swarm.id, members)
    };
    let delivered = members
        .iter()
        .filter(|thread| crate::threads::steer(application, (project, **thread), prompt).is_ok())
        .count();
    let message = format!(
        "to the whole swarm · {delivered} of {} agents: {prompt}",
        members.len()
    );
    let event = SwarmFeedEvent::new(&clock_timestamp(), "you", SwarmEventKind::You, message);
    report(post_event(application, project, swarm, event));
    Some(snapshot(application))
}

fn snapshot(application: &AppHandle) -> Result<LayoutSnapshot, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let mut layout = lock(
        application.state::<MainLayout>().inner(),
        "main-window state",
    )?;
    store.refresh_follow_through_layout(&mut layout);
    Ok(layout.snapshot())
}

/// A member stopped on an error; its worktree is kept (§2.1, F5).
pub(crate) fn record_member_failed(application: &AppHandle, key: (u32, u32), headline: &str) {
    let recorded = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .and_then(|mut store| {
        store
            .record_member_event(
                key.0,
                key.1,
                SwarmEventKind::Failed,
                format!("{headline} · workspace kept"),
                &clock_timestamp(),
            )
            .map_err(|error| error.to_string())
    });
    if let Ok(Some(_)) = recorded {
        report(
            snapshot(application)
                .and_then(|state| crate::layout::publish(application, &state, None)),
        );
    }
}
