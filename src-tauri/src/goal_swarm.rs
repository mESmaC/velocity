//! Tauri commands and runtime edges for Goal swarms (`SPEC.md` §3.1, F16).
//!
//! Draws the Goal swarm dialog from the cached work pool, launches all N agents at once in
//! their `<nick>` worktrees, tops up PR duty when asked, posts goal swarm lifecycle lines,
//! and spawns the next round when `keep_going` is enabled.

use std::sync::Mutex;

use tauri::{AppHandle, Manager};
use tauri_specta::Event as SpectaEvent;
use velocity_airc::{Channel, Event};
use velocity_core::goal_swarm::{GoalEventKind, GoalFeedEvent, PoolCounts};
use velocity_core::layout_view::LayoutSnapshot;
use velocity_core::swarm::{self, PullRequestStep, SwarmEventKind};
use velocity_github::follow_through::FollowThrough;
use velocity_store::ThreadItem;
use velocity_store::duty::{StartDutyRequest, clock_timestamp};
use velocity_store::goal_swarm_dialog::{
    GoalMixRow, GoalPool, GoalSwarmChoices, GoalSwarmDialog, GoalSwarmDialogQuery,
};
use velocity_store::goal_swarms::{
    AddGoalAgentsRequest, GoalAgentSlot, LaunchGoalSwarmRequest, thread_goal_swarm,
};
use velocity_threads::goal_swarm::{KeepGoingOutcome, evaluate_keep_going, plan_goal_member};

use crate::layout::MainLayout;
use crate::state::ManagedStateStore;
use crate::threads::{lock, report};

/// The work pool as the dialog last read it, and the project it belongs to.
#[derive(Default)]
pub struct GoalSwarmPool(Mutex<Option<(u32, GoalPool)>>);

/// The Goal swarm dialog for the user's choices with one edit applied; asked again after
/// every edit, and after the pool is read.
#[tauri::command]
#[specta::specta]
pub fn get_goal_swarm_dialog(
    application: AppHandle,
    query: GoalSwarmDialogQuery,
) -> Result<GoalSwarmDialog, String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let active = store.active_project().map(|project| project.id);
    let pool = cached_pool(&application, active)?;
    store
        .goal_swarm_dialog(&query, &pool)
        .ok_or_else(|| "Select or create a project first".into())
}

/// Reads the active project's work pool for the pool line and the PR duty check. Issues
/// come from GitHub and sections from `SPEC.md`, so the read runs off the UI thread.
#[tauri::command]
#[specta::specta]
pub async fn read_goal_swarm_pool(application: AppHandle) -> Result<(), String> {
    let project = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .active_project()
    .cloned()
    .ok_or("Select or create a project first")?;
    let identifier = project.id;
    let read = tauri::async_runtime::spawn_blocking(move || {
        velocity_threads::work_pool::read_work_pool(&project)
    })
    .await
    .map_err(|error| error.to_string())?;
    let pool = match read {
        Ok(pool) => GoalPool::Read(PoolCounts::new(
            pool.available_tasks,
            pool.available_issues,
            pool.available_sections,
        )),
        Err(error) => GoalPool::Failed(format!("Could not read the work pool: {error}")),
    };
    *lock(&application.state::<GoalSwarmPool>().0, "the work pool")? = Some((identifier, pool));
    Ok(())
}

/// Starts all N goal swarm agents at once on one prompt and the chosen model mix (F16).
#[tauri::command]
#[specta::specta]
pub fn launch_goal_swarm(
    application: AppHandle,
    request: LaunchGoalSwarmRequest,
) -> Result<LayoutSnapshot, String> {
    if request.add_pr_duty {
        report(ensure_pr_duty(
            &application,
            request.project_identifier,
            &request.mix,
        ));
    }
    let (swarm_id, slots, mix_summary) = {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let (swarm_id, slots) = store
            .open_goal_swarm(&request, clock_timestamp())
            .map_err(|error| error.to_string())?;
        let summary = store
            .project(request.project_identifier)
            .and_then(|project| project.goal_swarms.iter().find(|item| item.id == swarm_id))
            .map(|record| record.mix_summary.clone())
            .unwrap_or_default();
        (swarm_id, slots, summary)
    };
    let (first_thread, started, refused) =
        spawn_goal_slots(&application, request.project_identifier, slots);
    if started > 0 {
        post_swarm_started_event(
            &application,
            request.project_identifier,
            started,
            &mix_summary,
        );
    }
    finish_goal_launch(
        &application,
        first_thread,
        started,
        &refused,
        Some(swarm_id),
    )
}

/// `+ Agents`: starts more agents on an existing goal swarm's prompt (F16).
#[tauri::command]
#[specta::specta]
pub fn add_goal_agents(
    application: AppHandle,
    request: AddGoalAgentsRequest,
) -> Result<LayoutSnapshot, String> {
    if request.add_pr_duty {
        report(ensure_pr_duty(
            &application,
            request.project_identifier,
            &request.mix,
        ));
    }
    let slots = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .add_goal_swarm_agents(&request, &clock_timestamp())
    .map_err(|error| error.to_string())?;
    let (first_thread, started, refused) =
        spawn_goal_slots(&application, request.project_identifier, slots);
    finish_goal_launch(&application, first_thread, started, &refused, None)
}

fn cached_pool(application: &AppHandle, project: Option<u32>) -> Result<GoalPool, String> {
    Ok(
        lock(&application.state::<GoalSwarmPool>().0, "the work pool")?
            .as_ref()
            .filter(|(saved, _)| Some(*saved) == project)
            .map_or(GoalPool::Reading, |(_, pool)| pool.clone()),
    )
}

fn ensure_pr_duty(
    application: &AppHandle,
    project_id: u32,
    mix: &[GoalMixRow],
) -> Result<(), String> {
    let agents: u32 = mix.iter().fold(0, |sum, row| sum.saturating_add(row.count));
    let query = GoalSwarmDialogQuery::with_choices(GoalSwarmChoices::new(agents, mix.to_vec()));
    let pool = cached_pool(application, Some(project_id))?;
    let shortfall = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .goal_swarm_dialog(&query, &pool)
    .map_or(0, |dialog| dialog.pr_duty_shortfall);
    if shortfall == 0 {
        return Ok(());
    }
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
                StartDutyRequest::pr_duty(u8::try_from(shortfall).unwrap_or(u8::MAX)),
                clock_timestamp(),
            )
            .map_err(|error| error.to_string())?
    };
    crate::layout::publish(application, &outcome.snapshot, None)?;
    crate::duty::DutyEventPublished::new(outcome.role, outcome.event)
        .emit(application)
        .map_err(|error| error.to_string())?;
    crate::reviews::wake(application);
    Ok(())
}

fn spawn_goal_slots(
    application: &AppHandle,
    project_id: u32,
    slots: Vec<GoalAgentSlot>,
) -> (Option<u32>, usize, Vec<String>) {
    let mut first_thread = None;
    let mut started = 0;
    let mut refused = Vec::new();
    for slot in slots {
        match spawn_goal_slot(application, project_id, &slot) {
            Ok(thread_id) => {
                if first_thread.is_none() {
                    first_thread = Some(thread_id);
                }
                started += 1;
            }
            Err(error) => refused.push(error),
        }
    }
    (first_thread, started, refused)
}

fn spawn_goal_slot(
    application: &AppHandle,
    project_id: u32,
    slot: &GoalAgentSlot,
) -> Result<u32, String> {
    let (plan, thread_id) = {
        let mut store = lock(
            application.state::<ManagedStateStore>().inner(),
            "persisted state",
        )?;
        let project = store
            .project(project_id)
            .cloned()
            .ok_or("The selected project no longer exists")?;
        let worktrees = store.worktree_context().preferences;
        let settings = store.settings();
        let plan = plan_goal_member(&project, &worktrees, &settings, slot)
            .map_err(|error| error.to_string())?;
        let thread_id = store
            .start_goal_member_thread(project_id, slot, plan.thread.clone())
            .map_err(|error| error.to_string())?;
        (plan, thread_id)
    };
    crate::threads::start_goal_member(application, plan, thread_id)?;
    Ok(thread_id)
}

fn post_swarm_started_event(
    application: &AppHandle,
    project_id: u32,
    started: usize,
    mix_summary: &str,
) {
    report(
        Event::goal_swarm_started(started, mix_summary)
            .map_err(|error| error.to_string())
            .and_then(|event| crate::airc_events::post_event(application, project_id, event))
            .map(|_| ()),
    );
    report(crate::airc_modal::refresh(application));
}

fn finish_goal_launch(
    application: &AppHandle,
    first_thread: Option<u32>,
    started: usize,
    refused: &[String],
    launched_swarm: Option<u32>,
) -> Result<LayoutSnapshot, String> {
    if started == 0
        && let Some(reason) = refused.first()
    {
        return Err(reason.clone());
    }
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let mut layout = lock(
        application.state::<MainLayout>().inner(),
        "main-window state",
    )?;
    store.refresh_layout(&mut layout);
    if launched_swarm.is_some() {
        layout.close_goal_dialog_after_launch(first_thread.map(|id| id as usize));
    }
    let plural = if started == 1 { "" } else { "s" };
    let toast = if launched_swarm.is_some() {
        format!(
            "{started} agent{plural} launched \u{00b7} {started} worktree{plural} \u{00b7} they're claiming work in AIRC"
        )
    } else {
        format!(
            "Added {started} agent{plural} \u{00b7} {started} worktree{plural} \u{00b7} claiming work in AIRC"
        )
    };
    layout.show_toast(toast, None);
    let snapshot = layout.snapshot();
    crate::layout::publish(application, &snapshot, None)?;
    Ok(snapshot)
}

/// Records PR lifecycle transitions on a goal swarm member and runs `keep_going` on merge.
pub(crate) fn record_pull_request_observation(
    application: &AppHandle,
    project: u32,
    thread: &ThreadItem,
    before: Option<&FollowThrough>,
    after: &FollowThrough,
) {
    let Some((swarm_id, nick, finished_item)) = member_pr_context(application, project, thread)
    else {
        return;
    };
    let Some((swarm_kind, detail)) =
        swarm::pull_request_event(before.map(step), step(after), &finished_item)
    else {
        return;
    };
    let Some(kind) = goal_pr_kind(swarm_kind) else {
        return;
    };
    let event = GoalFeedEvent::new(&clock_timestamp(), &nick, kind, format!("{nick} {detail}"));
    report(record_event_and_publish(
        application,
        project,
        swarm_id,
        event,
    ));
    if kind == GoalEventKind::Merged {
        let handle = application.clone();
        let merged_thread = thread.id;
        tauri::async_runtime::spawn(async move {
            report(run_keep_going(&handle, project, merged_thread).await);
        });
    }
}

async fn run_keep_going(
    application: &AppHandle,
    project_id: u32,
    merged_thread: u32,
) -> Result<(), String> {
    let handle = application.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || {
        let data = handle
            .path()
            .app_data_dir()
            .map_err(|error| error.to_string())?;
        let channel =
            Channel::open(&data, &project_id.to_string()).map_err(|error| error.to_string())?;
        let store = handle.state::<ManagedStateStore>();
        evaluate_keep_going(
            store.inner(),
            &channel,
            project_id,
            merged_thread,
            &clock_timestamp(),
            velocity_threads::work_pool::read_work_pool,
        )
        .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())??;
    apply_keep_going(application, project_id, outcome)
}

fn apply_keep_going(
    application: &AppHandle,
    project_id: u32,
    outcome: KeepGoingOutcome,
) -> Result<(), String> {
    match outcome {
        KeepGoingOutcome::Stopped => Ok(()),
        KeepGoingOutcome::LeftEmpty { left_entry, .. } => {
            if let Some(entry) = left_entry {
                crate::airc_events::AircEventPublished::new(project_id, *entry)
                    .emit(application)
                    .map_err(|error| error.to_string())?;
            }
            report(crate::airc_modal::refresh(application));
            publish_snapshot(application)
        }
        KeepGoingOutcome::NextRound(slot) => {
            let thread_id = spawn_goal_slot(application, project_id, &slot)?;
            crate::threads::refresh(application, (project_id, thread_id), false).map(|_| ())
        }
    }
}

fn member_pr_context(
    application: &AppHandle,
    project: u32,
    thread: &ThreadItem,
) -> Option<(u32, String, String)> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()?;
    let (swarm, nick) = thread_goal_swarm(store.project(project)?, thread.id)?;
    let finished = if let Some(issue) = &thread.issue {
        format!("issue #{} merged", issue.number)
    } else if !thread.task_ids.is_empty() {
        "task done".into()
    } else if thread.spec_section.is_some() {
        "spec item built".into()
    } else {
        "merged".into()
    };
    Some((swarm.id, nick, finished))
}

fn goal_pr_kind(kind: SwarmEventKind) -> Option<GoalEventKind> {
    match kind {
        SwarmEventKind::Opened => Some(GoalEventKind::Opened),
        SwarmEventKind::Changes => Some(GoalEventKind::Changes),
        SwarmEventKind::Merged => Some(GoalEventKind::Merged),
        SwarmEventKind::Failed => Some(GoalEventKind::Failed),
        _ => None,
    }
}

fn step(watch: &FollowThrough) -> PullRequestStep<'_> {
    PullRequestStep::new(watch.number, watch.stage.as_str(), watch.worktree_deleted)
}

/// Records a goal swarm member stopping on an error while keeping its worktree (§2.1, F5).
pub(crate) fn record_member_failed(application: &AppHandle, key: (u32, u32), headline: &str) {
    let context = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )
    .ok()
    .and_then(|store| {
        let project = store.project(key.0)?;
        let (swarm, nick) = thread_goal_swarm(project, key.1)?;
        Some((swarm.id, nick))
    });
    let Some((swarm_id, nick)) = context else {
        return;
    };
    let event = GoalFeedEvent::new(
        &clock_timestamp(),
        &nick,
        GoalEventKind::Failed,
        format!("{nick}: {headline} \u{00b7} worktree kept"),
    );
    report(record_event_and_publish(
        application,
        key.0,
        swarm_id,
        event,
    ));
}

fn record_event_and_publish(
    application: &AppHandle,
    project_id: u32,
    swarm_id: u32,
    event: GoalFeedEvent,
) -> Result<(), String> {
    let recorded = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?
    .record_goal_swarm_event(project_id, swarm_id, event)
    .map_err(|error| error.to_string())?;
    if recorded {
        publish_snapshot(application)?;
    }
    Ok(())
}

fn publish_snapshot(application: &AppHandle) -> Result<(), String> {
    let store = lock(
        application.state::<ManagedStateStore>().inner(),
        "persisted state",
    )?;
    let mut layout = lock(
        application.state::<MainLayout>().inner(),
        "main-window state",
    )?;
    store.refresh_follow_through_layout(&mut layout);
    let snapshot = layout.snapshot();
    crate::layout::publish(application, &snapshot, None)
}
