//! Headless §2.2 entry point: clock-driven supervision of a saved thread.
use velocity_store::{AgentActivity, Settings, ThreadItem, ThreadState};
use velocity_threads::agent_activity::{Action, decide};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut thread = ThreadItem::new(1, "Supervision demo".into());
    let mut settings = Settings::default().agents;
    thread.activity = AgentActivity::Thinking;
    thread.supervision.started = 100;
    thread.supervision.last_update = 100;
    for (time, expected) in [(399, Action::Nothing), (400, Action::Nudge)] {
        let action = decide(&thread, &settings, time, true, false);
        println!("{time}: {action:?}");
        if action != expected {
            return Err("Unexpected supervision action".into());
        }
    }
    thread.supervision.nudged = true;
    thread.supervision.last_update = 400;
    println!("700: {:?}", decide(&thread, &settings, 700, true, false));
    settings.max_turns = Some(2);
    thread.supervision.turns = 2;
    println!(
        "turn cap: {:?}",
        decide(&thread, &settings, 700, true, false)
    );
    thread.state = ThreadState::Error;
    thread.supervision.unavailable_since = Some(700);
    println!(
        "grace expired: {:?}",
        decide(&thread, &settings, 1300, false, false)
    );
    thread.pull_request_number = Some(4);
    println!(
        "open PR: {:?}",
        decide(&thread, &settings, 1300, false, false)
    );
    let saved = serde_json::to_vec(&thread)?;
    let restored: ThreadItem = serde_json::from_slice(&saved)?;
    if restored.supervision != thread.supervision {
        return Err("Checkpoint lost".into());
    }
    println!("Supervision checkpoint survived save/load");
    persisted_release()?;
    Ok(())
}

fn persisted_release() -> Result<(), Box<dyn std::error::Error>> {
    use velocity_store::{
        PersistedState, ProjectItem, StateStore, TaskAttribution, TaskItem, TaskState,
    };
    let directory =
        std::env::temp_dir().join(format!("velocity-activity-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&directory)?;
    let mut state = PersistedState::default();
    let mut project =
        ProjectItem::new(1, "Activity driver".into(), directory.display().to_string());
    project.spec_locked = true;
    let mut thread = ThreadItem::new(1, "Retained work".into());
    thread.state = ThreadState::Error;
    thread.branch_name = "vel/retained".into();
    thread.task_ids.push(1);
    let mut task = TaskItem::new(
        1,
        "Retained work".into(),
        TaskState::Now,
        TaskAttribution::User,
    );
    task.assigned_thread_id = Some(1);
    project.tasks.push(task);
    project.threads.push(thread);
    state.projects.push(project);
    std::fs::write(directory.join("state.json"), serde_json::to_vec(&state)?)?;
    let channel = velocity_airc::Channel::open(&directory, "1")?;
    channel.join("thread-1", "retained")?;
    channel.claim_piece(
        "thread-1",
        "task:1",
        "Retained work",
        "vel/retained",
        vec![],
    )?;
    let mut store = StateStore::load_or_default(&directory)?;
    let thread = &store.project(1).ok_or("Missing project")?.threads[0];
    velocity_threads::agent_activity::release_claims(&channel, thread, Some("1234567"))?;
    store.release_agent_work(1, 1, Some("1234567".into()))?;
    let store = StateStore::load_or_default(&directory)?;
    let project = store.project(1).ok_or("Missing project")?;
    if !project.threads[0].supervision.released
        || project.tasks[0].assigned_thread_id.is_some()
        || project.tasks[0].state != TaskState::Todo
        || !channel.claims()?.is_empty()
    {
        return Err("Release was not durable".into());
    }
    println!(
        "Channel and store released ownership; failed thread and optional pushed work retained at {}",
        directory.display()
    );
    Ok(())
}
