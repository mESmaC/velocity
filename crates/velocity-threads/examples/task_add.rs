//! Exercise F8 through the real channel, store owner and saved-state reload.
use velocity_airc::Channel;
use velocity_store::threads::NewThread;
use velocity_store::{PersistedState, ProjectItem, StateStore, TaskAttribution};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let directory = std::env::args()
        .nth(1)
        .ok_or("usage: task_add <new scratch directory>")?;
    std::fs::create_dir(&directory)?;
    let mut state = PersistedState::default();
    state
        .projects
        .push(ProjectItem::new(1, "Demo".into(), directory.clone()));
    std::fs::write(
        std::path::Path::new(&directory).join("state.json"),
        serde_json::to_vec(&state)?,
    )?;
    let mut store = StateStore::load_or_default(&directory)?;
    let thread = store.add_thread(
        1,
        NewThread::new(
            "Author".into(),
            "vel/author".into(),
            directory.clone(),
            &store.settings(),
        ),
    )?;
    let channel = Channel::open(std::path::Path::new(&directory), "1")?;
    let member = format!("thread-{thread}");
    channel.join(&member, "author")?;
    let request = channel.request_task(&member, "Add the requested feature")?;
    velocity_threads::agent_tasks::ingest(&mut store, 1, &channel)?;
    let task = store.add_agent_task(1, &member, request, "Add the requested feature")?;
    if task.added_by != TaskAttribution::Thread(thread) {
        return Err("Attribution lost".into());
    }
    drop(store);
    let mut store = StateStore::load_or_default(&directory)?;
    velocity_threads::agent_tasks::ingest(&mut store, 1, &channel)?;
    let project = store.project(1).ok_or("Project lost")?;
    if project.tasks.len() != 1 || !channel.pending_tasks()?.is_empty() {
        return Err("Replay duplicated or lost the task".into());
    }
    println!("{}", serde_json::to_string_pretty(&project.tasks)?);
    println!("{}", serde_json::to_string_pretty(&channel.events(0)?)?);
    Ok(())
}
