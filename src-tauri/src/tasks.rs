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
    crate::layout::off_ui_thread(move || {
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
    crate::layout::off_ui_thread(move || {
        let managed = application.state::<ManagedStateStore>();
        let mut store = lock(&managed, "Task List")?;
        let task = store
            .add_task(project_identifier, title)
            .map_err(|error| error.to_string())?;
        let layout = application.state::<MainLayout>();
        let mut layout = lock(&layout, "main layout")?;
        if store.active_project_identifier() == Some(project_identifier) {
            layout.set_task_rows(
                store
                    .list_tasks(project_identifier)
                    .map_err(|error| error.to_string())?,
            );
        }
        let snapshot = velocity_store::build_gate::layout_snapshot(store.state(), &mut layout);
        drop(layout);
        drop(store);
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
