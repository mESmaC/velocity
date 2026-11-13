//! F8 request ingestion runs in the same store owner as user task edits. Agents add, change
//! and delete tasks through queued AIRC requests, and read the list from the snapshot this
//! owner publishes after each pass.
use std::sync::Mutex;

use velocity_airc::{Channel, Entry, Event, TaskListRow, TaskTarget};
use velocity_store::task_list::{TaskError, TaskUpdate};
use velocity_store::{StateStore, TaskState, threads::ThreadError};

#[derive(Debug, thiserror::Error)]
pub enum AgentTaskError {
    #[error(transparent)]
    Channel(#[from] velocity_airc::Error),
    #[error(transparent)]
    Persistence(#[from] velocity_store::StoreError),
    #[error("The saved state's lock was poisoned")]
    StoreLock,
}

/// What one request did: the event for the feed and the result for the agent, or the
/// reason it was refused.
enum Outcome {
    Saved { event: Event, result: String },
    Refused(String),
}

pub fn ingest(
    store: &mut StateStore,
    project: u32,
    channel: &Channel,
) -> Result<bool, AgentTaskError> {
    let requests = channel.pending_tasks()?;
    let (answers, failure) = apply_all(store, project, requests);
    let changed = answer(channel, answers, task_rows(store, project))?;
    failure.map_or(Ok(changed), Err)
}

/// [`ingest`] for the app, which runs it every second for every project: the store lock
/// is held only while requests change the store, never while the channel files are read
/// or written (#322).
pub fn ingest_shared(
    store: &Mutex<StateStore>,
    project: u32,
    channel: &Channel,
) -> Result<bool, AgentTaskError> {
    let requests = channel.pending_tasks()?;
    let (answers, failure, rows) = {
        let mut store = store.lock().map_err(|_| AgentTaskError::StoreLock)?;
        let (answers, failure) = apply_all(&mut store, project, requests);
        (answers, failure, task_rows(&store, project))
    };
    let changed = answer(channel, answers, rows)?;
    failure.map_or(Ok(changed), Err)
}

/// Each request with what it did. Stops at the first failure to save, which is returned
/// beside the answers already earned; the failed request stays queued for the next pass.
fn apply_all(
    store: &mut StateStore,
    project: u32,
    requests: Vec<Entry>,
) -> (Vec<(Entry, Outcome)>, Option<AgentTaskError>) {
    let mut answers = Vec::with_capacity(requests.len());
    for request in requests {
        match apply(store, project, &request) {
            Ok(outcome) => answers.push((request, outcome)),
            Err(error) => return (answers, Some(error)),
        }
    }
    (answers, None)
}

/// Posts each change's event, then the list as it stands after every change, then the
/// answers: an agent that reads the list after its answer sees its change. Returns
/// whether anything was saved.
fn answer(
    channel: &Channel,
    answers: Vec<(Entry, Outcome)>,
    rows: Option<Vec<TaskListRow>>,
) -> Result<bool, AgentTaskError> {
    let mut changed = false;
    for (request, outcome) in &answers {
        let Outcome::Saved { event, .. } = outcome else {
            continue;
        };
        if request.kind == "task_request" {
            // An add's event names its task number, so replay after a crash
            // deduplicates it without hiding a second task of the same title.
            channel.post_event_once(event.clone())?;
        } else {
            channel.post_event(event.clone())?;
        }
        changed = true;
    }
    if let Some(rows) = rows {
        channel.publish_task_list(&rows)?;
    }
    for (request, outcome) in answers {
        let text = match outcome {
            Outcome::Saved { result, .. } => result,
            Outcome::Refused(reason) => reason,
        };
        channel.finish_task(&request, &text)?;
    }
    Ok(changed)
}

fn apply(store: &mut StateStore, project: u32, request: &Entry) -> Result<Outcome, AgentTaskError> {
    let Some(change) = request.task_change.clone() else {
        return add(store, project, request);
    };
    let outcome = if request.kind == "task_delete_request" {
        store.delete_task(project, change.task).map(|task| {
            (
                format!(
                    "removed task #{} from the Task List: {}",
                    task.id, task.text
                ),
                format!("deleted task #{}", task.id),
            )
        })
    } else {
        let mut update = TaskUpdate::new(change.task);
        update.title = change.title;
        update.state = change.state.map(|state| match state {
            TaskTarget::Todo => TaskState::Todo,
            TaskTarget::Done => TaskState::Done,
        });
        store.update_task(project, update).map(|task| {
            let (state, title) = (state_name(task.state), &task.text);
            (
                format!(
                    "changed task #{} on the Task List: {state} · {title}",
                    task.id
                ),
                format!("updated task #{}: {state} · {title}", task.id),
            )
        })
    };
    match outcome {
        Ok((event, result)) => Ok(Outcome::Saved {
            event: Event::task_changed(&request.nick, &event)?,
            result,
        }),
        Err(TaskError::Persistence(error)) => Err(error.into()),
        // Numbers are never reused, so a missing number that was handed out was deleted:
        // the replay of a delete saved just before a crash, or someone else's delete.
        Err(TaskError::UnknownTask(task))
            if request.kind == "task_delete_request"
                && store
                    .project(project)
                    .is_some_and(|item| task <= item.last_task_identifier) =>
        {
            Ok(Outcome::Refused(format!("task #{task} is already deleted")))
        }
        Err(error) => Ok(Outcome::Refused(format!("Task was not changed: {error}"))),
    }
}

fn add(store: &mut StateStore, project: u32, request: &Entry) -> Result<Outcome, AgentTaskError> {
    match store.add_agent_task(project, &request.member, request.sequence, &request.text) {
        Ok(task) => Ok(Outcome::Saved {
            event: Event::task_saved(&request.nick, task.id, &task.text)?,
            result: format!("saved task #{}", task.id),
        }),
        Err(ThreadError::Persistence(error)) => Err(error.into()),
        Err(error) => Ok(Outcome::Refused(format!("Task was not added: {error}"))),
    }
}

/// The saved list as `task_list` publishes it, whoever changed it (the user, an agent, a
/// merge).
fn task_rows(store: &StateStore, project: u32) -> Option<Vec<TaskListRow>> {
    let views = store.list_tasks(project).ok()?;
    let rows = views
        .into_iter()
        .filter_map(|view| {
            let mut row = TaskListRow::new(view.identifier.parse().ok()?, view.title, view.state);
            row.status = (!view.metadata.is_empty()).then_some(view.metadata);
            row.thread = view
                .thread_identifier
                .and_then(|thread| thread.parse().ok());
            row.added_by = view.attribution;
            Some(row)
        })
        .collect();
    Some(rows)
}

fn state_name(state: TaskState) -> &'static str {
    match state {
        TaskState::Draft => "draft",
        TaskState::Todo => "todo",
        TaskState::Now => "now",
        TaskState::Done => "done",
    }
}
