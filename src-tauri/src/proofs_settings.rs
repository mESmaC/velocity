//! Settings → Proofs (SPEC.md F13): reads the current project's checkout for the page. The
//! stress-diff input count is saved with the rest of the settings through `set_settings`.
use std::path::PathBuf;

use tauri::State;
use velocity_core::proofs_settings::ProofsSettingsView;

use crate::state::ManagedStateStore;

#[tauri::command]
#[specta::specta]
pub async fn get_proofs_settings(
    store: State<'_, ManagedStateStore>,
    project_identifier: u32,
) -> Result<ProofsSettingsView, String> {
    let (repository, stress_diff_input_count) = {
        let store = store.lock().map_err(|error| error.to_string())?;
        let project = store
            .project(project_identifier)
            .ok_or("The project is no longer registered")?;
        (
            PathBuf::from(&project.repository_path),
            store.settings().proofs.stress_diff_input_count,
        )
    };
    // git ls-files and git grep run once per design proof: keep them off the async runtime.
    tauri::async_runtime::spawn_blocking(move || {
        velocity_proofs::proofs_settings(&repository, stress_diff_input_count)
            .map_err(|error| error.to_string())
    })
    .await
    .map_err(|error| error.to_string())?
}
