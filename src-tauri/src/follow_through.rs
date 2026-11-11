//! Native scheduling and edges for F6. Decisions and durable cursors live in the crates.
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter, Manager};
use velocity_git::remove_merged_worktree;
use velocity_github::follow_through_poll::{
    ListedPullRequest, PollChoice, PollThread, SnapshotCache,
};
use velocity_github::{PullRequest, PullRequestClient, PullRequestSnapshot, PullRequestState};
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

/// SPEC.md F6 sets no latency for follow-through. 15 s keeps a review comment's delay short,
/// and one list query per project per pass stays well inside the shared GitHub budget.
const PASS_INTERVAL: Duration = Duration::from_secs(15);

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
        let mut clients = Clients::new();
        let mut cache = SnapshotCache::default();
        loop {
            if let Err(error) = cycle(&application, &hooks, &mut clients, &mut cache) {
                eprintln!("Velocity PR follow-through: {error}");
            }
            thread::sleep(PASS_INTERVAL);
        }
    });
}

/// Each project's GitHub client, with the checkout it was read from. Reading the repository's
/// identity is a GitHub request, so it is kept between passes and read again only when the
/// checkout or its `origin` changes, or the identity is half an hour old (#229).
type Clients = BTreeMap<u32, (String, PullRequestClient)>;

/// One project's GitHub reads for this pass, shared by all of its threads.
struct ProjectPass<'a> {
    project: &'a ProjectItem,
    client: &'a PullRequestClient,
    listing: &'a [ListedPullRequest],
}

fn cycle(
    application: &AppHandle,
    hooks: &Hooks,
    clients: &mut Clients,
    cache: &mut SnapshotCache,
) -> Result<(), String> {
    let projects = application
        .state::<ManagedStateStore>()
        .lock()
        .map_err(|error| error.to_string())?
        .state()
        .projects
        .clone();
    clients.retain(|identifier, _| projects.iter().any(|project| project.id == *identifier));
    let mut followed = BTreeSet::new();
    for project in &projects {
        let threads = followed_threads(project);
        followed.extend(threads.iter().map(|thread| (project.id, thread.id)));
        if threads.is_empty() {
            continue;
        }
        let Some(client) = project_client(clients, project) else {
            continue;
        };
        // One request per project however many threads it has (#217).
        let listing = match client.follow_through_list() {
            Ok(listing) => listing,
            Err(error) => {
                eprintln!("PR watcher for {}: {error}", project.name);
                continue;
            }
        };
        let pass = ProjectPass {
            project,
            client,
            listing: &listing,
        };
        for thread in threads {
            if let Err(error) = observe(application, hooks, &pass, cache, &thread) {
                eprintln!(
                    "PR watcher for {} / {}: {error}",
                    project.name, thread.title
                );
            }
        }
    }
    cache.retain(&followed);
    Ok(())
}

fn followed_threads(project: &ProjectItem) -> Vec<ThreadItem> {
    project
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
        .collect()
}

fn project_client<'a>(
    clients: &'a mut Clients,
    project: &ProjectItem,
) -> Option<&'a PullRequestClient> {
    let current = clients.get(&project.id).is_some_and(|(checkout, client)| {
        *checkout == project.repository_path
            && client.is_current(Path::new(&project.repository_path))
    });
    if !current {
        match PullRequestClient::discover(Path::new(&project.repository_path)) {
            Ok(client) => {
                clients.insert(project.id, (project.repository_path.clone(), client));
            }
            Err(error) => {
                eprintln!("PR watcher for {}: {error}", project.name);
                return None;
            }
        }
    }
    clients.get(&project.id).map(|(_, client)| client)
}

fn observe(
    application: &AppHandle,
    hooks: &Hooks,
    pass: &ProjectPass<'_>,
    cache: &mut SnapshotCache,
    thread: &ThreadItem,
) -> Result<(), String> {
    let project = pass.project;
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
    let key = (project.id, thread.id);
    let Some(snapshot) = owning_snapshot(pass, cache, key, thread, &watch)? else {
        return Ok(());
    };
    let action = crate::threads::action_lock(application, key)?;
    let _action = action.blocking_lock_owned();
    verify_saved_owner(application, key, thread)?;
    watch.observe(&snapshot);
    if let Some(work) = (hooks.tool_steps)(application, key) {
        watch.observe_work(&work);
    }
    if let Some(worktree) = thread.worktree_path.as_deref() {
        crate::proof_card::observe(
            application,
            &crate::proof_card::ProofTarget {
                key,
                worktree,
                base_branch: &snapshot.pull_request.base_ref_name,
            },
            &mut watch,
        );
    }
    checkpoint(application, key, &snapshot, watch.clone())?;
    if snapshot.pull_request.state == PullRequestState::Merged {
        finish_merged(application, hooks, project, thread, &mut watch);
    } else {
        deliver_pending(application, hooks, key, &snapshot, &mut watch)?;
    }
    checkpoint(application, key, &snapshot, watch)
}

/// The thread's PR as it was last read. It is read again in full only when the project's
/// list shows it changed, or when it has not been read since Velocity started.
fn owning_snapshot(
    pass: &ProjectPass<'_>,
    cache: &mut SnapshotCache,
    key: ThreadKey,
    thread: &ThreadItem,
    watch: &velocity_github::follow_through::FollowThrough,
) -> Result<Option<PullRequestSnapshot>, String> {
    let client = pass.client;
    let poll = PollThread {
        key,
        saved_number: thread.pull_request_number,
        watch,
    };
    let owns = |request: &PullRequest| client.owns_branch(request, &thread.branch_name);
    let (number, fingerprint) = match cache.choose(&poll, pass.listing, owns) {
        PollChoice::Skip => return Ok(None),
        PollChoice::Reuse => return Ok(cache.snapshot(key).cloned()),
        PollChoice::Refresh {
            number,
            fingerprint,
        } => (number, fingerprint),
        PollChoice::Search => {
            let requests = client
                .for_branch(&thread.branch_name)
                .map_err(|error| error.to_string())?;
            cache.searched(key);
            match requests.first() {
                Some(request)
                    if watch.should_follow(request) && watch.belongs_to_thread(request) =>
                {
                    (request.number, None)
                }
                _ => return Ok(None),
            }
        }
    };
    let snapshot = client.snapshot(number).map_err(|error| error.to_string())?;
    if !client.owns_branch(&snapshot.pull_request, &thread.branch_name) {
        // Not read again until the list shows the PR changed (#229).
        cache.mismatched(key, number, fingerprint);
        return Err("The PR head does not match this thread's immutable branch".into());
    }
    let followed = watch.should_follow(&snapshot.pull_request);
    cache.store(key, fingerprint, snapshot.clone());
    Ok(followed.then_some(snapshot))
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
            cleanup(project, thread, application)?;
            crate::airc_claims::release(application, key)
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
    crate::swarm::record_pull_request_observation(
        application,
        key.0,
        &thread,
        before.as_ref(),
        &watch,
    );
    crate::goal_swarm::record_pull_request_observation(
        application,
        key.0,
        &thread,
        before.as_ref(),
        &watch,
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
