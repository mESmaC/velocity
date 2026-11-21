use crate::{
    layout::MainLayout,
    state::ManagedStateStore,
    threads::{lock, report},
};
use tauri::{AppHandle, Manager};
use velocity_core::layout_view::TaskView;
use velocity_store::TaskItem;

#[tauri::command]
#[specta::specta]
pub async fn list_tasks(
    application: AppHandle,
    project_identifier: u32,
) -> Result<Vec<TaskView>, String> {
    crate::layout::run_concurrently("list_tasks", move || {
        let store = application.state::<ManagedStateStore>();
        lock(&store, "Task List")?
            .list_tasks(project_identifier)
            .map_err(|error| error.to_string())
    })
    .await
}

#[tauri::command]
#[specta::specta]
pub async fn add_task(
    application: AppHandle,
    project_identifier: u32,
    title: String,
) -> Result<TaskItem, String> {
    crate::layout::off_ui_thread_named("add_task", move || {
        let (task, active_tasks, spec_locked) = {
            let managed = application.state::<ManagedStateStore>();
            let mut store = lock(&managed, "Task List")?;
            let task = store
                .add_task(project_identifier, title)
                .map_err(|error| error.to_string())?;
            let active_tasks = if store.active_project_identifier() == Some(project_identifier) {
                Some(
                    store
                        .list_tasks(project_identifier)
                        .map_err(|error| error.to_string())?,
                )
            } else {
                None
            };
            let spec_locked = store.active_project().map(|project| project.spec_locked);
            (task, active_tasks, spec_locked)
        };
        let snapshot = {
            let layout = application.state::<MainLayout>();
            let mut layout = lock(&layout, "main layout")?;
            if let Some(tasks) = active_tasks {
                layout.set_task_rows(tasks);
            }
            layout.set_project_spec_locked(spec_locked);
            layout.snapshot()
        };
        record_added(&application, project_identifier, &task);
        crate::layout::publish(&application, &snapshot, None)?;
        Ok(task)
    })
    .await
}

pub(crate) fn record_added(application: &AppHandle, project: u32, task: &TaskItem) {
    report(
        velocity_airc::Event::task_saved("you", task.id, &task.text)
            .map_err(|error| error.to_string())
            .and_then(|event| crate::airc_events::post_event(application, project, event)),
    );
}

#[tauri::command]
#[specta::specta]
pub async fn undo_task_event(
    application: AppHandle,
    project_identifier: u32,
    request: String,
) -> Result<(), String> {
    crate::layout::off_ui_thread_named("undo_task_event", move || {
        let request = request.parse().map_err(|_| "Invalid task event request")?;
        let (task, selected) = {
            let managed = application.state::<ManagedStateStore>();
            let mut store = lock(&managed, "Task List")?;
            let task = store
                .undo_task_request(project_identifier, request)
                .map_err(|error| error.to_string())?;
            let selected = store
                .project(project_identifier)
                .and_then(|project| project.active_thread_id)
                .unwrap_or(0);
            (task, selected)
        };
        report(
            velocity_airc::Event::task_changed(
                "you",
                &format!("undid a change to task #{}: {}", task.id, task.text),
            )
            .map_err(|error| error.to_string())
            .and_then(|event| {
                crate::airc_events::post_event(&application, project_identifier, event)
            }),
        );
        crate::threads::refresh(&application, (project_identifier, selected), false)?;
        crate::airc_modal::refresh(&application)
    })
    .await
}
