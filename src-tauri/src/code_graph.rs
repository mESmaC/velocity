//! The code graph's per-project switch and its measurement in the usage modal (F19
//! "Measuring it"). The switch and the totals live with the project's graph in the app data
//! folder; `velocity-graph` owns them and works out the comparison.

use std::path::PathBuf;

use tauri::{AppHandle, Manager};
use velocity_graph::usage::{self, GraphComparison};

fn app_data(application: &AppHandle) -> Result<PathBuf, String> {
    application
        .path()
        .app_data_dir()
        .map_err(|error| format!("Could not find the application data folder: {error}"))
}

/// The usage modal's code graph section for a project: its switch and the comparison.
#[tauri::command]
#[specta::specta]
pub async fn get_code_graph(
    application: AppHandle,
    project_identifier: u32,
) -> Result<GraphComparison, String> {
    crate::layout::off_ui_thread(move || {
        let app_data = app_data(&application)?;
        let usage = usage::read(&app_data, &project_identifier.to_string());
        Ok(GraphComparison::from_usage(&usage))
    })
    .await
}

/// Turns the project's graph on or off for agents started from now on. Running agents keep
/// what they started with.
#[tauri::command]
#[specta::specta]
pub async fn set_code_graph(
    application: AppHandle,
    project_identifier: u32,
    enabled: bool,
) -> Result<GraphComparison, String> {
    crate::layout::off_ui_thread(move || {
        let app_data = app_data(&application)?;
        let usage = usage::set_enabled(&app_data, &project_identifier.to_string(), enabled)
            .map_err(|error| error.to_string())?;
        Ok(GraphComparison::from_usage(&usage))
    })
    .await
}

/// Clears both sides' totals, so a comparison starts fresh.
#[tauri::command]
#[specta::specta]
pub async fn reset_code_graph_usage(
    application: AppHandle,
    project_identifier: u32,
) -> Result<GraphComparison, String> {
    crate::layout::off_ui_thread(move || {
        let app_data = app_data(&application)?;
        let usage = usage::reset(&app_data, &project_identifier.to_string())
            .map_err(|error| error.to_string())?;
        Ok(GraphComparison::from_usage(&usage))
    })
    .await
}
