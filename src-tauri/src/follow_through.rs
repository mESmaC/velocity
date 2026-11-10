//! Native scheduling and edges for F6. Decisions and durable cursors live in the crates.
use std::path::Path;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use velocity_git::remove_merged_worktree;
use velocity_github::{PullRequestClient, PullRequestSnapshot, PullRequestState};
use velocity_store::{ProjectItem, ThreadItem, ThreadState};
use velocity_threads::DeliveryStatus;

use crate::{layout::MainLayout, state::ManagedStateStore};

type ThreadKey = (u32, u32);
type Deliver = fn(
    &AppHandle,
    ThreadKey,
    &velocity_github::follow_through::AgentFollowUp,
) -> Result<DeliveryStatus, String>;
type Stop = fn(&AppHandle, ThreadKey) -> Result<bool, String>;
type ToolSteps = fn(&AppHandle, ThreadKey) -> Option<velocity_core::thread_view_model::SessionWork>;

pub const PR_OPENED_EVENT: &str = "pr://opened";
pub const PR_CI_EVENT: &str = "pr://ci";
pub const PR_REVIEW_EVENT: &str = "pr://review";
pub const PR_MERGED_EVENT: &str = "pr://merged";
pub const PR_CLOSED_EVENT: &str = "pr://closed";

/// The thread runtime installs these edges once it can receive queued messages.
pub struct Hooks {
    pub deliver: Deliver,
    pub stop: Stop,
    pub tool_steps: ToolSteps,
}

impl Default for Hooks {
    fn default() -> Self {
        Self {
            deliver: |_, _, _| Err("No agent session is running for this thread".into()),
            stop: |_, _| Ok(false),
            tool_steps: |_, _| None,
        }
    }
}

impl Hooks {
    pub fn new(deliver: Deliver, stop: Stop, tool_steps: ToolSteps) -> Self {
        Self {
            deliver,
            stop,
            tool_steps,
        }
    }
}

#[derive(Clone, Debug, serde::Serialize, specta::Type)]
pub struct PullRequestLifecycleEvent {
    pub project_identifier: u32,
    pub thread_identifier: u32,
    pub number: u32,
    pub stage: String,
    pub worktree_deleted: bool,
}

pub fn initialize(application: &AppHandle, hooks: Hooks) {
    let application = application.clone();
    thread::spawn(move || {
        loop {
            if let Err(error) = cycle(&application, &hooks) {
                eprintln!("Velocity PR follow-through: {error}");
            }
            thread::sleep(Duration::from_secs(5));
        }
    });
}

fn cycle(application: &AppHandle, hooks: &Hooks) -> Result<(), String> {
    let projects = application
        .state::<ManagedStateStore>()
        .lock()
        .map_err(|error| error.to_string())?
        .state()
        .projects
        .clone();
    for project in projects {
        let threads: Vec<_> = project
            .threads
            .iter()
            .filter(|thread| {
                matches!(
                    thread.state,
                    ThreadState::Working
                        | ThreadState::Review
                        | ThreadState::Closed
                        | ThreadState::Error
                ) && !thread.branch_name.is_empty()
                    && thread.worktree_path.is_some()
            })
            .cloned()
            .collect();
        if threads.is_empty() {
            continue;
        }
        let client = match PullRequestClient::discover(Path::new(&project.repository_path)) {
            Ok(client) => client,
            Err(error) => {
                eprintln!("PR watcher for {}: {error}", project.name);
                continue;
            }
        };
        for thread in threads {
            if let Err(error) = observe(application, hooks, &client, &project, &thread) {
                eprintln!(
                    "PR watcher for {} / {}: {error}",
                    project.name, thread.title
                );
            }
        }
    }
    Ok(())
}

fn observe(
    application: &AppHandle,
    hooks: &Hooks,
    client: &PullRequestClient,
    project: &ProjectItem,
    thread: &ThreadItem,
) -> Result<(), String> {
    if project
        .threads
        .iter()
        .any(|other| other.id != thread.id && other.branch_name == thread.branch_name)
    {
        return Err("Multiple saved threads own this branch; keeping their worktrees".into());
    }
    let mut watch = application
        .state::<ManagedStateStore>()
        .lock()
        .map_err(|error| error.to_string())?
        .follow_through_observer(project.id, thread.id)
        .ok_or("The owning thread was removed before reading GitHub")?;
    let Some(snapshot) = owning_snapshot(client, thread, &watch)? else {
        return Ok(());
    };
    let key = (project.id, thread.id);
    let action = crate::threads::action_lock(application, key)?;
    let _action = action.blocking_lock_owned();
    verify_saved_owner(application, key, thread)?;
    watch.observe(&snapshot);
    if let Some(work) = (hooks.tool_steps)(application, key) {
        watch.observe_work(&work);
    }
    checkpoint(application, key, &snapshot, watch.clone())?;
    if snapshot.pull_request.state == PullRequestState::Merged {
        finish_merged(application, hooks, project, thread, &mut watch);
    } else {
        deliver_pending(application, hooks, key, &snapshot, &mut watch)?;
    }
    checkpoint(application, key, &snapshot, watch)
}

fn owning_snapshot(
    client: &PullRequestClient,
    thread: &ThreadItem,
    watch: &velocity_github::follow_through::FollowThrough,
) -> Result<Option<PullRequestSnapshot>, String> {
    let number = match thread.pull_request_number {
        Some(number) => number,
        None => match client
            .for_branch(&thread.branch_name)
            .map_err(|error| error.to_string())?
            .first()
        {
            Some(request) if watch.should_follow(request) && watch.belongs_to_thread(request) => {
                request.number
            }
            _ => return Ok(None),
        },
    };
    let snapshot = client.snapshot(number).map_err(|error| error.to_string())?;
    if !client.owns_branch(&snapshot.pull_request, &thread.branch_name) {
        return Err("The PR head does not match this thread's immutable branch".into());
    }
    Ok(watch
        .should_follow(&snapshot.pull_request)
        .then_some(snapshot))
}

fn verify_saved_owner(
    application: &AppHandle,
    key: ThreadKey,
    expected: &ThreadItem,
) -> Result<(), String> {
    let store = application.state::<ManagedStateStore>();
    let store = store.lock().map_err(|error| error.to_string())?;
    let current = store
        .project(key.0)
        .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
        .ok_or("The owning thread was removed while reading GitHub")?;
    if current.branch_name != expected.branch_name
        || current.worktree_path != expected.worktree_path
        || current.pull_request_number != expected.pull_request_number
    {
        return Err("Thread ownership changed while reading GitHub".into());
    }
    Ok(())
}

fn finish_merged(
    application: &AppHandle,
    hooks: &Hooks,
    project: &ProjectItem,
    thread: &ThreadItem,
    watch: &mut velocity_github::follow_through::FollowThrough,
) {
    watch.pending.clear();
    if watch.worktree_deleted {
        return;
    }
    let key = (project.id, thread.id);
    match (hooks.stop)(application, key).and_then(|stopped| {
        if stopped {
            cleanup(project, thread, application)
        } else {
            Err("Waiting for the agent session to stop before removing its worktree".into())
        }
    }) {
        Ok(()) => watch.cleaned_up(),
        Err(error) => watch.cleanup_error = Some(error),
    }
}

fn deliver_pending(
    application: &AppHandle,
    hooks: &Hooks,
    key: ThreadKey,
    snapshot: &PullRequestSnapshot,
    watch: &mut velocity_github::follow_through::FollowThrough,
) -> Result<(), String> {
    for pending in watch.pending.clone() {
        match (hooks.deliver)(application, key, &pending) {
            Ok(DeliveryStatus::Pending) => continue,
            Ok(DeliveryStatus::Delivered) => {}
            Err(_) => break,
        }
        watch.delivered(&pending.identifier);
        checkpoint(application, key, snapshot, watch.clone())?;
    }
    Ok(())
}

fn checkpoint(
    application: &AppHandle,
    key: ThreadKey,
    snapshot: &PullRequestSnapshot,
    watch: velocity_github::follow_through::FollowThrough,
) -> Result<(), String> {
    let store_state = application.state::<ManagedStateStore>();
    let mut store = store_state.lock().map_err(|error| error.to_string())?;
    let thread = store
        .state()
        .projects
        .iter()
        .find(|project| project.id == key.0)
        .and_then(|project| project.threads.iter().find(|thread| thread.id == key.1))
        .ok_or("The owning thread was removed while reading GitHub")?
        .clone();
    let before = thread.follow_through.clone();
    if !store
        .save_follow_through(key.0, key.1, snapshot, watch.clone())
        .map_err(|error| error.to_string())?
    {
        return Ok(());
    }
    let layout_state = application.state::<MainLayout>();
    let mut layout = layout_state.lock().map_err(|error| error.to_string())?;
    store.refresh_follow_through_layout(&mut layout);
    let state = layout.snapshot();
    crate::layout::publish(application, &state, None)?;
    drop(layout);
    drop(store);
    crate::airc_events::record_pull_request_observation(
        application,
        key.0,
        &thread,
        before.as_ref(),
        &watch,
        snapshot,
    );
    publish_lifecycle(
        application,
        key,
        &watch,
        before.is_none_or(|watch| watch.number == 0),
    )
}

fn publish_lifecycle(
    application: &AppHandle,
    key: ThreadKey,
    watch: &velocity_github::follow_through::FollowThrough,
    opened: bool,
) -> Result<(), String> {
    let event = match watch.stage.as_str() {
        "closed" => PR_CLOSED_EVENT,
        "merged" if watch.worktree_deleted => PR_MERGED_EVENT,
        "ci" => PR_CI_EVENT,
        _ => PR_REVIEW_EVENT,
    };
    let payload = PullRequestLifecycleEvent {
        project_identifier: key.0,
        thread_identifier: key.1,
        number: watch.number,
        stage: watch.stage.clone(),
        worktree_deleted: watch.worktree_deleted,
    };
    if opened {
        application
            .emit(PR_OPENED_EVENT, &payload)
            .map_err(|error| error.to_string())?;
    }
    application
        .emit(event, payload)
        .map_err(|error| error.to_string())
}

fn cleanup(
    project: &ProjectItem,
    thread: &ThreadItem,
    application: &AppHandle,
) -> Result<(), String> {
    let path = Path::new(
        thread
            .worktree_path
            .as_deref()
            .ok_or("The thread has no worktree path")?,
    );
    let delete_branch = application
        .state::<ManagedStateStore>()
        .lock()
        .map_err(|error| error.to_string())?
        .settings()
        .worktrees
        .delete_local_branch_on_merge;
    remove_merged_worktree(
        Path::new(&project.repository_path),
        path,
        &thread.branch_name,
        delete_branch,
    )
    .map_err(|error| error.to_string())
}
