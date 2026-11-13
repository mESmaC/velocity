//! F8 request ingestion runs in the same store owner as user task edits. Agents add, change
//! and delete tasks through queued AIRC requests, and read the list from the snapshot this
//! owner publishes after each pass.
use velocity_airc::{Channel, Entry, Event, TaskListRow, TaskTarget};
use velocity_store::task_list::{TaskError, TaskUpdate};
use velocity_store::{StateStore, TaskState, threads::ThreadError};

#[derive(Debug, thiserror::Error)]
pub enum AgentTaskError {
    #[error(transparent)]
    Channel(#[from] velocity_airc::Error),
    #[error(transparent)]
    Persistence(#[from] velocity_store::StoreError),
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
    let mut changed = false;
    for request in channel.pending_tasks()? {
        match apply(store, project, &request)? {
            Outcome::Saved { event, result } => {
                if request.kind == "task_request" {
                    // An add's event names its task number, so replay after a crash
                    // deduplicates it without hiding a second task of the same title.
                    channel.post_event_once(event)?;
                } else {
                    channel.post_event(event)?;
                }
                // The list goes out before the answer, so an agent that reads it after
                // its change saved sees that change.
                publish(store, project, channel)?;
                channel.finish_task(&request, &result)?;
                changed = true;
            }
            Outcome::Refused(reason) => channel.finish_task(&request, &reason)?,
        }
    }
    publish(store, project, channel)?;
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

/// Publish the saved list for `task_list`, whoever changed it (the user, an agent, a merge).
fn publish(store: &StateStore, project: u32, channel: &Channel) -> Result<(), AgentTaskError> {
    let Ok(views) = store.list_tasks(project) else {
        return Ok(());
    };
    let rows: Vec<TaskListRow> = views
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
    channel.publish_task_list(&rows)?;
    Ok(())
}

fn state_name(state: TaskState) -> &'static str {
    match state {
        TaskState::Draft => "draft",
        TaskState::Todo => "todo",
        TaskState::Now => "now",
        TaskState::Done => "done",
    }
}
