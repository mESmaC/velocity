use tauri::{AppHandle, Manager};
use velocity_graph::token_usage::{self, Period, Project, Report};

use crate::state::ManagedStateStore;

#[tauri::command]
#[specta::specta]
pub async fn get_token_usage(
    application: AppHandle,
    period: Period,
    project_identifier: Option<u32>,
    thread_identifier: Option<u32>,
) -> Result<Report, String> {
    let thread = project_identifier.zip(thread_identifier);
    let store = application.state::<ManagedStateStore>();
    let mut projects = {
        let store = store.lock().map_err(|error| error.to_string())?;
        store
            .state()
            .projects
            .iter()
            .filter(|project| thread.is_none_or(|(id, _)| id == project.id))
            .map(|project| {
                (
                    project.id,
                    Project {
                        id: project.id,
                        name: project.name.clone(),
                        threads: project
                            .threads
                            .iter()
                            .map(|thread| (thread.id, thread.title.clone()))
                            .collect(),
                    },
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    if thread.is_none() {
        projects.entry(0).or_insert_with(|| Project {
            id: 0,
            name: "Machine duty".to_owned(),
            threads: Default::default(),
        });
    }
    let data = application
        .path()
        .app_data_dir()
        .map_err(|error| error.to_string())?;
    crate::layout::off_ui_thread(move || {
        token_usage::report(&data, projects, period, thread).map_err(|error| error.to_string())
    })
    .await
}
