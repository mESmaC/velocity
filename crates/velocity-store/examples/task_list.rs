//! Headless F8 walkthrough using an isolated state.json. No provider or desktop is used.
use std::{error::Error, path::Path};
use velocity_core::{layout_state::LayoutState, layout_view::LayoutAction};
use velocity_store::{
    PersistedState, ProjectItem, ReviewStage, StateStore, TaskAttribution, TaskState, ThreadItem,
    ThreadState,
};

fn main() -> Result<(), Box<dyn Error>> {
    let directory = std::env::temp_dir().join(format!("velocity-task-list-{}", std::process::id()));
    std::fs::create_dir_all(&directory)?;
    seed(&directory)?;
    let mut store = StateStore::load_or_default(&directory)?;
    let mut layout = LayoutState::default();
    store.restore_project_layout(&mut layout);
    store.dispatch_project_layout(&mut layout, LayoutAction::TaskText("draft task.".into()))?;
    store.dispatch_project_layout(&mut layout, LayoutAction::SubmitTask)?;
    show(&store, &mut layout);
    store.update_project::<_, velocity_store::projects::ProjectError>(1, |project| {
        project.spec_locked = true;
        for task in &mut project.tasks {
            task.state = TaskState::Todo;
        }
        Ok(())
    })?;
    store.refresh_layout(&mut layout);
    store.dispatch_project_layout(
        &mut layout,
        LayoutAction::ComposerText("/task ship the list".into()),
    )?;
    store.dispatch_project_layout(&mut layout, LayoutAction::SubmitComposer)?;
    show(&store, &mut layout);
    store.update_project::<_, velocity_store::projects::ProjectError>(1, |project| {
        let mut thread = ThreadItem::new(7, "Ship Task List".into());
        thread.state = ThreadState::Review;
        thread.pull_request_number = Some(42);
        thread.review_stage = Some(ReviewStage::Review);
        project.threads.push(thread);
        project.tasks[1].state = TaskState::Now;
        project.tasks[1].assigned_thread_id = Some(7);
        project.tasks[1].added_by = TaskAttribution::Thread(7);
        Ok(())
    })?;
    show(&store, &mut layout);
    store.update_project::<_, velocity_store::projects::ProjectError>(1, |project| {
        project.threads[0].state = ThreadState::Merged;
        project.tasks[1].state = TaskState::Done;
        Ok(())
    })?;
    drop(store);
    let store = StateStore::load_or_default(&directory)?;
    show(&store, &mut layout);
    println!(
        "Saved and reloaded {}",
        directory.join("state.json").display()
    );
    Ok(())
}

fn seed(directory: &Path) -> Result<(), Box<dyn Error>> {
    let mut state = PersistedState::default();
    state.projects.push(ProjectItem::new(
        1,
        "Task List demo".into(),
        r"C:\Projects with spaces\demo".into(),
    ));
    std::fs::write(
        directory.join("state.json"),
        serde_json::to_vec_pretty(&state)?,
    )?;
    Ok(())
}

fn show(store: &StateStore, layout: &mut LayoutState) {
    store.refresh_layout(layout);
    println!("Task List {}", layout.snapshot().task_count_label);
    for task in layout.snapshot().tasks {
        println!(
            "  {}: {} · {} · {}",
            task.state,
            task.title,
            task.metadata,
            task.attribution.unwrap_or_default()
        );
    }
}
