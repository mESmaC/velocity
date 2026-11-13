//! Tauri commands and events for swarms (SPEC.md §3.1, F10). Planning, records and read
//! models live in the crates; this file wires them to the window and the thread runtime.

use tauri::{AppHandle, Manager};
use tauri_specta::Event;
use velocity_core::layout_view::LayoutSnapshot;
use velocity_core::swarm::{self, PullRequestStep, SwarmEventKind, SwarmFeedEvent};
use velocity_github::follow_through::FollowThrough;
use velocity_store::duty::{StartDutyRequest, clock_timestamp};
use velocity_store::swarm_dialog::{
    OpenIssues, SwarmDialog, SwarmDialogQuery, provider_model_name,
};
use velocity_store::swarms::{
    LaunchSwarmRequest, SwarmItem, live_members, swarm_finished, thread_swarm,
};
use velocity_store::{ThreadItem, ThreadState};
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
    let (repository, issues) = open_issues(application, &request)?;
    {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        require_swarm_launch(&store, &request).map_err(|error| error.to_string())?;
    }
    if request.add_pr_duty {
        report(add_pr_duty(application, &request));
    }
    let swarm = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .open_swarm(&request, clock_timestamp())
    .map_err(|error| error.to_string())?;
    let assignments = swarm::round_robin(request.items.len(), request.providers.len());
    // Members take ready worktrees one by one instead of all checking out at once (F5).
    crate::ready_pool::reserve(application, request.project_identifier, request.items.len());
    let mut started = 0;
    let mut refused = Vec::new();
    for (item, provider) in request.items.iter().zip(assignments) {
        let provider = request.providers[provider];
        match start_member(
            application,
            &request,
            swarm,
            &repository,
            &issues,
            *item,
            provider,
        ) {
            Ok(()) => started += 1,
            Err(error) => refused.push(error),
        }
    }
    record_launch(application, &request, swarm, started);
    finish_launch(application, swarm, started, &refused)
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

/// Starts PR duty, or adds the agents it is short of, before the PRs arrive (F10).
fn add_pr_duty(application: &AppHandle, request: &LaunchSwarmRequest) -> Result<(), String> {
    let query = SwarmDialogQuery {
        source: request.source,
        items: Some(request.items.clone()),
        providers: request.providers.clone(),
        into: request.into,
    };
    let shortfall = get_swarm_dialog_now(application.clone(), query)?.pr_duty_shortfall;
    if shortfall == 0 {
        return Ok(());
    }
    let agents = u8::try_from(shortfall).unwrap_or(u8::MAX);
    let outcome = {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let mut layout = lock(
            application.state::<MainLayout>().inner(),
            "main-window state",
        )?;
        store
            .start_or_add_duty(
                &mut layout,
                StartDutyRequest::pr_duty(agents),
                clock_timestamp(),
            )
            .map_err(|error| error.to_string())?
    };
    crate::layout::publish(application, &outcome.snapshot, None)?;
    crate::duty::DutyEventPublished {
        role: outcome.role,
        event: outcome.event,
    }
    .emit(application)
    .map_err(|error| error.to_string())?;
    crate::reviews::wake(application);
    Ok(())
}

fn start_member(
    application: &AppHandle,
    request: &LaunchSwarmRequest,
    swarm: u32,
    repository: &str,
    issues: &[OpenIssue],
    item: SwarmItem,
    provider: velocity_store::ProviderKind,
) -> Result<(), String> {
    let (plan, thread) = {
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

fn finish_launch(
    application: &AppHandle,
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
    layout.close_swarm_dialog_and_select(swarm as usize);
    let mut message = format!(
        "{started} agent{} launched · {started} worktrees · all running",
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

/// Feed lines from a member's PR lifecycle (F6): opened, changes requested, resolved,
/// merged and closed. A merge also asks the affected members to rebase (F6, F10).
pub(crate) fn record_pull_request_observation(
    application: &AppHandle,
    project: u32,
    thread: &ThreadItem,
    before: Option<&FollowThrough>,
    after: &FollowThrough,
) {
    let Some((swarm, agent, finished_item)) = member_context(application, project, thread) else {
        return;
    };
    let Some((kind, message)) =
        swarm::pull_request_event(before.map(step), step(after), &finished_item)
    else {
        return;
    };
    let event = SwarmFeedEvent::new(&clock_timestamp(), &format!("a{agent}"), kind, message);
    report(post_event(application, project, swarm, event));
    if kind == SwarmEventKind::Merged {
        let application = application.clone();
        let (merged, number) = (thread.id, after.number);
        std::thread::spawn(move || {
            report(crate::swarm_rebase::notify_affected(
                &application,
                project,
                swarm,
                merged,
                number,
            ));
            report(announce_finished(&application, project, swarm));
        });
    }
}

fn step(watch: &FollowThrough) -> PullRequestStep<'_> {
    PullRequestStep {
        number: watch.number,
        stage: watch.stage.as_str(),
        worktree_deleted: watch.worktree_deleted,
    }
}

/// The member's swarm, agent number, and what merging it finishes.
fn member_context(
    application: &AppHandle,
    project: u32,
    thread: &ThreadItem,
) -> Option<(u32, u32, String)> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()?;
    let (swarm, agent) = thread_swarm(store.project(project)?, thread.id)?;
    let finished = match &thread.issue {
        Some(issue) if swarm.close_issues => format!("issue #{} closed", issue.number),
        Some(issue) => format!("issue #{} left open", issue.number),
        None => "task done".into(),
    };
    Some((swarm.id, agent, finished))
}

fn announce_finished(application: &AppHandle, project: u32, swarm: u32) -> Result<(), String> {
    let (finished, count, title) = {
        let store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let Some(item) = store.project(project) else {
            return Ok(());
        };
        let Some(record) = item.swarms.iter().find(|record| record.id == swarm) else {
            return Ok(());
        };
        let count = live_members(item, record).count();
        (swarm_finished(item, record), count, record.title())
    };
    if !finished {
        return Ok(());
    }
    let message = format!("Swarm finished · all {count} PRs merged");
    let event = SwarmFeedEvent::new(
        &clock_timestamp(),
        "swarm",
        SwarmEventKind::Finished,
        message,
    );
    post_event(application, project, swarm, event)?;
    let mut layout = lock(
        application.state::<MainLayout>().inner(),
        "main-window state",
    )?;
    layout.show_toast(format!("{title} finished · {count} PRs merged"), None);
    crate::layout::publish(application, &layout.snapshot(), None)
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
                format!("{headline} · worktree kept"),
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
