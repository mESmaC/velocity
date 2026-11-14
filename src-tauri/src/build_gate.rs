use tauri::{AppHandle, Manager};
use velocity_store::build_gate::{LaunchEligibility, LaunchRequest, launch_eligibility};

use crate::state::ManagedStateStore;

#[tauri::command]
#[specta::specta]
pub async fn get_launch_eligibility(
    application: AppHandle,
    request: LaunchRequest,
) -> Result<LaunchEligibility, String> {
    crate::layout::off_ui_thread(move || {
        let store = application.state::<ManagedStateStore>();
        let guard = store
            .lock()
            .map_err(|_| String::from("Persisted state store lock is poisoned"))?;
        Ok(launch_eligibility(guard.state(), request))
    })
    .await
}
