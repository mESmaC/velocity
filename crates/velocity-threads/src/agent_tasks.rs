//! F8 request ingestion runs in the same store owner as user task edits.
use velocity_airc::{Channel, Event};
use velocity_store::{StateStore, threads::ThreadError};

pub fn ingest(
    store: &mut StateStore,
    project: u32,
    channel: &Channel,
) -> Result<bool, Box<dyn std::error::Error>> {
    let mut changed = false;
    for request in channel.pending_tasks()? {
        match store.add_agent_task(project, &request.member, request.sequence, &request.text) {
            Ok(task) => {
                channel.post_event_once(Event::new(
                    "task",
                    &request.nick,
                    &format!("added task #{} · {}", task.id, task.text),
                ))?;
                channel.finish_task(&request, &format!("saved task #{}", task.id))?;
                changed = true;
            }
            Err(ThreadError::Persistence(error)) => return Err(error.into()),
            Err(error) => {
                channel.finish_task(&request, &format!("Task was not added: {error}"))?;
            }
        }
    }
    Ok(changed)
}
